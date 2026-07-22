use gtk::{
    glib::object::IsA, prelude::*, Box as GtkBox, CheckButton, ComboBoxText, Dialog, DialogFlags,
    Entry, Frame, Grid, Label, Orientation, ResponseType, Widget, Window,
};

use glossa_app::AppError;

use super::settings::{
    input_backend_id, input_mode_id, latency_mode_id, parse_input_backend, parse_input_mode,
    parse_latency_mode, parse_paste_mode, parse_provider_kind, parse_ui_theme, paste_mode_id,
    provider_kind_id, ui_theme_id, SettingsValues,
};

const INPUT_BACKEND_TOOLTIP: &str =
    "Selects the input backend that listens for recording commands. Options: portal or none.";
const INPUT_MODE_TOOLTIP: &str =
    "Selects how the portal shortcut behaves. Options: push-to-talk or toggle.";
const ENABLE_CLI_TOOLTIP: &str =
    "Enables the CLI control channel used by commands like `glossa ctl toggle`.";
const PROVIDER_KIND_TOOLTIP: &str =
    "Selects the speech-to-text provider mode. Options: groq, openai, or openai-compatible.";
const PROVIDER_BASE_URL_TOOLTIP: &str =
    "Sets the provider base URL. Required for openai-compatible configurations.";
const PROVIDER_MODEL_TOOLTIP: &str =
    "Sets the transcription model that the configured provider should use.";
const PROVIDER_API_KEY_TOOLTIP: &str =
    "API keys entered here are stored in Secret Service. Values such as env:VAR are also supported.";
const PASTE_MODE_TOOLTIP: &str = "Selects which keyboard shortcut dotool should emulate for paste.";
const APPEND_SPACE_TOOLTIP: &str =
    "Appends one trailing space to the pasted transcription for continuous dictation.";
const LATENCY_MODE_TOOLTIP: &str =
    "Selects idle microphone stream policy. Options: off, balanced, or instant.";
const KEEPALIVE_AFTER_STOP_SECONDS_TOOLTIP: &str =
    "Sets how many seconds balanced mode keeps the microphone stream warm after recording stops.";
const UI_THEME_TOOLTIP: &str = "Selects which tray icon set to use. Options: dark or light.";
const LLM_ENHANCER_TOOLTIP: &str = "Opens LLM enhancer settings.";
const LLM_ENABLED_TOOLTIP: &str =
    "Enables LLM post-processing for transcribed text before pasting.";
const LLM_BASE_URL_TOOLTIP: &str = "Sets the OpenAI-compatible chat completions base URL.";
const LLM_MODEL_TOOLTIP: &str = "Sets the chat model alias used for text enhancement.";
const LLM_API_KEY_TOOLTIP: &str =
    "API keys entered here are stored in Secret Service. Values such as env:VAR are also supported.";

pub(super) fn edit_settings(current: &SettingsValues) -> Result<Option<SettingsValues>, AppError> {
    let dialog = Dialog::with_buttons(
        Some("Settings"),
        None::<&Window>,
        DialogFlags::MODAL,
        &[
            ("LLM enhancer", ResponseType::Help),
            ("Cancel", ResponseType::Cancel),
            ("Save", ResponseType::Accept),
        ],
    );
    dialog.set_default_size(560, 580);
    dialog.set_resizable(true);
    dialog.set_keep_above(true);
    dialog.set_default_response(ResponseType::Accept);
    apply_response_button_spacing(&dialog);
    if let Some(button) = dialog.widget_for_response(ResponseType::Help) {
        button.set_tooltip_text(Some(LLM_ENHANCER_TOOLTIP));
    }

    let content = dialog.content_area();
    let container = GtkBox::new(Orientation::Vertical, 12);
    container.set_margin_top(12);
    container.set_margin_bottom(12);
    container.set_margin_start(12);
    container.set_margin_end(12);

    let input_grid = create_section_grid();
    let backend_combo = create_combo(
        &[("portal", "portal"), ("none", "none")],
        input_backend_id(current.input_backend),
        INPUT_BACKEND_TOOLTIP,
    );
    attach_row(
        &input_grid,
        0,
        "Backend",
        INPUT_BACKEND_TOOLTIP,
        &backend_combo,
    );
    let mode_combo = create_combo(
        &[("push-to-talk", "push-to-talk"), ("toggle", "toggle")],
        input_mode_id(current.input_mode),
        INPUT_MODE_TOOLTIP,
    );
    attach_row(&input_grid, 1, "Mode", INPUT_MODE_TOOLTIP, &mode_combo);
    container.pack_start(&wrap_section("Input", &input_grid), false, false, 0);

    let control_grid = create_section_grid();
    let enable_cli = CheckButton::new();
    enable_cli.set_active(current.enable_cli);
    enable_cli.set_tooltip_text(Some(ENABLE_CLI_TOOLTIP));
    attach_row(
        &control_grid,
        0,
        "Enable CLI",
        ENABLE_CLI_TOOLTIP,
        &enable_cli,
    );
    container.pack_start(&wrap_section("Control", &control_grid), false, false, 0);

    let provider_grid = create_section_grid();
    let provider_kind = create_combo(
        &[
            ("groq", "groq"),
            ("openai", "openai"),
            ("openai-compatible", "openai-compatible"),
        ],
        provider_kind_id(current.provider_kind),
        PROVIDER_KIND_TOOLTIP,
    );
    attach_row(
        &provider_grid,
        0,
        "Kind",
        PROVIDER_KIND_TOOLTIP,
        &provider_kind,
    );
    let base_url = create_entry(&current.provider_base_url, PROVIDER_BASE_URL_TOOLTIP, false);
    attach_row(
        &provider_grid,
        1,
        "Base URL",
        PROVIDER_BASE_URL_TOOLTIP,
        &base_url,
    );
    let model = create_entry(&current.provider_model, PROVIDER_MODEL_TOOLTIP, false);
    attach_row(&provider_grid, 2, "Model", PROVIDER_MODEL_TOOLTIP, &model);
    let api_key = create_entry(&current.provider_api_key, PROVIDER_API_KEY_TOOLTIP, true);
    attach_row(
        &provider_grid,
        3,
        "API Key",
        PROVIDER_API_KEY_TOOLTIP,
        &api_key,
    );
    container.pack_start(&wrap_section("Provider", &provider_grid), false, false, 0);

    let audio_grid = create_section_grid();
    let latency_mode = create_combo(
        &[
            ("off", "off"),
            ("balanced", "balanced"),
            ("instant", "instant"),
        ],
        latency_mode_id(current.latency_mode),
        LATENCY_MODE_TOOLTIP,
    );
    attach_row(
        &audio_grid,
        0,
        "Latency mode",
        LATENCY_MODE_TOOLTIP,
        &latency_mode,
    );
    let keepalive_after_stop_seconds = create_entry(
        &current.keepalive_after_stop_seconds.to_string(),
        KEEPALIVE_AFTER_STOP_SECONDS_TOOLTIP,
        false,
    );
    attach_row(
        &audio_grid,
        1,
        "Keepalive seconds",
        KEEPALIVE_AFTER_STOP_SECONDS_TOOLTIP,
        &keepalive_after_stop_seconds,
    );
    container.pack_start(&wrap_section("Audio", &audio_grid), false, false, 0);

    let paste_grid = create_section_grid();
    let paste_mode = create_combo(
        &[
            ("ctrl-v", "ctrl-v"),
            ("ctrl-shift-v", "ctrl-shift-v"),
            ("shift-insert", "shift-insert"),
        ],
        paste_mode_id(current.paste_mode),
        PASTE_MODE_TOOLTIP,
    );
    attach_row(&paste_grid, 0, "Mode", PASTE_MODE_TOOLTIP, &paste_mode);
    let append_space = CheckButton::new();
    append_space.set_active(current.append_space);
    append_space.set_tooltip_text(Some(APPEND_SPACE_TOOLTIP));
    attach_row(
        &paste_grid,
        1,
        "Append space",
        APPEND_SPACE_TOOLTIP,
        &append_space,
    );
    container.pack_start(&wrap_section("Paste", &paste_grid), false, false, 0);

    let ui_grid = create_section_grid();
    let theme = create_combo(
        &[("dark", "dark"), ("light", "light")],
        ui_theme_id(current.ui_theme),
        UI_THEME_TOOLTIP,
    );
    attach_row(&ui_grid, 0, "Theme", UI_THEME_TOOLTIP, &theme);
    container.pack_start(&wrap_section("UI", &ui_grid), false, false, 0);

    content.pack_start(&container, true, true, 0);
    dialog.show_all();
    dialog.present();

    let result = loop {
        match dialog.run() {
            ResponseType::Accept => {
                break Some(SettingsValues {
                    input_backend: parse_input_backend(&selected_id(
                        &backend_combo,
                        "input backend",
                    )?)
                    .ok_or_else(|| AppError::message("input backend selection is invalid"))?,
                    input_mode: parse_input_mode(&selected_id(&mode_combo, "input mode")?)
                        .ok_or_else(|| AppError::message("input mode selection is invalid"))?,
                    enable_cli: enable_cli.is_active(),
                    provider_kind: parse_provider_kind(&selected_id(
                        &provider_kind,
                        "provider kind",
                    )?)
                    .ok_or_else(|| AppError::message("provider kind selection is invalid"))?,
                    provider_base_url: base_url.text().to_string(),
                    provider_model: model.text().to_string(),
                    provider_api_key: api_key.text().to_string(),
                    llm_enabled: current.llm_enabled,
                    llm_base_url: current.llm_base_url.clone(),
                    llm_model: current.llm_model.clone(),
                    llm_api_key: current.llm_api_key.clone(),
                    paste_mode: parse_paste_mode(&selected_id(&paste_mode, "paste mode")?)
                        .ok_or_else(|| AppError::message("paste mode selection is invalid"))?,
                    append_space: append_space.is_active(),
                    latency_mode: parse_latency_mode(&selected_id(&latency_mode, "latency mode")?)
                        .ok_or_else(|| AppError::message("latency mode selection is invalid"))?,
                    keepalive_after_stop_seconds: parse_keepalive_seconds(
                        keepalive_after_stop_seconds.text().as_str(),
                    )?,
                    ui_theme: parse_ui_theme(&selected_id(&theme, "UI theme")?)
                        .ok_or_else(|| AppError::message("UI theme selection is invalid"))?,
                });
            }
            ResponseType::Help => {
                if let Some(settings) = edit_llm_settings(current)? {
                    break Some(settings);
                }
            }
            _ => break None,
        }
    };

    dialog.close();
    Ok(result)
}

fn edit_llm_settings(current: &SettingsValues) -> Result<Option<SettingsValues>, AppError> {
    let dialog = Dialog::with_buttons(
        Some("LLM Enhancer Settings"),
        None::<&Window>,
        DialogFlags::MODAL,
        &[
            ("Cancel", ResponseType::Cancel),
            ("Save", ResponseType::Accept),
        ],
    );
    dialog.set_default_size(520, 260);
    dialog.set_resizable(false);
    dialog.set_keep_above(true);
    dialog.set_default_response(ResponseType::Accept);
    apply_response_button_spacing(&dialog);

    let content = dialog.content_area();
    let container = GtkBox::new(Orientation::Vertical, 12);
    container.set_margin_top(12);
    container.set_margin_bottom(12);
    container.set_margin_start(12);
    container.set_margin_end(12);

    let llm_grid = create_section_grid();
    let llm_enabled = CheckButton::new();
    llm_enabled.set_active(current.llm_enabled);
    llm_enabled.set_tooltip_text(Some(LLM_ENABLED_TOOLTIP));
    attach_row(&llm_grid, 0, "Enabled", LLM_ENABLED_TOOLTIP, &llm_enabled);
    let llm_base_url = create_entry(&current.llm_base_url, LLM_BASE_URL_TOOLTIP, false);
    attach_row(
        &llm_grid,
        1,
        "Base URL",
        LLM_BASE_URL_TOOLTIP,
        &llm_base_url,
    );
    let llm_model = create_entry(&current.llm_model, LLM_MODEL_TOOLTIP, false);
    attach_row(&llm_grid, 2, "Model alias", LLM_MODEL_TOOLTIP, &llm_model);
    let llm_api_key = create_entry(&current.llm_api_key, LLM_API_KEY_TOOLTIP, true);
    attach_row(&llm_grid, 3, "API Key", LLM_API_KEY_TOOLTIP, &llm_api_key);

    container.pack_start(&wrap_section("LLM enhancer", &llm_grid), false, false, 0);
    content.pack_start(&container, true, true, 0);
    dialog.show_all();
    dialog.present();

    let result = if dialog.run() == ResponseType::Accept {
        let mut settings = current.clone();
        settings.llm_enabled = llm_enabled.is_active();
        settings.llm_base_url = llm_base_url.text().to_string();
        settings.llm_model = llm_model.text().to_string();
        settings.llm_api_key = llm_api_key.text().to_string();
        Some(settings)
    } else {
        None
    };

    dialog.close();
    Ok(result)
}

fn create_section_grid() -> Grid {
    let grid = Grid::new();
    grid.set_row_spacing(10);
    grid.set_column_spacing(16);
    grid
}

fn wrap_section(title: &str, grid: &Grid) -> Frame {
    let frame = Frame::new(Some(title));
    frame.set_margin_bottom(4);
    frame.set_label_align(0.03, 0.5);

    let section_body = GtkBox::new(Orientation::Vertical, 0);
    section_body.set_margin_top(10);
    section_body.set_margin_bottom(10);
    section_body.set_margin_start(12);
    section_body.set_margin_end(12);
    section_body.pack_start(grid, true, true, 0);

    frame.add(&section_body);
    frame
}

fn create_combo(options: &[(&str, &str)], selected_id: &str, tooltip: &str) -> ComboBoxText {
    let combo = ComboBoxText::new();
    combo.set_hexpand(true);
    combo.set_tooltip_text(Some(tooltip));

    for (id, label) in options {
        combo.append(Some(id), label);
    }

    if !combo.set_active_id(Some(selected_id)) {
        combo.set_active(Some(0));
    }

    combo
}

fn create_entry(text: &str, tooltip: &str, masked: bool) -> Entry {
    let entry = Entry::new();
    entry.set_hexpand(true);
    entry.set_text(text);
    entry.set_visibility(!masked);
    entry.set_tooltip_text(Some(tooltip));
    entry
}

fn attach_row<W: IsA<Widget>>(grid: &Grid, row: i32, label_text: &str, tooltip: &str, widget: &W) {
    let label = Label::new(Some(label_text));
    label.set_xalign(0.0);
    label.set_yalign(0.5);
    label.set_tooltip_text(Some(tooltip));
    widget.set_tooltip_text(Some(tooltip));

    grid.attach(&label, 0, row, 1, 1);
    grid.attach(widget, 1, row, 1, 1);
}

fn apply_response_button_spacing(dialog: &Dialog) {
    for response in [
        ResponseType::Help,
        ResponseType::Cancel,
        ResponseType::Accept,
    ] {
        if let Some(button) = dialog.widget_for_response(response) {
            button.set_margin_top(8);
            button.set_margin_bottom(4);
            button.set_margin_start(6);
            button.set_margin_end(6);
        }
    }
}

fn selected_id(combo: &ComboBoxText, label: &str) -> Result<String, AppError> {
    combo
        .active_id()
        .map(|value| value.to_string())
        .ok_or_else(|| AppError::message(format!("{label} must be selected")))
}

fn parse_keepalive_seconds(value: &str) -> Result<u64, AppError> {
    let seconds = value.trim().parse::<u64>().map_err(|_| {
        AppError::message("keepalive_after_stop_seconds must be a positive integer")
    })?;

    if seconds == 0 {
        return Err(AppError::message(
            "keepalive_after_stop_seconds must be greater than zero",
        ));
    }

    Ok(seconds)
}
