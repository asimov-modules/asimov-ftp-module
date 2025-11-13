// This is free and unencumbered software released into the public domain.

#![forbid(unsafe_code)]

use std::{num::ParseIntError, time::UNIX_EPOCH};

use asimov_module::{
    prelude::{
        boxed::Box,
        collections::BTreeMap,
        string::{String, ToString as _},
        vec::Vec,
    },
    tracing,
};
use iri_string::format::ToDedicatedString;
use know::{classes::FileMetadata, datatypes::DateTime};

#[derive(Debug, thiserror::Error)]
pub enum UrlError {
    #[error("invalid URL: {0}")]
    Parse(#[from] iri_string::validate::Error),

    #[error("only FTP and FTPS URLs are supported")]
    UnsupportedScheme,

    #[error("invalid port number: {0}")]
    InvalidPort(#[from] ParseIntError),

    #[error("URL needs to have a host")]
    NoHost,
}

/// (scheme, host, port, (user password))
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TargetHost(pub String, pub String, pub u16, pub (String, String));

pub fn group_targets(
    urls: &[impl AsRef<str>],
) -> Result<BTreeMap<TargetHost, (String, Vec<String>)>, UrlError> {
    let mut connections: BTreeMap<TargetHost, (String, Vec<String>)> = BTreeMap::new();

    for url in urls {
        let url = url.as_ref();
        let iri = iri_string::types::IriReferenceStr::new(url)?;

        let scheme = iri.scheme_str().unwrap_or("ftp").into();

        if scheme != "ftp" && scheme != "ftps" {
            return Err(UrlError::UnsupportedScheme);
        }

        let auth = iri.authority_components().ok_or(UrlError::NoHost)?;
        let port = auth.port().map(|p| p.parse()).transpose()?.unwrap_or(21);
        let host = auth.host().into();
        let (name, password) = auth
            .userinfo()
            .and_then(|s| s.split_once(':'))
            .unwrap_or(("anonymous", "guest"));
        let user = (name.into(), password.into());

        let path = iri.path_str();

        let key = TargetHost(scheme, host, port, user);
        let entry = connections.entry(key).or_insert((url.into(), Vec::new()));
        entry.1.push(path.into());
    }

    Ok(connections)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("URL validation error: {0}")]
    Url(#[from] iri_string::validate::Error),

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

#[tracing::instrument(skip(ftp))]
pub fn list(
    ftp: &mut suppaftp::RustlsFtpStream,
    path: &str,
    host: &str,
) -> Result<Vec<FileMetadata>, crate::Error> {
    let host_url = iri_string::types::IriAbsoluteStr::new(host)?;

    let dir_url = iri_string::types::IriRelativeStr::new(path)?.resolve_against(host_url);

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
                        let full_path = std::path::Path::new(&path)
                            .join(&file)
                            .to_string_lossy()
                            .to_string();
                        let child_path = iri_string::types::IriRelativeStr::new(&full_path)?
                            .resolve_against(host_url)
                            .and_normalize()
                            .to_dedicated_string()
                            .to_string();

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
                            "OS.unix=symlink" => {
                                let link = iri_string::types::IriRelativeStr::new(&file)?;
                                let result = link.resolve_against(host_url);
                                let target =
                                    result.and_normalize().to_dedicated_string().to_string();
                                know::classes::FileType::Symlink { target }
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
                id: Some(dir_url.to_dedicated_string().to_string()),
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
            if err.status == suppaftp::Status::BadCommand => {},
        Err(err) => Err(err)?,
    }

    match ftp
        .list(Some(path))
        .inspect_err(|err| tracing::error!("LIST command failed: {err}"))
    {
        Ok(list_output) => {
            tracing::debug!(?list_output);

            // cases:
            // 1. `ls` target is a directory:
            //    - multiple files are returned
            //    - files have all kind of types: f/d/s
            //    - names generally don't match the input path
            //      BUT: there could be a file inside the directory with the same name
            //           if it's the only file, then it would look like we `ls`'ed a file
            // 2. `ls` target is a file:
            //    - only one file is returned
            //    - file type is f
            //    - name matches the input `path`

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
                    let target = file
                        .symlink()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let link = iri_string::types::IriRelativeStr::new(&target)?;
                    let result = link.resolve_against(host_url);
                    let target = result.and_normalize().to_dedicated_string().to_string();
                    know::classes::FileType::Symlink { target }
                } else {
                    return Err(Error::Other("unknown file type"));
                };

                let name = iri_string::types::IriRelativeStr::new(name)?;
                let id = Some(
                    name.resolve_against(host_url)
                        .and_normalize()
                        .to_dedicated_string()
                        .to_string(),
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
        Err(suppaftp::FtpError::UnexpectedResponse(err))
            if err.status == suppaftp::Status::BadCommand => {},
        Err(err) => Err(err)?,
    }

    Err(Error::Other(
        "server supports neither LIST nor MLSD command",
    ))
}

fn parse_facts(line: &str) -> Result<(BTreeMap<String, String>, String), crate::Error> {
    let parts: Vec<&str> = line.split(';').collect();
    if parts.is_empty() {
        return Err(Error::MlstParse("invalid line".into()));
    }
    let mut map = BTreeMap::new();
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
