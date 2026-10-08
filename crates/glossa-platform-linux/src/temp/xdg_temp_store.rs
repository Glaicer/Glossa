use std::env;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use tokio::fs;

use glossa_app::{ports::TempStore, AppError};
use glossa_core::{AudioConfig, AudioFormat, SessionId, WorkDir};

/// Temporary storage rooted in `$XDG_RUNTIME_DIR` or a configured directory.
#[derive(Debug, Clone)]
pub struct XdgTempStore {
    base_dir: Utf8PathBuf,
    persist_audio: bool,
}

impl XdgTempStore {
    pub fn from_audio_config(config: &AudioConfig) -> Result<Self, AppError> {
        let base_dir = match &config.work_dir {
            WorkDir::Auto => {
                let runtime_dir = env::var("XDG_RUNTIME_DIR")
                    .ok()
                    .map(Utf8PathBuf::from)
                    .or_else(|| env::temp_dir().to_str().map(Utf8PathBuf::from))
                    .ok_or_else(|| {
                        AppError::message("could not determine a runtime directory for temp files")
                    })?;
                runtime_dir.join("glossa")
            }
            WorkDir::Path(path) => path.clone(),
        };

        Ok(Self {
            base_dir,
            persist_audio: config.persist_audio,
        })
    }

    #[must_use]
    pub fn base_dir(&self) -> &Utf8Path {
        self.base_dir.as_path()
    }

    fn recording_path(&self, session_id: SessionId, format: AudioFormat) -> Utf8PathBuf {
        self.base_dir
            .join(format!("glossa-{session_id}.{}", format.extension()))
    }

    async fn remove_matching_files(
        &self,
        mut should_remove: impl FnMut(&str) -> bool,
    ) -> Result<(), AppError> {
        fs::create_dir_all(self.base_dir())
            .await
            .map_err(|error| AppError::io("failed to create temp directory", error))?;
        let mut entries = fs::read_dir(self.base_dir())
            .await
            .map_err(|error| AppError::io("failed to read temp directory", error))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| AppError::io("failed to iterate temp directory", error))?
        {
            let path = entry.path();
            let should_remove_file = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(&mut should_remove);
            if should_remove_file {
                let _ = fs::remove_file(path).await;
            }
        }
        Ok(())
    }

    async fn remove_session_files(&self, session_id: SessionId) -> Result<(), AppError> {
        let prefix = format!("glossa-{session_id}");
        self.remove_matching_files(|name| name.starts_with(&prefix))
            .await
    }
}

#[async_trait]
impl TempStore for XdgTempStore {
    async fn create_recording_path(
        &self,
        session_id: SessionId,
        format: AudioFormat,
    ) -> Result<Utf8PathBuf, AppError> {
        fs::create_dir_all(self.base_dir())
            .await
            .map_err(|error| AppError::io("failed to create temp directory", error))?;
        Ok(self.recording_path(session_id, format))
    }

    async fn cleanup_session(&self, session_id: SessionId) -> Result<(), AppError> {
        if self.persist_audio {
            return Ok(());
        }

        self.remove_session_files(session_id).await
    }

    async fn purge_session(&self, session_id: SessionId) -> Result<(), AppError> {
        self.remove_session_files(session_id).await
    }

    async fn cleanup_stale_files(&self) -> Result<(), AppError> {
        if self.persist_audio {
            return Ok(());
        }

        self.remove_matching_files(|name| name.starts_with("glossa-"))
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs as std_fs};

    use glossa_core::{AudioConfig, SessionId, WorkDir};

    use super::*;

    fn test_store(persist_audio: bool) -> XdgTempStore {
        let base_dir = env::temp_dir().join(format!("glossa-temp-store-test-{}", SessionId::new()));
        let base_dir =
            Utf8PathBuf::from_path_buf(base_dir).expect("temp path should be valid utf-8");
        let config = AudioConfig {
            work_dir: WorkDir::Path(base_dir),
            persist_audio,
            ..AudioConfig::default()
        };

        XdgTempStore::from_audio_config(&config).expect("temp store should build")
    }

    #[tokio::test]
    async fn cleanup_session_should_keep_recordings_when_persist_audio_is_enabled() {
        let store = test_store(true);
        let session_id = SessionId::new();
        let path = store
            .create_recording_path(session_id, AudioFormat::Wav)
            .await
            .expect("recording path should be created");
        fs::write(&path, b"audio")
            .await
            .expect("test recording should be written");

        store
            .cleanup_session(session_id)
            .await
            .expect("cleanup should succeed");

        assert!(path.as_std_path().exists());
        std_fs::remove_dir_all(store.base_dir()).expect("test temp dir should be removed");
    }

    #[tokio::test]
    async fn cleanup_stale_files_should_keep_recordings_when_persist_audio_is_enabled() {
        let store = test_store(true);
        let session_id = SessionId::new();
        let path = store
            .create_recording_path(session_id, AudioFormat::Wav)
            .await
            .expect("recording path should be created");
        fs::write(&path, b"audio")
            .await
            .expect("test recording should be written");

        store
            .cleanup_stale_files()
            .await
            .expect("stale cleanup should succeed");

        assert!(path.as_std_path().exists());
        std_fs::remove_dir_all(store.base_dir()).expect("test temp dir should be removed");
    }

    #[tokio::test]
    async fn purge_session_should_delete_recordings_when_persist_audio_is_enabled() {
        let store = test_store(true);
        let session_id = SessionId::new();
        let path = store
            .create_recording_path(session_id, AudioFormat::Wav)
            .await
            .expect("recording path should be created");
        fs::write(&path, b"audio")
            .await
            .expect("test recording should be written");

        store
            .purge_session(session_id)
            .await
            .expect("purge should succeed");

        assert!(!path.as_std_path().exists());
        std_fs::remove_dir_all(store.base_dir()).expect("test temp dir should be removed");
    }
}
