use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::{AudioFormat, CoreError};

/// Idle microphone stream policy used to reduce cold-start recording latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LatencyMode {
    Off,
    Balanced,
    Instant,
}

/// Audio capture and processing settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioConfig {
    #[serde(default = "default_audio_enabled")]
    pub enabled: bool,
    pub work_dir: WorkDir,
    pub format: AudioFormat,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub trim_silence: bool,
    pub trim_threshold: u16,
    pub min_duration_ms: u64,
    pub max_duration_sec: u32,
    #[serde(default)]
    pub persist_audio: bool,
    #[serde(default = "default_latency_mode")]
    pub latency_mode: LatencyMode,
    #[serde(default = "default_keepalive_after_stop_seconds")]
    pub keepalive_after_stop_seconds: u64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: default_audio_enabled(),
            work_dir: WorkDir::Auto,
            format: AudioFormat::Wav,
            sample_rate_hz: 16_000,
            channels: 1,
            trim_silence: true,
            trim_threshold: 500,
            min_duration_ms: 150,
            max_duration_sec: 120,
            persist_audio: false,
            latency_mode: default_latency_mode(),
            keepalive_after_stop_seconds: default_keepalive_after_stop_seconds(),
        }
    }
}

impl AudioConfig {
    pub(crate) fn validate(&self) -> Result<(), CoreError> {
        if self.sample_rate_hz == 0 {
            return Err(CoreError::InvalidConfig(
                "audio.sample_rate_hz must be greater than zero".into(),
            ));
        }
        if self.channels == 0 {
            return Err(CoreError::InvalidConfig(
                "audio.channels must be greater than zero".into(),
            ));
        }
        if self.min_duration_ms == 0 {
            return Err(CoreError::InvalidConfig(
                "audio.min_duration_ms must be greater than zero".into(),
            ));
        }
        if self.max_duration_sec == 0 {
            return Err(CoreError::InvalidConfig(
                "audio.max_duration_sec must be greater than zero".into(),
            ));
        }
        if self.keepalive_after_stop_seconds == 0 {
            return Err(CoreError::InvalidConfig(
                "audio.keepalive_after_stop_seconds must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

fn default_audio_enabled() -> bool {
    true
}

fn default_latency_mode() -> LatencyMode {
    LatencyMode::Balanced
}

fn default_keepalive_after_stop_seconds() -> u64 {
    60
}

/// Working directory selection for temporary recordings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum WorkDir {
    Auto,
    Path(Utf8PathBuf),
}

impl TryFrom<String> for WorkDir {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == "auto" {
            Ok(Self::Auto)
        } else if value.trim().is_empty() {
            Err("audio.work_dir must not be empty".into())
        } else {
            Ok(Self::Path(Utf8PathBuf::from(value)))
        }
    }
}

impl From<WorkDir> for String {
    fn from(value: WorkDir) -> Self {
        match value {
            WorkDir::Auto => "auto".into(),
            WorkDir::Path(path) => path.into_string(),
        }
    }
}
