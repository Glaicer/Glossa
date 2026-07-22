mod bootstrap;
mod cli;
mod cmd;

use anyhow::Context;
use clap::Parser;

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Daemon => cmd::daemon::run(cli.config).await,
        Command::Service { service } => cmd::service::run(service).await,
        Command::Ctl { ctl } => cmd::ctl::run(cli.config, ctl).await,
        Command::Doctor => cmd::doctor::run(cli.config).await,
        Command::Status => cmd::status::run(cli.config).await,
        Command::StoreSecret { slot } => cmd::store_secret::run(&slot),
        Command::MigrateSecrets => cmd::migrate_secrets::run(cli.config).await,
        Command::Update => cmd::update::run().await,
    }
    .context("command failed")
}
