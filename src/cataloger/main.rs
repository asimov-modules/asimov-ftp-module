// This is free and unencumbered software released into the public domain.

#[cfg(not(feature = "std"))]
compile_error!("asimov-ftp-cataloger requires the 'std' feature");

use asimov_ftp_module::TargetHost;
use asimov_module::{
    SysexitsError::{self, *},
    tracing,
};
use clap::Parser;
use clientele::StandardOptions;
use know::{classes::FileMetadata, traits::ToJsonLd};
use std::{error::Error, sync::Arc};
use suppaftp::{Mode, RustlsFtpStream};

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

    let target_hosts = asimov_ftp_module::group_targets(&options.urls)?;

    for (TargetHost(scheme, host, port, user), (url, paths)) in target_hosts {
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
            let files = asimov_ftp_module::list(&mut ftp, &path, &url)?;

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
                    },
                }
            }
        }

        let _ = ftp.quit().ok();
    }

    Ok(EX_OK)
}
