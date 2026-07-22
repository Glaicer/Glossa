use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Command-line interface for the Glossa daemon and helper tools.
#[derive(Debug, Parser)]
#[command(
    name = "glossa",
    about = "Speech-to-text daemon for GNOME Wayland",
    version
)]
pub struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands supported by the binary.
#[derive(Debug, Subcommand)]
pub enum Command {
    Daemon,
    Service {
        #[command(subcommand)]
        service: ServiceCommand,
    },
    Ctl {
        #[command(subcommand)]
        ctl: CtlCommand,
    },
    Doctor,
    Status,
    #[command(hide = true)]
    StoreSecret {
        slot: String,
    },
    #[command(hide = true)]
    MigrateSecrets,
    Update,
}

/// Subcommands for the external control CLI.
#[derive(Debug, Subcommand)]
pub enum CtlCommand {
    Toggle,
    Stream,
    Shutdown,
}

/// Subcommands for managing the installed systemd user service.
#[derive(Debug, Clone, Copy, Subcommand, PartialEq, Eq)]
pub enum ServiceCommand {
    Start,
    Stop,
    Restart,
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;
    use clap::Parser;

    use super::{Cli, ServiceCommand};

    #[test]
    fn config_flag_after_subcommand_should_parse() {
        let cli = Cli::try_parse_from(["glossa", "daemon", "--config", "/tmp/glossa.toml"])
            .expect("config flag after daemon subcommand should parse");

        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("/tmp/glossa.toml"))
        );
    }

    #[test]
    fn config_flag_before_subcommand_should_parse() {
        let cli = Cli::try_parse_from(["glossa", "--config", "/tmp/glossa.toml", "daemon"])
            .expect("config flag before daemon subcommand should parse");

        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("/tmp/glossa.toml"))
        );
    }

    #[test]
    fn update_subcommand_should_parse() {
        let cli =
            Cli::try_parse_from(["glossa", "update"]).expect("update subcommand should parse");

        assert!(matches!(cli.command, super::Command::Update));
    }

    #[test]
    fn internal_secret_commands_should_parse() {
        let store = Cli::try_parse_from(["glossa", "store-secret", "provider"])
            .expect("store-secret should parse");
        assert!(matches!(
            store.command,
            super::Command::StoreSecret { slot } if slot == "provider"
        ));

        let migrate = Cli::try_parse_from([
            "glossa",
            "--config",
            "/tmp/glossa.toml",
            "migrate-secrets",
        ])
        .expect("migrate-secrets should parse");
        assert!(matches!(migrate.command, super::Command::MigrateSecrets));
    }

    #[test]
    fn service_start_subcommand_should_parse() {
        let cli = Cli::try_parse_from(["glossa", "service", "start"])
            .expect("service start subcommand should parse");

        assert!(matches!(
            cli.command,
            super::Command::Service {
                service: ServiceCommand::Start
            }
        ));
    }

    #[test]
    fn service_stop_subcommand_should_parse() {
        let cli = Cli::try_parse_from(["glossa", "service", "stop"])
            .expect("service stop subcommand should parse");

        assert!(matches!(
            cli.command,
            super::Command::Service {
                service: ServiceCommand::Stop
            }
        ));
    }

    #[test]
    fn service_restart_subcommand_should_parse() {
        let cli = Cli::try_parse_from(["glossa", "service", "restart"])
            .expect("service restart subcommand should parse");

        assert!(matches!(
            cli.command,
            super::Command::Service {
                service: ServiceCommand::Restart
            }
        ));
    }

    #[test]
    fn ctl_stream_subcommand_should_parse() {
        let cli =
            Cli::try_parse_from(["glossa", "ctl", "stream"]).expect("ctl stream should parse");

        assert!(matches!(
            cli.command,
            super::Command::Ctl {
                ctl: super::CtlCommand::Stream
            }
        ));
    }

    #[test]
    fn version_flag_should_be_available() {
        let error = Cli::try_parse_from(["glossa", "--version"])
            .expect_err("version flag should short-circuit parsing");

        assert_eq!(error.kind(), ErrorKind::DisplayVersion);
    }
}
