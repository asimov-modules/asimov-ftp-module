// This is free and unencumbered software released into the public domain.

#[cfg(not(feature = "std"))]
compile_error!("asimov-ftp-fetcher requires the 'std' feature");

use asimov_module::SysexitsError::{self, *};
use clap::Parser;
use clientele::StandardOptions;
use know::traits::ToJsonLd;
use std::{error::Error, io::Write as _};
use suppaftp::FtpStream;
use url::Url;

/// asimov-ftp-fetcher
#[derive(Debug, Parser)]
#[command(arg_required_else_help = true)]
struct Options {
    #[clap(flatten)]
    flags: StandardOptions,

    /// The output format.
    #[arg(value_name = "FORMAT", short = 'o', long, default_value_t, value_enum)]
    output: OutputFormat,

    /// The `ftp:` or `ftps:` URLs to fetch
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

    let mut output = std::io::stdout().lock();

    for url in options.urls {
        let parsed = Url::parse(&url)?;
        let scheme = parsed.scheme();
        if scheme != "ftp" && scheme != "ftps" {
            return Err("only FTP and FTPS URLs are supported".into());
        }
        let host = parsed.host_str().ok_or("no host")?;
        let port = parsed.port().unwrap_or(21);
        let path = parsed.path().strip_prefix('/').unwrap();
        let username = parsed.username();
        let password = parsed.password().unwrap_or("");
        let username = if username.is_empty() {
            "anonymous"
        } else {
            username
        };
        let password = if password.is_empty() && username == "anonymous" {
            "anonymous"
        } else {
            password
        };

        let mut ftp = FtpStream::connect((host, port))?;
        ftp.login(username, password)?;

        let data = ftp.retr_as_buffer(path)?.into_inner();

        let name = path.split('/').next_back().unwrap_or(path).to_string();

        let file = know::classes::File {
            id: Some(url.clone()),
            name: Some(name),
            size: data.len() as u64,
            data,
        };

        match options.output {
            OutputFormat::Jsonl | OutputFormat::Jsonld | OutputFormat::Json => {
                writeln!(&mut output, "{}", file.to_jsonld()?)?;
            },
            OutputFormat::Cli => {
                output.write_all(&file.data)?;
            },
        }
    }

    Ok(EX_OK)
}
