//! Shared configuration, domain types, and validation for Glossa.

pub mod audio;
pub mod command;
pub mod config;
pub mod error;
pub mod ids;
pub mod paste;
pub mod provider;
pub mod state;
pub mod status;

pub use audio::{AudioFormat, CapturedAudio, RecordSpec};
pub use command::{AppCommand, CommandOrigin};
pub use config::{
    parse_dictionary_terms, AppConfig, AudioConfig, ControlConfig, DictionaryConfig, InputBackend,
    InputConfig, InputMode, LatencyMode, LlmConfig, LogLevel, LoggingConfig, PasteConfig,
    SecretSource, UiConfig, UiTheme, WorkDir,
};
pub use error::CoreError;
pub use ids::SessionId;
pub use paste::PasteMode;
pub use provider::{ProviderConfig, ProviderKind};
pub use state::{AppState, AppStateKind, PastingState, ProcessingState, RecordingState};
pub use status::AppStatus;
