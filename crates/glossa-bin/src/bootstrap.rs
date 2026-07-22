use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use tokio::fs;
use tracing_subscriber::EnvFilter;

use glossa_app::{ports::TrayPort, AppActor, AppDependencies};
use glossa_audio::{CpalAudioCapture, CuePlayerBackend, WavSilenceTrimmer};
use glossa_core::{AppConfig, LogLevel};
use glossa_platform_linux::{
    clipboard::WlCopyClipboard, paste::DotoolPasteBackend, secret, temp::XdgTempStore,
    tray::BestEffortTrayPort,
};
use glossa_stt::{build_client, build_text_enhancer};

/// Loads the Glossa config from disk and validates it.
pub async fn load_config(path: &PathBuf) -> anyhow::Result<AppConfig> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read config from {}", path.display()))?;
    let config = AppConfig::from_toml_str(&content)?;
    Ok(config)
}

/// Loads a config from disk when provided, otherwise falls back to the default config.
pub async fn load_config_or_default(path: Option<PathBuf>) -> anyhow::Result<AppConfig> {
    match path {
        Some(path) => load_config(&path).await,
        None => Ok(AppConfig::default()),
    }
}

/// Initializes tracing based on the config logging level.
pub fn init_tracing(config: &AppConfig) -> anyhow::Result<()> {
    let filter = match config.logging.level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    };
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_target(true)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|error| anyhow!("failed to initialize tracing: {error}"))
}

/// Builds the app actor and its dependencies from a validated config.
pub fn build_tray(config_path: &PathBuf, config: &AppConfig) -> Arc<BestEffortTrayPort> {
    Arc::new(BestEffortTrayPort::new(config_path.clone(), config))
}

/// Builds the app actor and its dependencies from a validated config.
pub fn build_actor(
    config: AppConfig,
    tray: Arc<dyn TrayPort>,
) -> anyhow::Result<(AppActor, glossa_app::AppHandle)> {
    let provider_api_key = secret::resolve(&config.provider.api_key)?;
    let llm_api_key = if config.llm.enabled {
        secret::resolve(&config.llm.api_key)?
    } else {
        String::new()
    };
    let temp_store = Arc::new(XdgTempStore::from_audio_config(&config.audio)?);
    let deps = AppDependencies {
        audio_capture: Arc::new(CpalAudioCapture::new()),
        trimmer: Arc::new(WavSilenceTrimmer::new(config.audio.trim_threshold)),
        cue_player: Arc::new(CuePlayerBackend::from_config(
            config.audio.enabled,
            config.ui.start_sound.clone(),
            config.ui.stop_sound.clone(),
        )),
        stt_client: build_client(&config, provider_api_key),
        text_enhancer: build_text_enhancer(&config, llm_api_key),
        clipboard: Arc::new(WlCopyClipboard::new(config.paste.clipboard_command.clone())),
        paste: Arc::new(DotoolPasteBackend::new(config.paste.type_command.clone())),
        tray,
        temp_store,
    };
    Ok(AppActor::new(config, deps))
}

/// Resolves the IPC socket path from an optional config file.
pub async fn resolve_socket_path(
    config_path: Option<PathBuf>,
) -> anyhow::Result<camino::Utf8PathBuf> {
    let config = load_config_or_default(config_path).await?;
    Ok(config.control.socket_path.resolve()?)
}
