// This is free and unencumbered software released into the public domain.

#![forbid(unsafe_code)]

use std::time::UNIX_EPOCH;

use asimov_module::{
    prelude::{
        boxed::Box,
        collections::HashMap,
        string::{String, ToString as _},
        vec::Vec,
    },
    tracing,
};
use know::{classes::FileMetadata, datatypes::DateTime};
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),

    #[error("FTP: {0}")]
    Ftp(#[from] suppaftp::FtpError),

    #[error("error while parsing MLST command output: {0}")]
    MlstParse(#[from] Box<dyn core::error::Error>),

    #[error("error while parsing LIST command output: {0}")]
    ListParse(#[from] suppaftp::list::ParseError),

    #[error("error while handling timestamp in response: {0}")]
    Timestamp(#[from] jiff::Error),

    #[error("unexpected error: {0}")]
    Other(&'static str),
}

pub fn list(ftp: &mut suppaftp::FtpStream, url: &str) -> Result<Vec<FileMetadata>, crate::Error> {
    let parsed = Url::parse(url)?;
    let path = parsed.path().strip_prefix('/').unwrap();

    match ftp
        .mlst(Some(path))
        .inspect_err(|err| tracing::error!("MLST command failed: {err}"))
    {
        Ok(mlst_output) => {
            tracing::debug!(?mlst_output);

            let (facts, mlst_path) = parse_facts(&mlst_output)?;

            let type_ = facts.get("type").ok_or(Error::Other("no file type"))?;
            let modification_date = facts.get("modify").map(|s| parse_datetime(s)).transpose()?;
            let size = facts.get("size").and_then(|s| s.parse().ok());
            let owner = facts.get("UNIX.ownername").cloned();
            let group = facts.get("UNIX.groupname").cloned();

            let mut children_metadata = Vec::new();

            let filetype = match type_.as_str() {
                "file" => know::classes::FileType::Regular,
                "dir" => {
                    let mlsd_lines = ftp.mlsd(Some(path))?;

                    let mut children = Vec::new();
                    for line in mlsd_lines {
                        let (facts, file) = parse_facts(&line)?;
                        if file == "." || file == ".." {
                            continue;
                        }
                        let child_path = std::path::Path::new(&url)
                            .join(&file)
                            .to_string_lossy()
                            .into_owned();

                        children.push(child_path.clone());

                        let type_ = facts.get("type").ok_or(Error::Other("no file type"))?;
                        let modification_date =
                            facts.get("modify").map(|s| parse_datetime(s)).transpose()?;
                        let size = facts.get("size").and_then(|s| s.parse().ok());
                        let owner = facts.get("UNIX.ownername").cloned();
                        let group = facts.get("UNIX.groupname").cloned();
                        let filetype = match type_.as_str() {
                            "file" => know::classes::FileType::Regular,
                            "dir" => know::classes::FileType::Directory {
                                // TODO: is there a way to symbolize that we haven't checked the contents of
                                // the directory? maybe the children field should be made into a
                                // `Option<Vec<String>>` where `None` means contents are unknown, and
                                // `Some(vec![])` means directory is empty.
                                children: Vec::new(),
                            },
                            "OS.unix=symlink" => know::classes::FileType::Symlink {
                                target: child_path.clone(),
                            },
                            _ => {
                                return Err(Error::MlstParse(
                                    format!("unknown file type: {}", type_).into(),
                                ));
                            },
                        };

                        children_metadata.push(FileMetadata {
                            id: Some(child_path),
                            modification_date,
                            size,
                            owner,
                            group,
                            filetype,
                        })
                    }

                    know::classes::FileType::Directory { children }
                },
                "OS.unix=symlink" => know::classes::FileType::Symlink { target: mlst_path },
                _ => {
                    return Err(Error::MlstParse(
                        format!("unknown file type: {}", type_).into(),
                    ));
                },
            };

            let metadata = FileMetadata {
                id: Some(url.into()),
                modification_date,
                size,
                owner,
                group,
                filetype,
            };

            let mut files = vec![metadata];
            files.extend(children_metadata);

            return Ok(files);
        },
        Err(suppaftp::FtpError::UnexpectedResponse(err))
            if err.status == suppaftp::Status::BadCommand =>
        {
            // proceed to trying `list` instead
        },
        Err(err) => Err(err)?,
    }

    match ftp
        .list(Some(path))
        .inspect_err(|err| tracing::error!("LIST command failed: {err}"))
    {
        Ok(list_output) => {
            tracing::debug!(?list_output);

            let mut files = Vec::new();
            for line in list_output {
                let file: suppaftp::list::File = line.parse()?;

                let name = file.name();
                let modification_date = Some(DateTime::from(
                    file.modified()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64,
                ));

                let filetype = if file.is_file() {
                    know::classes::FileType::Regular
                } else if file.is_directory() {
                    // TODO: is there a way to symbolize that we haven't checked the contents of
                    // the directory? maybe the children field should be made into a
                    // `Option<Vec<String>>` where `None` means contents are unknown, and
                    // `Some(vec![])` means directory is empty.
                    know::classes::FileType::Directory {
                        children: Vec::new(),
                    }
                } else if file.is_symlink() {
                    know::classes::FileType::Symlink {
                        target: file
                            .symlink()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    }
                } else {
                    return Err(Error::Other("unknown file type"));
                };

                let id = Some(
                    std::path::Path::new(url)
                        .join(name)
                        .to_string_lossy()
                        .into_owned(),
                );

                let metadata = FileMetadata {
                    id,
                    modification_date,
                    size: Some(file.size()),
                    owner: file.uid().map(|uid| uid.to_string()),
                    group: file.gid().map(|gid| gid.to_string()),
                    filetype,
                };

                files.push(metadata);
            }
            return Ok(files);
        },
        Err(err) => Err(err)?,
    }

    Err(Error::Other(
        "server supports neither MLSD nor LIST command",
    ))
}

fn parse_facts(line: &str) -> Result<(HashMap<String, String>, String), crate::Error> {
    let parts: Vec<&str> = line.split(';').collect();
    if parts.is_empty() {
        return Err(Error::MlstParse("invalid line".into()));
    }
    let mut map = HashMap::new();
    for part in &parts[..parts.len() - 1] {
        if let Some(eq) = part.find('=') {
            let key = part[..eq].trim().to_string().to_lowercase();
            let value = part[eq + 1..].trim().to_string();
            map.insert(key, value);
        }
    }
    let name = parts.last().unwrap().trim().to_string();
    Ok((map, name))
}

fn parse_datetime(modify: &str) -> Result<know::datatypes::DateTime, crate::Error> {
    Ok(jiff::civil::DateTime::strptime("%Y%m%d%H%M%S", modify)?
        .to_zoned(jiff::tz::TimeZone::UTC)?
        .timestamp()
        .as_second()
        .into())
}
