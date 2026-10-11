use std::env;
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::oneshot;
use tracing::warn;

use glossa_app::{ports::OverlayPort, AppError};
use glossa_core::AppConfig;

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(16);
const WRITE_TIMEOUT: Duration = Duration::from_millis(50);

enum OverlayCommand {
    Show,
    Hide,
}

/// Best-effort recording indicator isolated from the daemon's GTK backend.
pub struct BestEffortOverlayPort {
    worker: Option<OverlayWorker>,
}

struct OverlayWorker {
    command_tx: mpsc::Sender<(OverlayCommand, oneshot::Sender<()>)>,
    thread_handle: JoinHandle<()>,
}

impl BestEffortOverlayPort {
    #[must_use]
    pub fn new(config: &AppConfig) -> Self {
        let command = if config.ui.overlay && is_wayland_session() {
            match env::current_exe() {
                Ok(path) => {
                    let mut command = Command::new(path);
                    command.arg("overlay-helper");
                    Some(command)
                }
                Err(error) => {
                    warn!(%error, "cannot locate recording overlay helper");
                    None
                }
            }
        } else {
            None
        };
        Self::with_config_and_command(config, command)
    }

    fn with_config_and_command(config: &AppConfig, command: Option<Command>) -> Self {
        let worker = command.filter(|_| config.ui.overlay).and_then(|command| {
            let (command_tx, command_rx) = mpsc::channel();
            match thread::Builder::new()
                .name("glossa-overlay".into())
                .spawn(move || run_worker(command, command_rx))
            {
                Ok(thread_handle) => Some(OverlayWorker {
                    command_tx,
                    thread_handle,
                }),
                Err(error) => {
                    warn!(%error, "failed to spawn recording overlay supervisor");
                    None
                }
            }
        });
        Self { worker }
    }

    async fn send_command(&self, command: OverlayCommand) {
        let Some(worker) = &self.worker else {
            return;
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if worker.command_tx.send((command, reply_tx)).is_err() {
            warn!("recording overlay supervisor is unavailable");
            return;
        }
        let _ = reply_rx.await;
    }
}

impl Drop for BestEffortOverlayPort {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            drop(worker.command_tx);
            let _ = worker.thread_handle.join();
        }
    }
}

#[async_trait]
impl OverlayPort for BestEffortOverlayPort {
    async fn show_recording(&self) -> Result<(), AppError> {
        glossa_app::level::record_peak(0.0);
        self.send_command(OverlayCommand::Show).await;
        Ok(())
    }

    async fn hide(&self) -> Result<(), AppError> {
        self.send_command(OverlayCommand::Hide).await;
        Ok(())
    }
}

struct Helper {
    child: Child,
    input: UnixStream,
}

impl Helper {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        let (input, child_input) = UnixStream::pair()?;
        input.set_write_timeout(Some(WRITE_TIMEOUT))?;
        command
            .env("GDK_BACKEND", "x11")
            .stdin(Stdio::from(OwnedFd::from(child_input)))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let child = command.spawn()?;
        Ok(Self { child, input })
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        if let Err(error) = self.child.wait() {
            warn!(%error, "failed to reap recording overlay helper");
        }
    }
}

fn run_worker(
    mut command: Command,
    command_rx: mpsc::Receiver<(OverlayCommand, oneshot::Sender<()>)>,
) {
    let mut helper: Option<Helper> = None;
    loop {
        match command_rx.recv_timeout(EVENT_POLL_INTERVAL) {
            Ok((OverlayCommand::Show, reply)) => {
                if helper.is_none() {
                    match Helper::spawn(&mut command) {
                        Ok(child) => helper = Some(child),
                        Err(error) => warn!(%error, "failed to start recording overlay helper"),
                    }
                }
                if let Some(child) = helper.as_mut() {
                    if let Err(error) = writeln!(child.input, "show") {
                        warn!(%error, "recording overlay helper is unavailable");
                        helper = None;
                    }
                }
                let _ = reply.send(());
            }
            Ok((OverlayCommand::Hide, reply)) => {
                helper = None;
                let _ = reply.send(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if let Some(child) = helper.as_mut() {
            match child.child.try_wait() {
                Ok(None) => {
                    if let Err(error) =
                        writeln!(child.input, "level {}", glossa_app::level::level_percent())
                    {
                        warn!(%error, "cannot send microphone level to recording overlay; dictation continues");
                        helper = None;
                    }
                }
                Ok(Some(status)) => {
                    warn!(%status, "recording overlay helper exited; dictation continues");
                    helper = None;
                }
                Err(error) => {
                    warn!(%error, "cannot monitor recording overlay helper; dictation continues");
                    helper = None;
                }
            }
        }
    }
}

fn is_wayland_session() -> bool {
    is_wayland_session_values(
        env::var("XDG_SESSION_TYPE").ok().as_deref(),
        env::var_os("WAYLAND_DISPLAY")
            .and_then(|value| value.into_string().ok())
            .as_deref(),
    )
}

fn is_wayland_session_values(session_type: Option<&str>, wayland_display: Option<&str>) -> bool {
    wayland_display.is_some_and(|value| !value.is_empty())
        || session_type.is_some_and(|value| value.eq_ignore_ascii_case("wayland"))
}

#[cfg(test)]
mod tests {
    use super::{is_wayland_session_values, BestEffortOverlayPort};
    use glossa_app::ports::OverlayPort;

    static METER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn read_log(path: &std::path::Path, expected: &str) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let log = std::fs::read_to_string(path).unwrap_or_default();
                if log.contains(expected) {
                    return log;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("helper should produce the expected output")
    }

    #[tokio::test]
    async fn recording_commands_should_reach_an_x11_helper_and_hide_should_reap_it() {
        let _guard = METER_TEST_LOCK.lock().await;
        let path = std::env::temp_dir().join(format!("glossa-overlay-{}.log", std::process::id()));
        let parent_backend = std::env::var_os("GDK_BACKEND");
        let mut command = std::process::Command::new("/bin/sh");
        command.env("GDK_BACKEND", "wayland");
        command.arg("-c").arg(
            "printf '%s %s\n' \"$$\" \"$GDK_BACKEND\" > \"$1\"; while read -r line; do printf '%s\n' \"$line\" >> \"$1\"; done",
        ).arg("fixture").arg(&path);
        let overlay = BestEffortOverlayPort::with_config_and_command(
            &glossa_core::AppConfig::default(),
            Some(command),
        );
        overlay.show_recording().await.expect("best effort show");
        let log = read_log(&path, "level 0\n").await;
        let pid = log.split_whitespace().next().expect("pid");
        assert!(log.lines().next().expect("header").ends_with(" x11"));
        assert!(log.contains("show\n"), "{log}");
        glossa_app::level::record_peak(1.0);
        let levels = read_log(&path, "level 100\n").await;
        assert!(levels.contains("level 100\n"), "{levels}");
        overlay.hide().await.expect("best effort hide");
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        std::fs::write(&path, "").expect("reset fixture log");
        overlay.show_recording().await.expect("second recording");
        let second = read_log(&path, "level 0\n").await;
        assert!(second.contains("level 0\n"), "{second}");
        assert!(!second.contains("level 100\n"), "stale level: {second}");
        let second_pid = second.split_whitespace().next().expect("second pid");
        assert_ne!(pid, second_pid);
        drop(overlay);
        assert!(!std::path::Path::new(&format!("/proc/{second_pid}")).exists());
        std::fs::remove_file(path).expect("remove fixture");
        assert_eq!(std::env::var_os("GDK_BACKEND"), parent_backend);
    }

    #[tokio::test]
    async fn disabled_overlay_should_not_start_a_helper() {
        let _guard = METER_TEST_LOCK.lock().await;
        let path = std::env::temp_dir().join(format!(
            "glossa-overlay-disabled-{}.log",
            std::process::id()
        ));
        let mut config = glossa_core::AppConfig::default();
        config.ui.overlay = false;
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("touch \"$1\"")
            .arg("fixture")
            .arg(&path);
        let overlay = BestEffortOverlayPort::with_config_and_command(&config, Some(command));
        overlay.show_recording().await.expect("disabled show");
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        overlay.hide().await.expect("disabled hide");
        drop(overlay);
        assert!(!path.exists(), "disabled overlay started a helper");
    }

    #[tokio::test]
    async fn missing_helper_should_not_fail_recording_controls() {
        let _guard = METER_TEST_LOCK.lock().await;
        let overlay = BestEffortOverlayPort::with_config_and_command(
            &glossa_core::AppConfig::default(),
            Some(std::process::Command::new(
                "/nonexistent/glossa-overlay-helper",
            )),
        );
        for _ in 0..2 {
            overlay
                .show_recording()
                .await
                .expect("missing helper must not fail show");
            overlay
                .hide()
                .await
                .expect("missing helper must not fail hide");
        }
    }

    #[tokio::test]
    async fn crashed_helper_should_be_reaped_and_recover_on_the_next_recording() {
        let _guard = METER_TEST_LOCK.lock().await;
        let path =
            std::env::temp_dir().join(format!("glossa-overlay-crash-{}.log", std::process::id()));
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg(
            "if [ ! -e \"$1\" ]; then printf '%s\n' \"$$\" > \"$1\"; exit 23; fi; printf '%s\n' \"$$\" >> \"$1\"; while read -r line; do printf '%s\n' \"$line\" >> \"$1\"; done",
        ).arg("fixture").arg(&path);
        let overlay = BestEffortOverlayPort::with_config_and_command(
            &glossa_core::AppConfig::default(),
            Some(command),
        );
        overlay
            .show_recording()
            .await
            .expect("crashed helper must not fail show");
        let first = read_log(&path, "\n").await;
        let pid = first.lines().next().expect("crashed pid");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("crashed helper should be reaped while recording");
        overlay.hide().await.expect("hide crashed helper");
        overlay.show_recording().await.expect("recover helper");
        let recovered = read_log(&path, "show\n").await;
        assert!(recovered.contains("show\n"), "{recovered}");
        let pid = recovered.lines().nth(1).expect("recovered pid");
        overlay.hide().await.expect("hide recovered helper");
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        std::fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn wayland_session_should_be_detected() {
        assert!(is_wayland_session_values(Some("wayland"), None));
        assert!(is_wayland_session_values(None, Some("wayland-0")));
    }

    #[test]
    fn missing_or_x11_session_should_be_detected_as_unsupported() {
        assert!(!is_wayland_session_values(None, None));
        assert!(!is_wayland_session_values(Some("x11"), None));
    }
}
