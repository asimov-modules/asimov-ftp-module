// This is free and unencumbered software released into the public domain.

#[cfg(not(feature = "std"))]
compile_error!("asimov-ftp-cataloger requires the 'std' feature");

use asimov_module::SysexitsError::{self, *};
use clap::Parser;
use clientele::StandardOptions;
use know::{
    classes::{FileMetadata, FileType},
    traits::ToJsonLd,
};
use std::error::Error;
use suppaftp::{FtpStream, Mode};
use url::Url;

/// asimov-ftp-cataloger
#[derive(Debug, Parser)]
#[command(arg_required_else_help = true)]
struct Options {
    #[clap(flatten)]
    flags: StandardOptions,

    /// The output format.
    #[arg(value_name = "FORMAT", short = 'o', long, default_value_t, value_enum)]
    output: OutputFormat,

    /// The `ftp:` or `ftps:` URLs to catalog
    urls: Vec<String>,
}

#[derive(Clone, Debug, Default, clap::ValueEnum)]
enum OutputFormat {
    #[default]
    Cli,
    Jsonl,
    Jsonld,
    Json,
}

fn main() -> Result<SysexitsError, Box<dyn Error>> {
    // Load environment variables from `.env`:
    asimov_module::dotenv().ok();

    // Expand wildcards and @argfiles:
    let args = asimov_module::args_os()?;

    // Parse command-line options:
    let options = Options::parse_from(args);

    // Handle the `--version` flag:
    if options.flags.version {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(EX_OK);
    }

    // Handle the `--license` flag:
    if options.flags.license {
        print!("{}", include_str!("../../UNLICENSE"));
        return Ok(EX_OK);
    }

    // Configure logging & tracing:
    #[cfg(feature = "tracing")]
    asimov_module::init_tracing_subscriber(&options.flags).expect("failed to initialize logging");

    for url in options.urls {
        let parsed = Url::parse(&url)?;
        let scheme = parsed.scheme();
        if scheme != "ftp" && scheme != "ftps" {
            return Err("only FTP and FTPS URLs are supported".into());
        }
        let host = parsed.host_str().ok_or("no host")?;
        let port = parsed.port().unwrap_or(21);
        let username = parsed.username();
        let password = parsed.password().unwrap_or("");
        let username = if username.is_empty() {
            "anonymous"
        } else {
            username
        };
        let password = if password.is_empty() && username == "anonymous" {
            "guest"
        } else {
            password
        };

        let mut ftp = FtpStream::connect((host, port))?;
        ftp.login(username, password)?;
        ftp.set_mode(Mode::Passive);

        let files = asimov_ftp_module::list(&mut ftp, &url)?;

        for metadata in files {
            match options.output {
                OutputFormat::Jsonl | OutputFormat::Jsonld | OutputFormat::Json => {
                    println!("{}", metadata.to_jsonld()?)
                },
                OutputFormat::Cli => {
                    let print = |metadata: &FileMetadata| {
                        match options.flags.verbose {
                            0 => println!("{}", metadata.inline()),
                            1 => println!("{}", metadata.oneliner()),
                            2 => println!("{}", metadata.concise()),
                            3.. => println!("{}", metadata.detailed()),
                        };
                    };

                    print(&metadata);

                    if let FileType::Directory { children } = metadata.filetype {
                        for child in children {
                            let metadata = FileMetadata {
                                id: Some(child),
                                ..Default::default()
                            };

                            print(&metadata);
                        }
                    }
                },
            }
        }
    }

    Ok(EX_OK)
}
