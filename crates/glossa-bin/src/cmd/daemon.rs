use anyhow::{anyhow, Context};
use tokio::task::JoinHandle;
use tracing::{error, info};

use glossa_app::{ports::CommandSource, ActorExit};
use glossa_core::InputBackend;
use glossa_platform_linux::{
    dialog::show_fatal_error_dialog,
    ipc::IpcServer,
    portal::{run_escape_cancel_monitor, PortalShortcutSource},
    secret::migrate_plaintext_api_keys,
};

use crate::bootstrap::{build_actor, build_tray, init_tracing, load_config};

const FATAL_ERROR_TITLE: &str = "Glossa fatal error";

pub async fn run(config_path: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    handle_fatal_daemon_result(run_inner(config_path).await, report_fatal_error).await
}

async fn run_inner(config_path: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let config_path =
        config_path.ok_or_else(|| anyhow!("`glossa daemon` requires --config <path>"))?;
    let mut config = load_config(&config_path).await?;
    migrate_plaintext_api_keys(&config_path, &mut config)?;
    init_tracing(&config)?;
    info!(path = %config_path.display(), "starting glossa daemon");
    let tray = build_tray(&config_path, &config);

    loop {
        let socket_path = config
            .control
            .socket_path
            .resolve()
            .context("failed to resolve daemon socket path")?;
        let input_backend = config.input.backend;
        let portal_config = config.input.clone();
        let cli_enabled = config.control.enable_cli;

        let tray_port: std::sync::Arc<dyn glossa_app::ports::TrayPort> = tray.clone();
        let (actor, handle) = build_actor(config, tray_port.clone())?;
        tray.bind_command_sender(handle.command_sender());
        let mut tasks: Vec<JoinHandle<()>> = Vec::new();
        let escape_status_rx = handle.subscribe();
        let escape_command_tx = handle.command_sender();

        tasks.push(tokio::spawn(async move {
            if let Err(error) = run_escape_cancel_monitor(escape_status_rx, escape_command_tx).await
            {
                error!(error = %error, "escape cancel monitor exited with an error");
            }
        }));

        if cli_enabled {
            let server = IpcServer::new(socket_path, handle.clone());
            tasks.push(tokio::spawn(async move {
                if let Err(error) = server.run().await {
                    error!(error = %error, "ipc server exited with an error");
                }
            }));
        }

        if input_backend == InputBackend::Portal {
            let tx = handle.command_sender();
            let source: Box<dyn CommandSource> = Box::new(PortalShortcutSource::new(
                portal_config,
                Some(tray_port.clone()),
            ));
            tasks.push(tokio::spawn(async move {
                if let Err(error) = source.run(tx).await {
                    error!(error = %error, "portal shortcut source exited with an error");
                }
            }));
        }

        let exit = actor.run().await?;
        for task in tasks {
            task.abort();
        }

        match exit {
            ActorExit::Shutdown => break,
            ActorExit::Restart => {
                info!(path = %config_path.display(), "restarting glossa daemon");
                config = load_config(&config_path).await?;
                migrate_plaintext_api_keys(&config_path, &mut config)?;
            }
        }
    }

    Ok(())
}

async fn handle_fatal_daemon_result<F>(
    result: anyhow::Result<()>,
    reporter: F,
) -> anyhow::Result<()>
where
    F: Fn(&str, &str) -> Result<(), String>,
{
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            let message = format_fatal_error_message(&error);
            eprintln!("{message}");

            if let Err(dialog_error) = reporter(FATAL_ERROR_TITLE, &message) {
                eprintln!("failed to show fatal error dialog: {dialog_error}");
            }

            Ok(())
        }
    }
}

fn format_fatal_error_message(error: &anyhow::Error) -> String {
    format!("Glossa encountered a fatal error and exited.\n\n{error:#}")
}

fn report_fatal_error(title: &str, message: &str) -> Result<(), String> {
    show_fatal_error_dialog(title, message).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;

    use super::{format_fatal_error_message, handle_fatal_daemon_result};

    #[tokio::test]
    async fn handle_fatal_daemon_result_should_report_error_and_return_ok_when_inner_run_fails() {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let reporter_messages = Arc::clone(&messages);

        let result = handle_fatal_daemon_result(
            Err(anyhow!("missing secret value from env:GROQ_API_KEY").context("command failed")),
            move |title, message| {
                reporter_messages
                    .lock()
                    .expect("mutex should not be poisoned")
                    .push((title.to_owned(), message.to_owned()));
                Ok(())
            },
        )
        .await;

        assert!(result.is_ok());

        let messages = messages.lock().expect("mutex should not be poisoned");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, "Glossa fatal error");
        assert!(messages[0]
            .1
            .contains("missing secret value from env:GROQ_API_KEY"));
    }

    #[tokio::test]
    async fn handle_fatal_daemon_result_should_not_report_when_inner_run_succeeds() {
        let called = Arc::new(Mutex::new(false));
        let reporter_called = Arc::clone(&called);

        let result = handle_fatal_daemon_result(Ok(()), move |_, _| {
            *reporter_called
                .lock()
                .expect("mutex should not be poisoned") = true;
            Ok(())
        })
        .await;

        assert!(result.is_ok());
        assert!(
            !*called.lock().expect("mutex should not be poisoned"),
            "reporter should not be called on success"
        );
    }

    #[tokio::test]
    async fn handle_fatal_daemon_result_should_return_ok_when_reporter_fails() {
        let result = handle_fatal_daemon_result(Err(anyhow!("fatal startup error")), |_, _| {
            Err("gtk unavailable".to_owned())
        })
        .await;

        assert!(result.is_ok());
    }

    #[test]
    fn format_fatal_error_message_should_include_error_chain() {
        let error = anyhow!("missing secret value from env:GROQ_API_KEY").context("command failed");

        let message = format_fatal_error_message(&error);

        assert!(message.contains("Glossa encountered a fatal error and exited."));
        assert!(message.contains("command failed"));
        assert!(message.contains("missing secret value from env:GROQ_API_KEY"));
    }
}
