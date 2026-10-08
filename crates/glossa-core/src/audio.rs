use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::SessionId;

/// Supported on-disk audio formats for captured recordings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioFormat {
    Wav,
    Mp3,
    M4a,
    Flac,
    Ogg,
    Aac,
}

impl AudioFormat {
    /// Returns the file extension associated with the format.
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::M4a => "m4a",
            Self::Flac => "flac",
            Self::Ogg => "ogg",
            Self::Aac => "aac",
        }
    }

    /// Parses a supported audio file extension.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "wav" => Some(Self::Wav),
            "mp3" => Some(Self::Mp3),
            "m4a" => Some(Self::M4a),
            "flac" => Some(Self::Flac),
            "ogg" => Some(Self::Ogg),
            "aac" => Some(Self::Aac),
            _ => None,
        }
    }

    /// Returns the MIME type used for transcription uploads.
    #[must_use]
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Mp3 => "audio/mpeg",
            Self::M4a => "audio/mp4",
            Self::Flac => "audio/flac",
            Self::Ogg => "audio/ogg",
            Self::Aac => "audio/aac",
        }
    }
}

/// Recording parameters used by the audio capture backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordSpec {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub format: AudioFormat,
    pub max_duration_sec: u32,
}

/// Audio file produced by a completed capture session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedAudio {
    pub session_id: SessionId,
    pub path: Utf8PathBuf,
    pub duration_ms: u64,
    pub sample_rate_hz: u32,
    pub channels: u16,
}
