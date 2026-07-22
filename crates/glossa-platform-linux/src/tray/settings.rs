use std::{collections::BTreeSet, fs, path::Path};

use glossa_app::AppError;
use glossa_core::{
    AppConfig, InputBackend, InputMode, LatencyMode, PasteMode, ProviderKind, UiTheme,
};

use crate::secret::{self, LLM_SLOT, PROVIDER_SLOT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SettingsValues {
    pub(super) input_backend: InputBackend,
    pub(super) input_mode: InputMode,
    pub(super) enable_cli: bool,
    pub(super) provider_kind: ProviderKind,
    pub(super) provider_base_url: String,
    pub(super) provider_model: String,
    pub(super) provider_api_key: String,
    pub(super) llm_enabled: bool,
    pub(super) llm_base_url: String,
    pub(super) llm_model: String,
    pub(super) llm_api_key: String,
    pub(super) paste_mode: PasteMode,
    pub(super) append_space: bool,
    pub(super) latency_mode: LatencyMode,
    pub(super) keepalive_after_stop_seconds: u64,
    pub(super) ui_theme: UiTheme,
}

impl SettingsValues {
    pub(super) fn from_config(config: &AppConfig) -> Self {
        Self {
            input_backend: config.input.backend,
            input_mode: config.input.mode,
            enable_cli: config.control.enable_cli,
            provider_kind: config.provider.kind,
            provider_base_url: config.provider.base_url.clone().unwrap_or_default(),
            provider_model: config.provider.model.clone(),
            provider_api_key: String::from(config.provider.api_key.clone()),
            llm_enabled: config.llm.enabled,
            llm_base_url: config.llm.base_url.clone(),
            llm_model: config.llm.model.clone(),
            llm_api_key: String::from(config.llm.api_key.clone()),
            paste_mode: config.paste.mode,
            append_space: config.paste.append_space,
            latency_mode: config.audio.latency_mode,
            keepalive_after_stop_seconds: config.audio.keepalive_after_stop_seconds,
            ui_theme: config.ui.theme,
        }
    }
}

pub(super) fn apply_settings_to_config(
    source: &str,
    settings: &SettingsValues,
) -> Result<String, AppError> {
    let updates = build_updates(settings);
    let mut seen = BTreeSet::new();
    let mut current_section = None::<&str>;
    let mut updated = String::with_capacity(source.len() + 128);

    for raw_line in lines_with_endings(source) {
        let (line, newline) = split_line_ending(raw_line);

        if let Some(section) = parse_section_name(line) {
            maybe_insert_missing_supported_keys(current_section, &updates, &mut seen, &mut updated);
            current_section = Some(section);
            updated.push_str(line);
            updated.push_str(newline);
            continue;
        }

        let replacement = current_section.and_then(|section| {
            updates
                .iter()
                .find(|update| update.section == section && line_matches_key(line, update.key))
        });

        if let Some(update) = replacement {
            updated.push_str(&replace_line_value(line, &update.value));
            updated.push_str(newline);
            seen.insert((update.section, update.key));
            continue;
        }

        updated.push_str(line);
        updated.push_str(newline);
    }

    maybe_insert_missing_supported_keys(current_section, &updates, &mut seen, &mut updated);
    insert_missing_supported_sections(&updates, &mut seen, &mut updated);

    let missing: Vec<String> = updates
        .iter()
        .filter(|update| !seen.contains(&(update.section, update.key)))
        .map(|update| format!("[{}].{}", update.section, update.key))
        .collect();
    if !missing.is_empty() {
        return Err(AppError::message(format!(
            "config.toml is missing required setting lines: {}",
            missing.join(", ")
        )));
    }

    AppConfig::from_toml_str(&updated)?;
    Ok(updated)
}

fn maybe_insert_missing_supported_keys(
    current_section: Option<&str>,
    updates: &[SettingUpdate],
    seen: &mut BTreeSet<(&'static str, &'static str)>,
    updated: &mut String,
) {
    let missing: Vec<&SettingUpdate> = updates
        .iter()
        .filter(|update| {
            current_section == Some(update.section)
                && is_supported_missing_key(update)
                && !seen.contains(&(update.section, update.key))
        })
        .collect();
    if missing.is_empty() {
        return;
    }

    let trailing_blank_lines = take_trailing_blank_lines(updated);
    for update in missing {
        insert_missing_setting(update, seen, updated);
    }
    updated.push_str(&trailing_blank_lines);
}

fn insert_missing_setting(
    update: &SettingUpdate,
    seen: &mut BTreeSet<(&'static str, &'static str)>,
    updated: &mut String,
) {
    updated.push_str(update.key);
    updated.push_str(" = ");
    updated.push_str(&update.value);
    updated.push('\n');
    seen.insert((update.section, update.key));
}

fn is_supported_missing_key(update: &SettingUpdate) -> bool {
    matches!(
        (update.section, update.key),
        ("audio", "latency_mode")
            | ("audio", "keepalive_after_stop_seconds")
            | ("paste", "append_space")
            | ("LLM", "enabled")
            | ("LLM", "base_url")
            | ("LLM", "model")
            | ("LLM", "api_key")
    )
}

fn insert_missing_supported_sections(
    updates: &[SettingUpdate],
    seen: &mut BTreeSet<(&'static str, &'static str)>,
    updated: &mut String,
) {
    insert_missing_section("LLM", updates, seen, updated);
}

fn insert_missing_section(
    section: &'static str,
    updates: &[SettingUpdate],
    seen: &mut BTreeSet<(&'static str, &'static str)>,
    updated: &mut String,
) {
    let section_updates: Vec<&SettingUpdate> = updates
        .iter()
        .filter(|update| update.section == section)
        .collect();
    if section_updates.is_empty()
        || section_updates
            .iter()
            .any(|update| seen.contains(&(update.section, update.key)))
    {
        return;
    }

    if !updated.is_empty() {
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        if !updated.ends_with("\n\n") {
            updated.push('\n');
        }
    }
    updated.push('[');
    updated.push_str(section);
    updated.push_str("]\n");
    for update in section_updates {
        insert_missing_setting(update, seen, updated);
    }
}

fn take_trailing_blank_lines(updated: &mut String) -> String {
    let mut trailing = String::new();

    loop {
        let Some(line_end) = updated.rfind('\n') else {
            break;
        };
        let line_start = updated[..line_end].rfind('\n').map_or(0, |index| index + 1);
        let line = &updated[line_start..line_end];

        if !line.trim().is_empty() {
            break;
        }

        let removed = updated.split_off(line_start);
        trailing.insert_str(0, &removed);
    }

    trailing
}

pub(super) fn write_settings_to_config(
    path: &Path,
    settings: &mut SettingsValues,
) -> Result<(), AppError> {
    settings.provider_api_key = secret::secure_input(&settings.provider_api_key, PROVIDER_SLOT)?;
    settings.llm_api_key = secret::secure_input(&settings.llm_api_key, LLM_SLOT)?;
    let source = fs::read_to_string(path)
        .map_err(|error| AppError::io("failed to read config.toml", error))?;
    let updated = apply_settings_to_config(&source, settings)?;
    fs::write(path, updated).map_err(|error| AppError::io("failed to write config.toml", error))
}

pub(super) fn input_backend_id(value: InputBackend) -> &'static str {
    match value {
        InputBackend::Portal => "portal",
        InputBackend::None => "none",
    }
}

pub(super) fn parse_input_backend(value: &str) -> Option<InputBackend> {
    match value {
        "portal" => Some(InputBackend::Portal),
        "none" => Some(InputBackend::None),
        _ => None,
    }
}

pub(super) fn input_mode_id(value: InputMode) -> &'static str {
    match value {
        InputMode::PushToTalk => "push-to-talk",
        InputMode::Toggle => "toggle",
    }
}

pub(super) fn parse_input_mode(value: &str) -> Option<InputMode> {
    match value {
        "push-to-talk" => Some(InputMode::PushToTalk),
        "toggle" => Some(InputMode::Toggle),
        _ => None,
    }
}

pub(super) fn provider_kind_id(value: ProviderKind) -> &'static str {
    match value {
        ProviderKind::Groq => "groq",
        ProviderKind::OpenAi => "openai",
        ProviderKind::OpenAiCompatible => "openai-compatible",
    }
}

pub(super) fn parse_provider_kind(value: &str) -> Option<ProviderKind> {
    match value {
        "groq" => Some(ProviderKind::Groq),
        "openai" => Some(ProviderKind::OpenAi),
        "openai-compatible" => Some(ProviderKind::OpenAiCompatible),
        _ => None,
    }
}

pub(super) fn paste_mode_id(value: PasteMode) -> &'static str {
    match value {
        PasteMode::CtrlV => "ctrl-v",
        PasteMode::CtrlShiftV => "ctrl-shift-v",
        PasteMode::ShiftInsert => "shift-insert",
    }
}

pub(super) fn parse_paste_mode(value: &str) -> Option<PasteMode> {
    match value {
        "ctrl-v" => Some(PasteMode::CtrlV),
        "ctrl-shift-v" => Some(PasteMode::CtrlShiftV),
        "shift-insert" => Some(PasteMode::ShiftInsert),
        _ => None,
    }
}

pub(super) fn latency_mode_id(value: LatencyMode) -> &'static str {
    match value {
        LatencyMode::Off => "off",
        LatencyMode::Balanced => "balanced",
        LatencyMode::Instant => "instant",
    }
}

pub(super) fn parse_latency_mode(value: &str) -> Option<LatencyMode> {
    match value {
        "off" => Some(LatencyMode::Off),
        "balanced" => Some(LatencyMode::Balanced),
        "instant" => Some(LatencyMode::Instant),
        _ => None,
    }
}

pub(super) fn ui_theme_id(value: UiTheme) -> &'static str {
    match value {
        UiTheme::Dark => "dark",
        UiTheme::Light => "light",
    }
}

pub(super) fn parse_ui_theme(value: &str) -> Option<UiTheme> {
    match value {
        "dark" => Some(UiTheme::Dark),
        "light" => Some(UiTheme::Light),
        _ => None,
    }
}

fn build_updates(settings: &SettingsValues) -> Vec<SettingUpdate> {
    vec![
        SettingUpdate::new(
            "input",
            "backend",
            quoted(input_backend_id(settings.input_backend)),
        ),
        SettingUpdate::new("input", "mode", quoted(input_mode_id(settings.input_mode))),
        SettingUpdate::new("control", "enable_cli", settings.enable_cli.to_string()),
        SettingUpdate::new(
            "provider",
            "kind",
            quoted(provider_kind_id(settings.provider_kind)),
        ),
        SettingUpdate::new("provider", "base_url", quoted(&settings.provider_base_url)),
        SettingUpdate::new("provider", "model", quoted(&settings.provider_model)),
        SettingUpdate::new("provider", "api_key", quoted(&settings.provider_api_key)),
        SettingUpdate::new("LLM", "enabled", settings.llm_enabled.to_string()),
        SettingUpdate::new("LLM", "base_url", quoted(&settings.llm_base_url)),
        SettingUpdate::new("LLM", "model", quoted(&settings.llm_model)),
        SettingUpdate::new("LLM", "api_key", quoted(&settings.llm_api_key)),
        SettingUpdate::new(
            "audio",
            "latency_mode",
            quoted(latency_mode_id(settings.latency_mode)),
        ),
        SettingUpdate::new(
            "audio",
            "keepalive_after_stop_seconds",
            settings.keepalive_after_stop_seconds.to_string(),
        ),
        SettingUpdate::new("paste", "mode", quoted(paste_mode_id(settings.paste_mode))),
        SettingUpdate::new("paste", "append_space", settings.append_space.to_string()),
        SettingUpdate::new("ui", "theme", quoted(ui_theme_id(settings.ui_theme))),
    ]
}

fn quoted(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');

    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0C}' => escaped.push_str("\\f"),
            _ => escaped.push(ch),
        }
    }

    escaped.push('"');
    escaped
}

fn lines_with_endings(input: &str) -> Vec<&str> {
    if input.is_empty() {
        return Vec::new();
    }

    input.split_inclusive('\n').collect()
}

fn split_line_ending(line: &str) -> (&str, &str) {
    line.strip_suffix('\n')
        .map_or((line, ""), |line| (line, "\n"))
}

fn parse_section_name(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return None;
    }

    trimmed.strip_prefix('[')?.strip_suffix(']')
}

fn line_matches_key(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }

    let Some(eq_index) = trimmed.find('=') else {
        return false;
    };

    trimmed[..eq_index].trim_end() == key
}

fn replace_line_value(line: &str, new_value: &str) -> String {
    let trimmed = line.trim_start();
    let leading_offset = line.len() - trimmed.len();
    let eq_index = leading_offset
        + trimmed
            .find('=')
            .expect("matched key lines must contain '='");
    let prefix = &line[..=eq_index];
    let after_eq = &line[eq_index + 1..];
    let comment_index = find_comment_start(after_eq).unwrap_or(after_eq.len());
    let value_segment = &after_eq[..comment_index];
    let leading_ws_len =
        value_segment.len() - value_segment.trim_start_matches(char::is_whitespace).len();
    let trailing_ws_len =
        value_segment.len() - value_segment.trim_end_matches(char::is_whitespace).len();
    let leading_ws = &value_segment[..leading_ws_len];
    let trailing_ws = &value_segment[value_segment.len() - trailing_ws_len..];
    let suffix = &after_eq[comment_index..];

    format!("{prefix}{leading_ws}{new_value}{trailing_ws}{suffix}")
}

fn find_comment_start(value: &str) -> Option<usize> {
    let mut in_basic_string = false;
    let mut in_literal_string = false;
    let mut escape_next = false;

    for (index, ch) in value.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match ch {
            '\\' if in_basic_string => escape_next = true,
            '"' if !in_literal_string => in_basic_string = !in_basic_string,
            '\'' if !in_basic_string => in_literal_string = !in_literal_string,
            '#' if !in_basic_string && !in_literal_string => return Some(index),
            _ => {}
        }
    }

    None
}

struct SettingUpdate {
    section: &'static str,
    key: &'static str,
    value: String,
}

impl SettingUpdate {
    fn new(section: &'static str, key: &'static str, value: String) -> Self {
        Self {
            section,
            key,
            value,
        }
    }
}

#[cfg(test)]
mod tests {
    use glossa_core::{InputBackend, InputMode, LatencyMode, PasteMode, ProviderKind, UiTheme};

    use super::{apply_settings_to_config, quoted, SettingsValues};

    fn valid_config_source() -> String {
        r#"[input]
# Backend comment
backend = "portal"
mode = "push-to-talk"
shortcut = "<Ctrl><Alt>space"

[control]
enable_cli = true # keep this comment
socket_path = "auto"

[provider]
kind = "groq"
base_url = "https://api.groq.com/openai/v1"
model = "whisper-large-v3"
api_key = "env:GROQ_API_KEY"

[audio]
enabled = true
work_dir = "auto"
format = "wav"
sample_rate_hz = 16000
channels = 1
trim_silence = true
trim_threshold = 500
min_duration_ms = 150
max_duration_sec = 120
persist_audio = false
latency_mode = "balanced"
keepalive_after_stop_seconds = 60

[paste]
mode = "ctrl-v"
append_space = false # keep this comment
clipboard_command = "wl-copy"
type_command = "dotoolc"

[ui]
theme = "dark"
tray = true
idle_icon = "/tmp/idle.png"
recording_icon = "/tmp/recording.png"
processing_icon = "/tmp/processing.png"
idle_dark_icon = "/tmp/idle_dark.png"
recording_dark_icon = "/tmp/recording_dark.png"
processing_dark_icon = "/tmp/processing_dark.png"
start_sound = "/tmp/start.wav"
stop_sound = "/tmp/stop.wav"

[logging]
level = "info"
journal = true
file = false

[LLM]
enabled = false
base_url = ""
model = ""
api_key = ""
"#
        .into()
    }

    fn updated_settings() -> SettingsValues {
        SettingsValues {
            input_backend: InputBackend::None,
            input_mode: InputMode::Toggle,
            enable_cli: false,
            provider_kind: ProviderKind::OpenAiCompatible,
            provider_base_url: "https://example.com/v1".into(),
            provider_model: "custom-model".into(),
            provider_api_key: "env:CUSTOM_KEY".into(),
            llm_enabled: true,
            llm_base_url: "https://llm.example.com/v1".into(),
            llm_model: "custom-llm".into(),
            llm_api_key: "env:LLM_KEY".into(),
            paste_mode: PasteMode::ShiftInsert,
            append_space: true,
            latency_mode: LatencyMode::Instant,
            keepalive_after_stop_seconds: 30,
            ui_theme: UiTheme::Light,
        }
    }

    #[test]
    fn apply_settings_to_config_should_update_only_supported_lines_and_preserve_comments() {
        let updated = apply_settings_to_config(&valid_config_source(), &updated_settings())
            .expect("config patch should succeed");

        let expected = r#"[input]
# Backend comment
backend = "none"
mode = "toggle"
shortcut = "<Ctrl><Alt>space"

[control]
enable_cli = false # keep this comment
socket_path = "auto"

[provider]
kind = "openai-compatible"
base_url = "https://example.com/v1"
model = "custom-model"
api_key = "env:CUSTOM_KEY"

[audio]
enabled = true
work_dir = "auto"
format = "wav"
sample_rate_hz = 16000
channels = 1
trim_silence = true
trim_threshold = 500
min_duration_ms = 150
max_duration_sec = 120
persist_audio = false
latency_mode = "instant"
keepalive_after_stop_seconds = 30

[paste]
mode = "shift-insert"
append_space = true # keep this comment
clipboard_command = "wl-copy"
type_command = "dotoolc"

[ui]
theme = "light"
tray = true
idle_icon = "/tmp/idle.png"
recording_icon = "/tmp/recording.png"
processing_icon = "/tmp/processing.png"
idle_dark_icon = "/tmp/idle_dark.png"
recording_dark_icon = "/tmp/recording_dark.png"
processing_dark_icon = "/tmp/processing_dark.png"
start_sound = "/tmp/start.wav"
stop_sound = "/tmp/stop.wav"

[logging]
level = "info"
journal = true
file = false

[LLM]
enabled = true
base_url = "https://llm.example.com/v1"
model = "custom-llm"
api_key = "env:LLM_KEY"
"#;

        assert_eq!(updated, expected);
    }

    #[test]
    fn apply_settings_to_config_should_fail_when_a_supported_key_is_missing() {
        let source = valid_config_source().replace("mode = \"push-to-talk\"\n", "");

        let error = apply_settings_to_config(&source, &updated_settings())
            .expect_err("missing mode line should fail");

        assert!(error.to_string().contains("[input].mode"));
    }

    #[test]
    fn apply_settings_to_config_should_insert_missing_append_space_into_paste_section() {
        let source =
            valid_config_source().replace("append_space = false # keep this comment\n", "");

        let updated = apply_settings_to_config(&source, &updated_settings())
            .expect("missing append_space should be inserted");

        let expected = r#"[input]
# Backend comment
backend = "none"
mode = "toggle"
shortcut = "<Ctrl><Alt>space"

[control]
enable_cli = false # keep this comment
socket_path = "auto"

[provider]
kind = "openai-compatible"
base_url = "https://example.com/v1"
model = "custom-model"
api_key = "env:CUSTOM_KEY"

[audio]
enabled = true
work_dir = "auto"
format = "wav"
sample_rate_hz = 16000
channels = 1
trim_silence = true
trim_threshold = 500
min_duration_ms = 150
max_duration_sec = 120
persist_audio = false
latency_mode = "instant"
keepalive_after_stop_seconds = 30

[paste]
mode = "shift-insert"
clipboard_command = "wl-copy"
type_command = "dotoolc"
append_space = true

[ui]
theme = "light"
tray = true
idle_icon = "/tmp/idle.png"
recording_icon = "/tmp/recording.png"
processing_icon = "/tmp/processing.png"
idle_dark_icon = "/tmp/idle_dark.png"
recording_dark_icon = "/tmp/recording_dark.png"
processing_dark_icon = "/tmp/processing_dark.png"
start_sound = "/tmp/start.wav"
stop_sound = "/tmp/stop.wav"

[logging]
level = "info"
journal = true
file = false

[LLM]
enabled = true
base_url = "https://llm.example.com/v1"
model = "custom-llm"
api_key = "env:LLM_KEY"
"#;

        assert_eq!(updated, expected);
    }

    #[test]
    fn apply_settings_to_config_should_insert_missing_latency_settings_into_audio_section() {
        let source = valid_config_source()
            .replace("latency_mode = \"balanced\"\n", "")
            .replace("keepalive_after_stop_seconds = 60\n", "");

        let updated = apply_settings_to_config(&source, &updated_settings())
            .expect("missing latency settings should be inserted");

        let expected = r#"[input]
# Backend comment
backend = "none"
mode = "toggle"
shortcut = "<Ctrl><Alt>space"

[control]
enable_cli = false # keep this comment
socket_path = "auto"

[provider]
kind = "openai-compatible"
base_url = "https://example.com/v1"
model = "custom-model"
api_key = "env:CUSTOM_KEY"

[audio]
enabled = true
work_dir = "auto"
format = "wav"
sample_rate_hz = 16000
channels = 1
trim_silence = true
trim_threshold = 500
min_duration_ms = 150
max_duration_sec = 120
persist_audio = false
latency_mode = "instant"
keepalive_after_stop_seconds = 30

[paste]
mode = "shift-insert"
append_space = true # keep this comment
clipboard_command = "wl-copy"
type_command = "dotoolc"

[ui]
theme = "light"
tray = true
idle_icon = "/tmp/idle.png"
recording_icon = "/tmp/recording.png"
processing_icon = "/tmp/processing.png"
idle_dark_icon = "/tmp/idle_dark.png"
recording_dark_icon = "/tmp/recording_dark.png"
processing_dark_icon = "/tmp/processing_dark.png"
start_sound = "/tmp/start.wav"
stop_sound = "/tmp/stop.wav"

[logging]
level = "info"
journal = true
file = false

[LLM]
enabled = true
base_url = "https://llm.example.com/v1"
model = "custom-llm"
api_key = "env:LLM_KEY"
"#;

        assert_eq!(updated, expected);
    }

    #[test]
    fn apply_settings_to_config_should_insert_missing_llm_section() {
        let source = valid_config_source().replace(
            r#"
[LLM]
enabled = false
base_url = ""
model = ""
api_key = ""
"#,
            "",
        );

        let updated = apply_settings_to_config(&source, &updated_settings())
            .expect("missing LLM section should be inserted");

        assert!(updated.contains(
            r#"[LLM]
enabled = true
base_url = "https://llm.example.com/v1"
model = "custom-llm"
api_key = "env:LLM_KEY"
"#
        ));
    }

    #[test]
    fn quoted_should_escape_backslashes_and_quotes() {
        let quoted = quoted(r#"env:\"quoted\"\key"#);

        assert_eq!(quoted, r#""env:\\\"quoted\\\"\\key""#);
    }
}
