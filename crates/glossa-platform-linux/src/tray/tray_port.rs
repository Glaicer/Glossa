use std::{
    cell::RefCell,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    thread,
    time::Duration,
};

use async_trait::async_trait;
use camino::Utf8Path;
use gtk::{
    events_pending,
    gdk::{self, keys::constants as keyconst},
    glib::{translate::IntoGlib, Propagation},
    init as gtk_init, main_iteration_do,
    prelude::*,
    Box as GtkBox, ButtonsType, Dialog, DialogFlags, Label, MessageDialog, MessageType,
    Orientation, ResponseType, Window,
};
use png::{ColorType, Decoder};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

use glossa_app::{
    ports::{TrayPort, TrayState},
    AppError,
};
use glossa_core::{AppCommand, AppConfig, CommandOrigin, InputBackend, InputMode, UiConfig};

use crate::dialog::show_message_dialog;
use crate::portal::{portal_shortcut_description, PORTAL_APP_ID, PORTAL_SHORTCUT_ID};
use crate::service::restart_glossa_service;
use crate::shortcut_capture::begin_shortcut_capture;
use crate::updater::{check_for_update, install_update, UpdaterStatus};

use super::settings::{write_settings_to_config, SettingsValues};
use super::settings_dialog::edit_settings;

const TRAY_THREAD_NAME: &str = "glossa-tray";
const NOTIFY_SEND_COMMAND: &str = "notify-send";
const APPINDICATOR_LIBRARY_NAMES: &[&str] = &[
    "libayatana-appindicator3.so.1",
    "libappindicator3.so.1",
    "libayatana-appindicator3.so",
    "libappindicator3.so",
];
const APPINDICATOR_LIBRARY_DIRS: &[&str] = &[
    "/lib64",
    "/usr/lib64",
    "/lib",
    "/usr/lib",
    "/lib/x86_64-linux-gnu",
    "/usr/lib/x86_64-linux-gnu",
    "/lib/aarch64-linux-gnu",
    "/usr/lib/aarch64-linux-gnu",
];

/// Best-effort tray port that runs a GTK/AppIndicator tray on GNOME Wayland.
pub struct BestEffortTrayPort {
    enabled: bool,
    ui: UiConfig,
    shortcut_binding: Option<ShortcutBindingConfig>,
    settings: SettingsValues,
    config_path: PathBuf,
    app_command_tx: std::sync::Arc<Mutex<Option<UnboundedSender<AppCommand>>>>,
    command_tx: Mutex<Option<mpsc::Sender<TrayCommand>>>,
    thread_handle: Mutex<Option<thread::JoinHandle<()>>>,
    started: AtomicBool,
}

impl BestEffortTrayPort {
    #[must_use]
    pub fn new(config_path: PathBuf, config: &AppConfig) -> Self {
        Self {
            enabled: config.ui.tray,
            ui: config.ui.clone(),
            shortcut_binding: (config.input.backend == InputBackend::Portal).then_some(
                ShortcutBindingConfig {
                    current_shortcut: None,
                    mode: config.input.mode,
                },
            ),
            settings: SettingsValues::from_config(config),
            config_path,
            app_command_tx: std::sync::Arc::new(Mutex::new(None)),
            command_tx: Mutex::new(None),
            thread_handle: Mutex::new(None),
            started: AtomicBool::new(false),
        }
    }

    pub fn bind_command_sender(&self, app_command_tx: UnboundedSender<AppCommand>) {
        if !self.enabled {
            info!("tray is disabled by configuration");
            return;
        }

        match self.app_command_tx.lock() {
            Ok(mut guard) => {
                *guard = Some(app_command_tx);
            }
            Err(_) => {
                warn!("tray app command mutex is poisoned; skipping tray initialization");
                return;
            }
        }

        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        if !is_supported_tray_environment() {
            warn!("tray is only enabled on GNOME Wayland; skipping tray initialization");
            self.started.store(false, Ordering::SeqCst);
            return;
        }

        if !has_appindicator_runtime_library() {
            warn!(
                "tray requires ayatana-appindicator3 or appindicator3 runtime library; install libayatana-appindicator-gtk3 on Fedora or the equivalent package for your distro; skipping tray initialization"
            );
            self.started.store(false, Ordering::SeqCst);
            return;
        }

        let (command_tx, command_rx) = mpsc::channel();
        match self.command_tx.lock() {
            Ok(mut guard) => {
                *guard = Some(command_tx);
            }
            Err(_) => {
                warn!("tray command mutex is poisoned; skipping tray initialization");
                self.started.store(false, Ordering::SeqCst);
                return;
            }
        }

        let ui = self.ui.clone();
        let shortcut_binding = self.shortcut_binding.clone();
        let settings = self.settings.clone();
        let config_path = self.config_path.clone();
        let app_command_tx = self.app_command_tx.clone();
        let handle = match thread::Builder::new()
            .name(TRAY_THREAD_NAME.into())
            .spawn(move || {
                run_tray_thread(
                    ui,
                    shortcut_binding,
                    settings,
                    config_path,
                    app_command_tx,
                    command_rx,
                )
            }) {
            Ok(handle) => handle,
            Err(error) => {
                warn!(error = %error, "failed to spawn tray thread");
                self.started.store(false, Ordering::SeqCst);
                if let Ok(mut guard) = self.command_tx.lock() {
                    *guard = None;
                }
                return;
            }
        };

        match self.thread_handle.lock() {
            Ok(mut guard) => {
                *guard = Some(handle);
            }
            Err(_) => {
                warn!("tray thread handle mutex is poisoned; shutting tray thread down");
                self.started.store(false, Ordering::SeqCst);
                if let Ok(mut guard) = self.command_tx.lock() {
                    *guard = None;
                }
                let _ = handle.join();
            }
        }
    }

    fn send_command(&self, command: TrayCommand) {
        let sender = match self.command_tx.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => {
                warn!("tray command mutex is poisoned");
                None
            }
        };

        let Some(sender) = sender else {
            return;
        };

        if sender.send(command).is_err() {
            warn!("tray thread is unavailable; dropping tray update");
            if let Ok(mut guard) = self.command_tx.lock() {
                *guard = None;
            }
            self.started.store(false, Ordering::SeqCst);
        }
    }
}

#[async_trait]
impl TrayPort for BestEffortTrayPort {
    async fn set_state(&self, state: TrayState) -> Result<(), AppError> {
        self.send_command(TrayCommand::SetState(state));
        Ok(())
    }

    async fn set_shortcut_description(&self, description: Option<&str>) -> Result<(), AppError> {
        self.send_command(TrayCommand::SetShortcutDescription(
            description.map(ToOwned::to_owned),
        ));
        Ok(())
    }

    async fn set_mic_stream_state(&self, active: bool) -> Result<(), AppError> {
        self.send_command(TrayCommand::SetMicStreamState(active));
        Ok(())
    }

    async fn show_error(&self, message: &str) -> Result<(), AppError> {
        if let Err(error) = send_error_notification(message).await {
            warn!(error = %error, message, "failed to show desktop notification");
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ShortcutBindingConfig {
    current_shortcut: Option<String>,
    mode: InputMode,
}

#[derive(Clone)]
enum TrayCommand {
    SetState(TrayState),
    SetShortcutDescription(Option<String>),
    SetMicStreamState(bool),
    Shutdown,
}

fn run_tray_thread(
    ui: UiConfig,
    shortcut_binding: Option<ShortcutBindingConfig>,
    settings: SettingsValues,
    config_path: PathBuf,
    app_command_tx: std::sync::Arc<Mutex<Option<UnboundedSender<AppCommand>>>>,
    command_rx: mpsc::Receiver<TrayCommand>,
) {
    if let Err(error) = gtk_init() {
        warn!(error = %error, "failed to initialize GTK tray runtime");
        return;
    }

    let runtime =
        match TrayRuntime::new(&ui, shortcut_binding, settings, config_path, app_command_tx) {
            Ok(runtime) => runtime,
            Err(error) => {
                warn!(error = %error, "failed to create tray icon");
                return;
            }
        };

    info!("tray icon initialized");
    loop {
        pump_gtk_events();
        runtime.handle_menu_events();

        match command_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(TrayCommand::SetState(state)) => {
                if let Err(error) = runtime.set_state(state) {
                    warn!(error = %error, ?state, "failed to update tray icon");
                }
            }
            Ok(TrayCommand::SetShortcutDescription(description)) => {
                runtime.set_shortcut_description(description);
            }
            Ok(TrayCommand::SetMicStreamState(active)) => {
                runtime.set_mic_stream_state(active);
            }
            Ok(TrayCommand::Shutdown) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

async fn send_error_notification(message: &str) -> Result<(), AppError> {
    let status = TokioCommand::new(NOTIFY_SEND_COMMAND)
        .args(notify_send_arguments(message))
        .status()
        .await
        .map_err(|error| AppError::io("failed to spawn notify-send", error))?;

    if status.success() {
        Ok(())
    } else {
        Err(AppError::message(format!(
            "notify-send exited with status {status}"
        )))
    }
}

fn notify_send_arguments(message: &str) -> Vec<&str> {
    vec!["-a", "Glossa", "-t", "3000", "Glossa", message]
}

fn pump_gtk_events() {
    while events_pending() {
        main_iteration_do(false);
    }
}

struct TrayRuntime {
    _tray_icon: TrayIcon,
    change_shortcut_id: Option<tray_icon::menu::MenuId>,
    settings_id: tray_icon::menu::MenuId,
    update_id: tray_icon::menu::MenuId,
    mic_stream_id: tray_icon::menu::MenuId,
    mic_stream_item: CheckMenuItem,
    llm_enhancer_id: tray_icon::menu::MenuId,
    llm_enhancer_item: CheckMenuItem,
    exit_id: tray_icon::menu::MenuId,
    icons: TrayIcons,
    shortcut_binding: RefCell<Option<ShortcutBindingConfig>>,
    settings: RefCell<SettingsValues>,
    config_path: PathBuf,
    app_command_tx: std::sync::Arc<Mutex<Option<UnboundedSender<AppCommand>>>>,
}

impl TrayRuntime {
    fn new(
        ui: &UiConfig,
        shortcut_binding: Option<ShortcutBindingConfig>,
        settings: SettingsValues,
        config_path: PathBuf,
        app_command_tx: std::sync::Arc<Mutex<Option<UnboundedSender<AppCommand>>>>,
    ) -> Result<Self, String> {
        let icons = TrayIcons::load(ui)?;
        let change_shortcut_item = MenuItem::new("Change shortcut", true, None);
        let change_shortcut_id = shortcut_binding
            .as_ref()
            .map(|_| change_shortcut_item.id().clone());
        let settings_item = MenuItem::new("Settings", true, None);
        let settings_id = settings_item.id().clone();
        let update_item = MenuItem::new("Update", true, None);
        let update_id = update_item.id().clone();
        let version_item = MenuItem::new(&tray_version_label(), false, None);
        let mic_stream_item = CheckMenuItem::new(&mic_stream_label(false), true, false, None);
        let mic_stream_id = mic_stream_item.id().clone();
        let llm_enhancer_item =
            CheckMenuItem::new("AI enhancer", true, settings.llm_enabled, None);
        let llm_enhancer_id = llm_enhancer_item.id().clone();
        let exit_item = MenuItem::new("Exit", true, None);
        let exit_id = exit_item.id().clone();

        let menu = Menu::new();
        if shortcut_binding.is_some() {
            menu.append(&change_shortcut_item)
                .map_err(|error| format!("failed to build tray menu: {error}"))?;
        }
        menu.append(&settings_item)
            .map_err(|error| format!("failed to build tray menu: {error}"))?;
        menu.append(&update_item)
            .map_err(|error| format!("failed to build tray menu: {error}"))?;
        menu.append(&mic_stream_item)
            .map_err(|error| format!("failed to build tray menu: {error}"))?;
        menu.append(&llm_enhancer_item)
            .map_err(|error| format!("failed to build tray menu: {error}"))?;
        menu.append(&version_item)
            .map_err(|error| format!("failed to build tray menu: {error}"))?;
        menu.append(&exit_item)
            .map_err(|error| format!("failed to build tray menu: {error}"))?;

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icons.idle.clone())
            .with_temp_dir_path(tray_icon_temp_dir()?)
            .build()
            .map_err(|error| format!("failed to create tray icon: {error}"))?;

        Ok(Self {
            _tray_icon: tray_icon,
            change_shortcut_id,
            settings_id,
            update_id,
            mic_stream_id,
            mic_stream_item,
            llm_enhancer_id,
            llm_enhancer_item,
            exit_id,
            icons,
            shortcut_binding: RefCell::new(shortcut_binding),
            settings: RefCell::new(settings),
            config_path,
            app_command_tx,
        })
    }

    fn set_state(&self, state: TrayState) -> Result<(), String> {
        let icon = match state {
            TrayState::Idle => self.icons.idle.clone(),
            TrayState::Recording => self.icons.recording.clone(),
            TrayState::Processing => self.icons.processing.clone(),
        };

        self._tray_icon
            .set_icon(Some(icon))
            .map_err(|error| format!("failed to set tray icon: {error}"))
    }

    fn set_shortcut_description(&self, description: Option<String>) {
        if let Some(binding) = self.shortcut_binding.borrow_mut().as_mut() {
            binding.current_shortcut = description;
        }
    }

    fn set_mic_stream_state(&self, active: bool) {
        self.mic_stream_item.set_checked(active);
        self.mic_stream_item.set_text(mic_stream_label(active));
    }

    fn handle_menu_events(&self) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if self
                .change_shortcut_id
                .as_ref()
                .is_some_and(|id| id == &event.id)
            {
                self.handle_change_shortcut();
                continue;
            }

            if event.id == self.settings_id {
                self.handle_settings();
                continue;
            }

            if event.id == self.update_id {
                self.handle_update();
                continue;
            }

            if event.id == self.mic_stream_id {
                self.handle_mic_stream_toggle();
                continue;
            }

            if event.id == self.llm_enhancer_id {
                self.handle_llm_enhancer_toggle();
                continue;
            }

            if event.id != self.exit_id {
                continue;
            }

            if let Err(error) = self.send_app_command(AppCommand::Shutdown {
                origin: CommandOrigin::TrayMenu,
            }) {
                warn!(error = %error, "failed to send shutdown command from tray");
            }
        }
    }

    fn handle_mic_stream_toggle(&self) {
        let active = self.mic_stream_item.is_checked();
        self.set_mic_stream_state(active);
        let command = if active {
            AppCommand::EnableInputStream {
                origin: CommandOrigin::TrayMenu,
            }
        } else {
            AppCommand::DisableInputStream {
                origin: CommandOrigin::TrayMenu,
            }
        };

        if let Err(error) = self.send_app_command(command) {
            warn!(error = %error, "failed to send input stream command from tray");
        }
    }

    fn handle_llm_enhancer_toggle(&self) {
        let enabled = self.llm_enhancer_item.is_checked();
        let mut settings = self.settings.borrow().clone();
        settings.llm_enabled = enabled;

        if let Err(error) = write_settings_to_config(&self.config_path, &mut settings) {
            warn!(
                error = %error,
                path = %self.config_path.display(),
                "failed to save LLM enhancer setting from tray"
            );
            let previous = self.settings.borrow().llm_enabled;
            self.llm_enhancer_item.set_checked(previous);
            let _ = show_message_dialog("LLM enhancer", &error.to_string(), MessageType::Error);
            return;
        }

        *self.settings.borrow_mut() = settings;
        self.llm_enhancer_item.set_checked(enabled);

        if let Err(error) = restart_glossa_service() {
            warn!(error = %error, "failed to restart glossa service after toggling LLM enhancer");
            let _ = show_message_dialog(
                "LLM enhancer",
                &format!(
                    "The LLM enhancer setting was saved, but `systemctl --user restart glossa` failed.\n\n{error}"
                ),
                MessageType::Warning,
            );
        }
    }

    fn handle_settings(&self) {
        let current_settings = self.settings.borrow().clone();
        let edited = match edit_settings(&current_settings) {
            Ok(result) => result,
            Err(error) => {
                warn!(error = %error, "failed to open settings dialog from tray");
                let _ = show_message_dialog("Settings", &error.to_string(), MessageType::Error);
                return;
            }
        };

        let Some(mut settings) = edited else {
            info!("settings change cancelled from tray");
            return;
        };

        if let Err(error) = write_settings_to_config(&self.config_path, &mut settings) {
            warn!(
                error = %error,
                path = %self.config_path.display(),
                "failed to save settings from tray"
            );
            let _ = show_message_dialog("Settings", &error.to_string(), MessageType::Error);
            return;
        }

        *self.settings.borrow_mut() = settings;

        if let Err(error) = restart_glossa_service() {
            warn!(error = %error, "failed to restart glossa service after saving settings");
            let _ = show_message_dialog(
                "Settings",
                &format!(
                    "The settings were saved, but `systemctl --user restart glossa` failed.\n\n{error}"
                ),
                MessageType::Warning,
            );
        }
    }

    fn handle_change_shortcut(&self) {
        let Some(binding) = self.shortcut_binding.borrow().clone() else {
            return;
        };

        let captured = match capture_shortcut(&binding) {
            Ok(result) => result,
            Err(error) => {
                warn!(error = %error, "failed to capture shortcut from tray");
                let _ = show_message_dialog("Change shortcut", &error, MessageType::Error);
                return;
            }
        };

        let Some(shortcut) = captured else {
            info!("shortcut change cancelled from tray");
            return;
        };

        if let Err(error) = write_shortcut_override(&binding, &shortcut) {
            warn!(error = %error, shortcut = %shortcut, "failed to update GNOME shortcut override");
            let _ = show_message_dialog("Change shortcut", &error, MessageType::Error);
            return;
        }

        if let Some(current) = self.shortcut_binding.borrow_mut().as_mut() {
            current.current_shortcut = Some(shortcut.clone());
        }

        info!(shortcut = %shortcut, "updated GNOME shortcut override from tray");
        if let Err(error) = self.send_app_command(AppCommand::Restart {
            origin: CommandOrigin::TrayMenu,
        }) {
            warn!(error = %error, "failed to restart daemon after shortcut change");
            let _ = show_message_dialog(
                "Change shortcut",
                "The shortcut was stored, but the daemon could not restart. Restart it manually.",
                MessageType::Warning,
            );
        }
    }

    fn handle_update(&self) {
        let check_result = match check_for_update() {
            Ok(result) => result,
            Err(error) => {
                warn!(error = %error, "failed to check for updates from tray");
                let _ = show_message_dialog("Update", &error.to_string(), MessageType::Error);
                return;
            }
        };

        match check_result.status {
            UpdaterStatus::UpToDate => {
                let _ = show_message_dialog(
                    "Update",
                    "The latest version is already installed.",
                    MessageType::Info,
                );
            }
            UpdaterStatus::Available => {
                let confirmed = show_confirmation_dialog(
                    "Update",
                    &format!(
                        "A new version {} is available.\n\nDo you want to install it now?",
                        check_result.version
                    ),
                );

                if !confirmed {
                    return;
                }

                match install_update() {
                    Ok(result) if result.status == UpdaterStatus::Updated => {
                        let _ = show_message_dialog(
                            "Update",
                            &format!("New version {} installed successfully.", result.version),
                            MessageType::Info,
                        );
                    }
                    Ok(result) if result.status == UpdaterStatus::UpToDate => {
                        let _ = show_message_dialog(
                            "Update",
                            "The latest version is already installed.",
                            MessageType::Info,
                        );
                    }
                    Ok(result) => {
                        warn!(?result, "unexpected updater result after install");
                        let _ = show_message_dialog(
                            "Update",
                            "The updater returned an unexpected result.",
                            MessageType::Error,
                        );
                    }
                    Err(error) => {
                        warn!(error = %error, "failed to install update from tray");
                        let _ =
                            show_message_dialog("Update", &error.to_string(), MessageType::Error);
                    }
                }
            }
            UpdaterStatus::Updated => {
                warn!("update check unexpectedly returned 'updated' status");
                let _ = show_message_dialog(
                    "Update",
                    "The updater returned an unexpected result.",
                    MessageType::Error,
                );
            }
        }
    }

    fn send_app_command(&self, command: AppCommand) -> Result<(), AppError> {
        let sender = match self.app_command_tx.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => {
                return Err(AppError::message("tray app command mutex is poisoned"));
            }
        };

        let Some(sender) = sender else {
            return Err(AppError::message("daemon command sender is not bound"));
        };

        sender
            .send(command)
            .map_err(|_| AppError::message("daemon command channel is closed"))
    }
}

struct TrayIcons {
    idle: Icon,
    recording: Icon,
    processing: Icon,
}

impl TrayIcons {
    fn load(ui: &UiConfig) -> Result<Self, String> {
        Ok(Self {
            idle: load_icon(ui.idle_tray_icon())?,
            recording: load_icon(ui.recording_tray_icon())?,
            processing: load_icon(ui.processing_tray_icon())?,
        })
    }
}

fn load_icon(path: &Utf8Path) -> Result<Icon, String> {
    let file = fs::File::open(path)
        .map_err(|error| format!("failed to open tray icon {path}: {error}"))?;
    let decoder = Decoder::new(file);

    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("failed to decode tray icon {path}: {error}"))?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| format!("failed to read tray icon {path}: {error}"))?;
    let rgba = normalize_rgba(&buffer[..info.buffer_size()], info.color_type)
        .map_err(|error| format!("failed to normalize tray icon {path}: {error}"))?;

    Icon::from_rgba(rgba, info.width, info.height)
        .map_err(|error| format!("failed to convert tray icon {path}: {error}"))
}

fn normalize_rgba(bytes: &[u8], color_type: ColorType) -> Result<Vec<u8>, &'static str> {
    let rgba = match color_type {
        ColorType::Rgba => bytes.to_vec(),
        ColorType::Rgb => bytes
            .chunks_exact(3)
            .flat_map(|chunk| [chunk[0], chunk[1], chunk[2], u8::MAX])
            .collect(),
        ColorType::Grayscale => bytes
            .iter()
            .flat_map(|value| [*value, *value, *value, u8::MAX])
            .collect(),
        ColorType::GrayscaleAlpha => bytes
            .chunks_exact(2)
            .flat_map(|chunk| [chunk[0], chunk[0], chunk[0], chunk[1]])
            .collect(),
        ColorType::Indexed => return Err("indexed PNG tray icons are unsupported"),
    };

    Ok(rgba)
}

fn tray_icon_temp_dir() -> Result<PathBuf, String> {
    let path = if let Some(runtime_dir) = env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("glossa").join("tray-icon")
    } else {
        env::temp_dir().join("glossa").join("tray-icon")
    };

    fs::create_dir_all(&path).map_err(|error| {
        format!(
            "failed to prepare tray temp dir {}: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

fn capture_shortcut(binding: &ShortcutBindingConfig) -> Result<Option<String>, String> {
    let _capture_guard = begin_shortcut_capture();
    let dialog = Dialog::with_buttons(
        Some("Change shortcut"),
        None::<&Window>,
        DialogFlags::MODAL,
        &[("Cancel", ResponseType::Cancel)],
    );
    dialog.set_resizable(false);
    dialog.set_keep_above(true);

    let content = dialog.content_area();
    let container = GtkBox::new(Orientation::Vertical, 8);
    container.set_margin_top(12);
    container.set_margin_bottom(12);
    container.set_margin_start(12);
    container.set_margin_end(12);

    let title = Label::new(Some("Press the new shortcut"));
    title.set_xalign(0.0);
    let detail = Label::new(Some(
        "Glossa will write the binding into GNOME's shortcut storage and restart.\nPress Esc to cancel.",
    ));
    detail.set_xalign(0.0);
    detail.set_line_wrap(true);
    let current_text = binding.current_shortcut.as_deref().map_or(
        "Current shortcut is managed outside config.toml.".to_owned(),
        |shortcut| format!("Current stored shortcut: {shortcut}"),
    );
    let current = Label::new(Some(&current_text));
    current.set_xalign(0.0);
    let preview = Label::new(Some("Waiting for input..."));
    preview.set_xalign(0.0);

    container.pack_start(&title, false, false, 0);
    container.pack_start(&detail, false, false, 0);
    container.pack_start(&current, false, false, 0);
    container.pack_start(&preview, false, false, 0);
    content.pack_start(&container, true, true, 0);

    let captured = Rc::new(RefCell::new(None::<String>));
    let captured_ref = Rc::clone(&captured);
    let preview_ref = preview.clone();
    dialog.connect_key_press_event(move |dialog, event| {
        if event.keyval() == keyconst::Escape {
            dialog.response(ResponseType::Cancel);
            return Propagation::Stop;
        }

        if is_modifier_key(event.keyval()) {
            preview_ref.set_text("Keep holding modifiers and press a non-modifier key.");
            return Propagation::Stop;
        }

        let shortcut = shortcut_from_event(event);
        if shortcut.is_empty() {
            preview_ref.set_text("That key combination is not supported.");
            return Propagation::Stop;
        }

        preview_ref.set_text(&format!("Selected: {shortcut}"));
        *captured_ref.borrow_mut() = Some(shortcut);
        dialog.response(ResponseType::Accept);
        Propagation::Stop
    });

    dialog.show_all();
    dialog.present();
    let response = dialog.run();
    dialog.close();

    if response == ResponseType::Accept {
        Ok(captured.borrow().clone())
    } else {
        Ok(None)
    }
}

fn shortcut_from_event(event: &gdk::EventKey) -> String {
    let modifiers = event.state() & gtk::accelerator_get_default_mod_mask();
    gtk::accelerator_name(event.keyval().into_glib(), modifiers)
        .map(|shortcut| normalize_captured_shortcut(shortcut.as_str()))
        .unwrap_or_default()
}

fn normalize_captured_shortcut(shortcut: &str) -> String {
    shortcut
        .replace("<Primary>", "<Ctrl>")
        .replace("<Control>", "<Ctrl>")
        .replace("<Mod1>", "<Alt>")
}

fn is_modifier_key(keyval: gdk::keys::Key) -> bool {
    matches!(
        keyval,
        keyconst::Shift_L
            | keyconst::Shift_R
            | keyconst::Control_L
            | keyconst::Control_R
            | keyconst::Alt_L
            | keyconst::Alt_R
            | keyconst::Meta_L
            | keyconst::Meta_R
            | keyconst::Super_L
            | keyconst::Super_R
            | keyconst::Hyper_L
            | keyconst::Hyper_R
    )
}

fn write_shortcut_override(binding: &ShortcutBindingConfig, shortcut: &str) -> Result<(), String> {
    let payload = shortcut_override_value(binding, shortcut);
    let output = Command::new("dconf")
        .arg("write")
        .arg(shortcut_override_path())
        .arg(payload)
        .output()
        .map_err(|error| format!("failed to launch dconf: {error}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        Err(format!("dconf write failed with status {}", output.status))
    } else {
        Err(format!("dconf write failed: {stderr}"))
    }
}

fn shortcut_override_path() -> String {
    format!("/org/gnome/settings-daemon/global-shortcuts/{PORTAL_APP_ID}/shortcuts")
}

fn shortcut_override_value(binding: &ShortcutBindingConfig, shortcut: &str) -> String {
    format!(
        "[('{}', {{'shortcuts': <['{}']>, 'description': <'{}'>}})]",
        PORTAL_SHORTCUT_ID,
        escape_dconf_string(shortcut),
        escape_dconf_string(portal_shortcut_description(binding.mode)),
    )
}

fn escape_dconf_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn tray_version_label() -> String {
    format!("version: {}", env!("CARGO_PKG_VERSION"))
}

fn mic_stream_label(active: bool) -> &'static str {
    if active {
        "Mic stream: on"
    } else {
        "Mic stream: off"
    }
}

fn show_confirmation_dialog(title: &str, message: &str) -> bool {
    let dialog = MessageDialog::new(
        None::<&Window>,
        DialogFlags::MODAL,
        MessageType::Question,
        ButtonsType::YesNo,
        message,
    );
    dialog.set_title(title);
    let response = dialog.run();
    dialog.close();

    response == ResponseType::Yes
}

impl Drop for BestEffortTrayPort {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.command_tx.lock() {
            if let Some(sender) = guard.take() {
                let _ = sender.send(TrayCommand::Shutdown);
            }
        }

        if let Ok(mut guard) = self.thread_handle.lock() {
            if let Some(handle) = guard.take() {
                if handle.thread().id() != thread::current().id() {
                    let _ = handle.join();
                }
            }
        }
    }
}

fn is_supported_tray_environment() -> bool {
    is_supported_tray_environment_values(
        env::var("XDG_SESSION_TYPE").ok().as_deref(),
        env::var_os("WAYLAND_DISPLAY")
            .and_then(|value| value.into_string().ok())
            .as_deref(),
        env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        env::var("DESKTOP_SESSION").ok().as_deref(),
    )
}

fn is_supported_tray_environment_values(
    session_type: Option<&str>,
    wayland_display: Option<&str>,
    current_desktop: Option<&str>,
    desktop_session: Option<&str>,
) -> bool {
    let is_wayland =
        session_type.is_some_and(|value| value.eq_ignore_ascii_case("wayland"))
            || wayland_display.is_some();

    let desktop = current_desktop.or(desktop_session).unwrap_or_default();
    desktop.to_ascii_uppercase().contains("GNOME")
        && is_wayland
}

fn has_appindicator_runtime_library() -> bool {
    let ldconfig_has_library = Command::new("ldconfig")
        .arg("-p")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|output| has_appindicator_library_in_ldconfig_output(&output));

    ldconfig_has_library || appindicator_library_exists_in_common_paths()
}

fn has_appindicator_library_in_ldconfig_output(output: &str) -> bool {
    APPINDICATOR_LIBRARY_NAMES
        .iter()
        .any(|name| output.lines().any(|line| line.contains(name)))
}

fn appindicator_library_exists_in_common_paths() -> bool {
    APPINDICATOR_LIBRARY_DIRS.iter().any(|dir| {
        APPINDICATOR_LIBRARY_NAMES
            .iter()
            .any(|name| Path::new(dir).join(name).exists())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        has_appindicator_library_in_ldconfig_output, is_supported_tray_environment_values,
        mic_stream_label, normalize_captured_shortcut, notify_send_arguments,
        shortcut_override_value, tray_version_label, ShortcutBindingConfig,
    };
    use glossa_core::InputMode;

    #[test]
    fn captured_shortcut_should_use_ctrl_alias() {
        assert_eq!(
            normalize_captured_shortcut("<Primary><Shift>r"),
            "<Ctrl><Shift>r"
        );
    }

    #[test]
    fn shortcut_override_value_should_include_shortcut_and_description() {
        let value = shortcut_override_value(
            &ShortcutBindingConfig {
                current_shortcut: Some("<Ctrl><Alt>space".into()),
                mode: InputMode::Toggle,
            },
            "<Ctrl><Shift>r",
        );

        assert!(value.contains("'main'"));
        assert!(value.contains("<['<Ctrl><Shift>r']>"));
        assert!(value.contains("Toggle Glossa recording"));
    }

    #[test]
    fn tray_version_label_should_include_workspace_version() {
        assert_eq!(
            tray_version_label(),
            format!("version: {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn mic_stream_label_should_match_state() {
        assert_eq!(mic_stream_label(true), "Mic stream: on");
        assert_eq!(mic_stream_label(false), "Mic stream: off");
    }

    #[test]
    fn notify_send_arguments_should_include_timeout_and_message() {
        assert_eq!(
            notify_send_arguments("status 429 Too Many Requests"),
            vec![
                "-a",
                "Glossa",
                "-t",
                "3000",
                "Glossa",
                "status 429 Too Many Requests",
            ]
        );
    }

    #[test]
    fn supported_tray_environment_should_allow_fedora_gnome_wayland() {
        assert!(is_supported_tray_environment_values(
            Some("wayland"),
            None,
            Some("GNOME"),
            None,
        ));
    }

    #[test]
    fn appindicator_library_check_should_accept_ayatana_runtime_library() {
        assert!(has_appindicator_library_in_ldconfig_output(
            "libayatana-appindicator3.so.1 (libc6,x86-64) => /lib64/libayatana-appindicator3.so.1"
        ));
    }
}
