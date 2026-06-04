use async_trait::async_trait;
use tokio::{io::AsyncWriteExt, process::Command};

use glossa_app::{ports::PasteBackend, AppError};
use glossa_core::PasteMode;

/// Paste backend backed by the `dotool` binary.
#[derive(Debug, Clone)]
pub struct DotoolPasteBackend {
    command: String,
}

impl DotoolPasteBackend {
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
        }
    }

    #[must_use]
    pub fn command_script(mode: PasteMode) -> &'static str {
        match mode {
            PasteMode::CtrlV => {
                "keydelay 50\nkeydown leftctrl\nkeydown v\nkeyup v\nkeyup leftctrl\n"
            }
            PasteMode::CtrlShiftV => {
                "keydelay 50\nkeydown leftctrl\nkeydown leftshift\nkeydown v\nkeyup v\nkeyup leftshift\nkeyup leftctrl\n"
            }
            PasteMode::ShiftInsert => {
                "keydelay 50\nkeydown leftshift\nkeydown insert\nkeyup insert\nkeyup leftshift\n"
            }
        }
    }
}

#[async_trait]
impl PasteBackend for DotoolPasteBackend {
    async fn paste(&self, mode: PasteMode) -> Result<(), AppError> {
        let mut child = Command::new(&self.command)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| AppError::io("failed to spawn dotool", error))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(Self::command_script(mode).as_bytes())
                .await
                .map_err(|error| AppError::io("failed to write dotool command", error))?;
        }
        let status = child
            .wait()
            .await
            .map_err(|error| AppError::io("failed to wait for dotool", error))?;

        if status.success() {
            Ok(())
        } else {
            Err(AppError::message(format!(
                "dotool exited with status {status}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_mode_should_map_to_expected_dotool_script() {
        let cases = [
            (
                PasteMode::CtrlV,
                "keydelay 20\nkeydown leftctrl\nkeydown v\nkeyup v\nkeyup leftctrl\n",
            ),
            (
                PasteMode::CtrlShiftV,
                "keydelay 20\nkeydown leftctrl\nkeydown leftshift\nkeydown v\nkeyup v\nkeyup leftshift\nkeyup leftctrl\n",
            ),
            (
                PasteMode::ShiftInsert,
                "keydelay 20\nkeydown leftshift\nkeydown insert\nkeyup insert\nkeyup leftshift\n",
            ),
        ];

        for (mode, script) in cases {
            assert_eq!(DotoolPasteBackend::command_script(mode), script);
        }
    }
}
