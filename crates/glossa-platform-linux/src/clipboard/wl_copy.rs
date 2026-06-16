use async_trait::async_trait;
use tokio::{io::AsyncWriteExt, process::Command};

use glossa_app::{
    ports::{ClipboardSnapshot, ClipboardWriter},
    AppError,
};

/// Clipboard writer backed by the `wl-copy` binary.
#[derive(Debug, Clone)]
pub struct WlCopyClipboard {
    copy_command: String,
    paste_command: String,
}

impl WlCopyClipboard {
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            copy_command: command.into(),
            paste_command: "wl-paste".into(),
        }
    }

    async fn copy_bytes(&self, mime_type: Option<&str>, bytes: &[u8]) -> Result<(), AppError> {
        let mut command = Command::new(&self.copy_command);
        if let Some(mime_type) = mime_type {
            command.arg("--type").arg(mime_type);
        }

        let mut child = command
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| AppError::io("failed to spawn wl-copy", error))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(bytes)
                .await
                .map_err(|error| AppError::io("failed to write clipboard data", error))?;
        }

        let status = child
            .wait()
            .await
            .map_err(|error| AppError::io("failed to wait for wl-copy", error))?;
        if status.success() {
            Ok(())
        } else {
            Err(AppError::message(format!(
                "wl-copy exited with status {status}"
            )))
        }
    }

    fn select_mime_type(types: &str) -> Option<String> {
        let mut fallback = None;
        let mut plain_text = None;
        for mime_type in types.lines() {
            let mime_type = mime_type.trim();
            if mime_type.is_empty() {
                continue;
            }
            if fallback.is_none() {
                fallback = Some(mime_type.to_owned());
            }

            if matches!(mime_type, "x-special/gnome-copied-files" | "text/uri-list")
                || mime_type.starts_with("image/")
            {
                return Some(mime_type.to_owned());
            }

            if mime_type == "text/plain;charset=utf-8" {
                return Some(mime_type.to_owned());
            }
            if mime_type == "text/plain" && plain_text.is_none() {
                plain_text = Some(mime_type.to_owned());
            }
        }
        plain_text.or(fallback)
    }

    fn is_empty_clipboard_error(stderr: &[u8]) -> bool {
        let stderr = String::from_utf8_lossy(stderr).to_lowercase();
        stderr.contains("nothing is copied") || stderr.contains("no selection")
    }
}

#[async_trait]
impl ClipboardWriter for WlCopyClipboard {
    async fn snapshot(&self) -> Result<Option<ClipboardSnapshot>, AppError> {
        let output = Command::new(&self.paste_command)
            .arg("--list-types")
            .output()
            .await
            .map_err(|error| AppError::io("failed to spawn wl-paste", error))?;
        if !output.status.success() {
            return if Self::is_empty_clipboard_error(&output.stderr) {
                Ok(None)
            } else {
                Err(AppError::message(format!(
                    "wl-paste --list-types exited with status {}",
                    output.status
                )))
            };
        }

        let types = String::from_utf8(output.stdout).map_err(|error| {
            AppError::message(format!("wl-paste returned invalid UTF-8: {error}"))
        })?;
        let Some(mime_type) = Self::select_mime_type(&types) else {
            return Ok(None);
        };

        let output = Command::new(&self.paste_command)
            .arg("--type")
            .arg(&mime_type)
            .output()
            .await
            .map_err(|error| AppError::io("failed to spawn wl-paste", error))?;
        if output.status.success() {
            Ok(Some(ClipboardSnapshot::new(mime_type, output.stdout)))
        } else {
            Err(AppError::message(format!(
                "wl-paste exited with status {}",
                output.status
            )))
        }
    }

    async fn set_text(&self, text: &str) -> Result<(), AppError> {
        self.copy_bytes(None, text.as_bytes()).await
    }

    async fn restore(&self, snapshot: ClipboardSnapshot) -> Result<(), AppError> {
        self.copy_bytes(Some(&snapshot.mime_type), &snapshot.bytes)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::WlCopyClipboard;

    #[test]
    fn select_mime_type_should_prefer_plain_text_over_rich_text() {
        let selected = WlCopyClipboard::select_mime_type(
            "text/html\ntext/plain;charset=utf-8\napplication/x-private\n",
        );

        assert_eq!(selected, Some("text/plain;charset=utf-8".to_owned()));
    }

    #[test]
    fn select_mime_type_should_fall_back_to_non_text() {
        let selected = WlCopyClipboard::select_mime_type("image/png\n");

        assert_eq!(selected, Some("image/png".to_owned()));
    }

    #[test]
    fn select_mime_type_should_fall_back_to_plain_text() {
        let selected = WlCopyClipboard::select_mime_type("text/plain;charset=utf-8\n");

        assert_eq!(selected, Some("text/plain;charset=utf-8".to_owned()));
    }
}
