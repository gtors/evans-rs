pub mod cli;
pub mod repl;

use anyhow::Result;

use crate::cli::{Cli, Command};
use crate::config::Config;
use crate::idl::Idl;

pub async fn run(cli: Cli) -> Result<()> {
    let cfg = match crate::config::load(&cli).await {
        Ok(cfg) => cfg,
        Err(err) => return Err(err.context("evans")),
    };

    let result = run_mode(&cfg, &cli).await;

    // The prefix of the log is prepended to the user-facing error messages.
    result.map_err(|err| err.context(log_prefix(&cfg)))
}

/// Runs the mode that the command line selects.
async fn run_mode(cfg: &Config, cli: &Cli) -> Result<()> {
    let idl = build_idl(cfg).await?;
    match &cli.command {
        None | Some(Command::Repl { .. }) => run_repl(cfg, &idl).await,
        Some(Command::Cli { command, .. }) => cli::run(cfg, &idl, command).await,
    }
}

/// The prefix of the user-facing error messages, without its trailing separator.
fn log_prefix(cfg: &Config) -> String {
    cfg.log
        .prefix
        .trim_end()
        .trim_end_matches(':')
        .trim_end()
        .to_string()
}

async fn build_idl(cfg: &Config) -> Result<Idl> {
    if cfg.server.reflection {
        let channel = crate::grpc::connect(&cfg.server, &cfg.request).await?;
        let pool = crate::idl::reflection::load(channel, &cfg.request.header).await?;
        Ok(Idl::from_pool(pool))
    } else {
        Idl::from_files(&cfg.default.proto_path, &cfg.default.proto_file)
    }
}

async fn run_repl(cfg: &Config, idl: &Idl) -> Result<()> {
    let mut client = crate::grpc::Client::new(&cfg.server, &cfg.request).await?;
    let mut repl = crate::repl::Repl::new(cfg, idl, &mut client)?;
    repl.run().await
}
