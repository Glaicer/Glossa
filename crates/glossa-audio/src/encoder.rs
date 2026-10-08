use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;

use glossa_app::{ports::AudioEncoder, AppError};
use glossa_core::{AudioFormat, CapturedAudio};

/// Converts processed WAV audio using the system FFmpeg executable.
#[derive(Debug, Clone, Copy)]
pub struct FfmpegAudioEncoder;

#[async_trait]
impl AudioEncoder for FfmpegAudioEncoder {
    async fn encode(
        &self,
        input: &CapturedAudio,
        format: AudioFormat,
    ) -> Result<CapturedAudio, AppError> {
        if format == AudioFormat::Wav {
            return Ok(input.clone());
        }

        let path = input.path.with_extension(format.extension());
        let codec = match format {
            AudioFormat::Wav => "pcm_s16le",
            AudioFormat::Mp3 => "libmp3lame",
            AudioFormat::M4a | AudioFormat::Aac => "aac",
            AudioFormat::Flac => "flac",
            AudioFormat::Ogg => "libvorbis",
        };
        let output = Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(input.path.as_std_path())
            .args(["-vn", "-c:a", codec])
            .arg(path.as_std_path())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|error| {
                AppError::io(
                    "failed to run ffmpeg; install FFmpeg for non-WAV audio formats",
                    error,
                )
            })?;
        if !output.status.success() {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(AppError::message(format!(
                "failed to encode {} audio with ffmpeg: {}",
                format.extension(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        Ok(CapturedAudio {
            path,
            ..input.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use glossa_core::SessionId;

    use super::*;

    fn test_audio(path: Utf8PathBuf) -> CapturedAudio {
        CapturedAudio {
            session_id: SessionId::new(),
            path,
            duration_ms: 1000,
            sample_rate_hz: 16000,
            channels: 1,
        }
    }

    #[tokio::test]
    async fn wav_should_pass_through_without_running_ffmpeg() {
        let audio = test_audio("/nonexistent/recording.wav".into());
        let encoded = FfmpegAudioEncoder
            .encode(&audio, AudioFormat::Wav)
            .await
            .expect("WAV needs no conversion");
        assert_eq!(encoded, audio);
    }

    #[tokio::test]
    async fn ffmpeg_should_encode_all_compressed_formats() {
        let directory = std::env::temp_dir().join(format!("glossa-encoder-{}", SessionId::new()));
        tokio::fs::create_dir_all(&directory)
            .await
            .expect("create directory");
        let path = Utf8PathBuf::from_path_buf(directory.join("recording.wav")).expect("UTF-8 path");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .expect("create WAV");
        for index in 0..16000 {
            let sample =
                ((index as f32 * 440.0 * std::f32::consts::TAU / 16000.0).sin() * 8000.0) as i16;
            writer.write_sample(sample).expect("write sample");
        }
        writer.finalize().expect("finalize WAV");
        let audio = test_audio(path);

        for (format, signature) in [
            (AudioFormat::Mp3, b"ID3".as_slice()),
            (AudioFormat::M4a, b"ftyp".as_slice()),
            (AudioFormat::Flac, b"fLaC".as_slice()),
            (AudioFormat::Ogg, b"OggS".as_slice()),
            (AudioFormat::Aac, b"\xff\xf1".as_slice()),
        ] {
            let encoded = FfmpegAudioEncoder
                .encode(&audio, format)
                .await
                .expect("encode audio");
            assert_eq!(encoded.path.extension(), Some(format.extension()));
            assert_eq!(encoded.session_id, audio.session_id);
            assert_eq!(encoded.duration_ms, audio.duration_ms);
            assert_eq!(encoded.sample_rate_hz, audio.sample_rate_hz);
            assert_eq!(encoded.channels, audio.channels);
            let bytes = tokio::fs::read(&encoded.path)
                .await
                .expect("read encoded audio");
            let offset = if format == AudioFormat::M4a { 4 } else { 0 };
            assert_eq!(&bytes[offset..offset + signature.len()], signature);
            let decoded = Command::new("ffmpeg")
                .args(["-nostdin", "-loglevel", "error", "-i"])
                .arg(encoded.path.as_std_path())
                .args(["-f", "s16le", "-acodec", "pcm_s16le", "pipe:1"])
                .output()
                .await
                .expect("decode audio");
            assert!(
                decoded.status.success(),
                "{format:?}: {}",
                String::from_utf8_lossy(&decoded.stderr)
            );
            assert!(
                (30000..=40000).contains(&decoded.stdout.len()),
                "{format:?}: decoded duration"
            );
        }
        tokio::fs::remove_dir_all(directory)
            .await
            .expect("remove test directory");
    }

    #[tokio::test]
    async fn invalid_audio_should_fail_without_leaving_an_encoded_file() {
        let directory = std::env::temp_dir().join(format!("glossa-encoder-{}", SessionId::new()));
        tokio::fs::create_dir_all(&directory)
            .await
            .expect("create directory");
        let path = Utf8PathBuf::from_path_buf(directory.join("invalid.wav")).expect("UTF-8 path");
        tokio::fs::write(&path, b"not audio")
            .await
            .expect("write invalid input");
        let audio = test_audio(path);
        let error = FfmpegAudioEncoder
            .encode(&audio, AudioFormat::Flac)
            .await
            .expect_err("encoding should fail");
        assert!(error.to_string().contains("failed to encode flac"));
        assert!(!audio.path.with_extension("flac").exists());
        assert!(audio.path.exists());
        tokio::fs::remove_dir_all(directory)
            .await
            .expect("remove test directory");
    }
}
