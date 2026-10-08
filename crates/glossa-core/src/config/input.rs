use serde::{Deserialize, Serialize};

/// Input backend used to receive global recording commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputBackend {
    Portal,
    None,
}

/// Recording semantics for the portal shortcut backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputMode {
    Toggle,
    PushToTalk,
}

/// Portal shortcut configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputConfig {
    pub backend: InputBackend,
    #[serde(default)]
    pub shortcut: String,
    pub mode: InputMode,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            backend: InputBackend::Portal,
            shortcut: "<Ctrl><Alt>space".into(),
            mode: InputMode::Toggle,
        }
    }
}
