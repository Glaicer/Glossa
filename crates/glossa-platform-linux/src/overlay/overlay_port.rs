use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, mpsc::RecvTimeoutError, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_trait::async_trait;
use gtk::glib;
use tracing::{info, warn};

use super::pill::PillWindow;
use glossa_app::{ports::OverlayPort, AppError};
use glossa_core::AppConfig;

const OVERLAY_THREAD_NAME: &str = "glossa-overlay";
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(16);

enum OverlayCommand {
    Show,
    Hide,
}

/// Best-effort layer-shell overlay that shows a recording indicator at the
/// bottom of the screen. The surface never takes keyboard focus and lets
/// pointer events pass through to the windows below. The overlay stays
/// disabled unless the compositor exposes the wlr-layer-shell protocol.
pub struct BestEffortOverlayPort {
    enabled: bool,
    started: AtomicBool,
    command_tx: Mutex<Option<mpsc::Sender<OverlayCommand>>>,
    thread_handle: Mutex<Option<JoinHandle<()>>>,
}

impl BestEffortOverlayPort {
    #[must_use]
    pub fn new(config: &AppConfig) -> Self {
        Self {
            enabled: config.ui.overlay,
            started: AtomicBool::new(false),
            command_tx: Mutex::new(None),
            thread_handle: Mutex::new(None),
        }
    }

    fn ensure_started(&self) {
        if !self.enabled || self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        if !is_wayland_session() {
            info!("recording overlay requires a Wayland session; skipping overlay initialization");
            self.started.store(false, Ordering::SeqCst);
            return;
        }

        let (command_tx, command_rx) = mpsc::channel();
        if let Err(error) = self.set_command_tx(Some(command_tx)) {
            warn!(
                error = %error,
                "overlay command mutex is poisoned; skipping overlay initialization"
            );
            self.started.store(false, Ordering::SeqCst);
            return;
        }

        let handle = match thread::Builder::new()
            .name(OVERLAY_THREAD_NAME.into())
            .spawn(move || run_overlay_thread(command_rx))
        {
            Ok(handle) => handle,
            Err(error) => {
                warn!(error = %error, "failed to spawn overlay thread");
                self.started.store(false, Ordering::SeqCst);
                let _ = self.set_command_tx(None);
                return;
            }
        };

        match self.thread_handle.lock() {
            Ok(mut guard) => *guard = Some(handle),
            Err(_) => {
                warn!("overlay thread handle mutex is poisoned; shutting overlay thread down");
                self.started.store(false, Ordering::SeqCst);
                let _ = self.set_command_tx(None);
                if handle.thread().id() != thread::current().id() {
                    let _ = handle.join();
                }
            }
        }
    }

    fn set_command_tx(&self, sender: Option<mpsc::Sender<OverlayCommand>>) -> Result<(), String> {
        self.command_tx
            .lock()
            .map(|mut guard| *guard = sender)
            .map_err(|_| "overlay command mutex is poisoned".to_owned())
    }

    fn send_command(&self, command: OverlayCommand) {
        self.ensure_started();

        let sender = match self.command_tx.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => {
                warn!("overlay command mutex is poisoned; dropping overlay update");
                return;
            }
        };

        let Some(sender) = sender else {
            return;
        };

        if sender.send(command).is_err() {
            warn!("overlay thread is unavailable; dropping overlay update");
            let _ = self.set_command_tx(None);
            self.started.store(false, Ordering::SeqCst);
        }
    }
}

#[async_trait]
impl OverlayPort for BestEffortOverlayPort {
    async fn show_recording(&self) -> Result<(), AppError> {
        self.send_command(OverlayCommand::Show);
        Ok(())
    }

    async fn hide(&self) -> Result<(), AppError> {
        self.send_command(OverlayCommand::Hide);
        Ok(())
    }
}

fn run_overlay_thread(command_rx: mpsc::Receiver<OverlayCommand>) {
    if let Err(error) = gtk::init() {
        warn!(error = %error, "failed to initialize GTK overlay runtime");
        return;
    }

    if !gtk_layer_shell::is_supported() {
        warn!(
            "compositor does not support the wlr-layer-shell protocol (Mutter/GNOME does not expose it to clients); recording overlay stays disabled"
        );
        return;
    }

    let pill = PillWindow::new();
    let main_context = glib::MainContext::default();
    loop {
        let command = match command_rx.recv_timeout(EVENT_POLL_INTERVAL) {
            Ok(command) => Some(command),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if let Some(command) = command {
            match command {
                OverlayCommand::Show => pill.set_visible(true),
                OverlayCommand::Hide => pill.set_visible(false),
            }
        }
        while main_context.pending() {
            main_context.iteration(false);
        }
    }

    pill.set_visible(false);
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
    use super::is_wayland_session_values;

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
