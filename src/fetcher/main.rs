// This is free and unencumbered software released into the public domain.

#[cfg(not(feature = "std"))]
compile_error!("asimov-ftp-fetcher requires the 'std' feature");

use asimov_ftp_module::TargetHost;
use asimov_module::{
    SysexitsError::{self, *},
    tracing,
};
use clap::Parser;
use clientele::StandardOptions;
use iri_string::format::ToDedicatedString as _;
use know::traits::ToJsonLd;
use std::{error::Error, io::Write as _, sync::Arc};
use suppaftp::{Mode, RustlsFtpStream};
// use url::Url;

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

    let target_hosts = asimov_ftp_module::group_targets(&options.urls)?;

    for (TargetHost(scheme, host, port, user), (url, paths)) in target_hosts {
        let host_url = iri_string::types::IriAbsoluteStr::new(&url)?;

        let ftp = RustlsFtpStream::connect((host.clone(), port))?;

        let mut ftp = if scheme == "ftps" {
            use suppaftp::{
                RustlsConnector,
                rustls::{ClientConfig, RootCertStore},
            };

            let root_store =
                RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

            let config = ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth();

            let ctx = RustlsConnector::from(Arc::new(config));
            ftp.into_secure(ctx, &host)
                .inspect_err(|e| tracing::error!("{e}"))?
        } else {
            ftp
        };

        ftp.login(user.0, user.1)?;
        ftp.set_mode(Mode::Passive);

        for path in paths {
            let data = ftp.retr_as_buffer(&path)?.into_inner();

            let name = path.split('/').next_back().unwrap_or(&path).to_string();

            let file_url = iri_string::types::IriRelativeStr::new(&path)?
                .resolve_against(host_url)
                .and_normalize()
                .to_dedicated_string()
                .to_string();

            let file = know::classes::File {
                id: Some(file_url.clone()),
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

        let _ = ftp.quit().ok();
    }

    Ok(EX_OK)
}
