//! `meta-whatsapp-server`: see `meta_whatsapp_server::cli` and
//! docs/guides/server.md.

use clap::Parser as _;
use meta_whatsapp_server::cli::{Cli, run};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // Errors name settings, never their values.
            eprintln!("meta-whatsapp-server: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
