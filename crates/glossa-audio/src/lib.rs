//! Audio capture, encoding, trimming, and cue playback for Glossa.

pub mod capture;
pub mod cue;
pub mod encoder;
pub mod trim;
pub mod wav;

pub use self::capture::CpalAudioCapture;
pub use self::cue::{CuePlayerBackend, RodioCuePlayer};
pub use self::encoder::FfmpegAudioEncoder;
pub use self::trim::WavSilenceTrimmer;
