use serde::{Deserialize, Deserializer, Serialize};

use crate::CoreError;

const MAX_TERMS: usize = 99;
const MAX_TERM_CHARS: usize = 50;
const MAX_TERM_WORDS: usize = 5;
const FORBIDDEN_CHARS: [char; 7] = ['<', '>', '{', '}', '[', ']', '\\'];

/// User dictionary that biases transcription toward listed terms.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, deserialize_with = "deserialize_terms")]
    pub terms: Vec<String>,
}

impl DictionaryConfig {
    pub(crate) fn validate(&self) -> Result<(), CoreError> {
        check_terms(&self.terms)
    }

    /// Terms to send to the provider: none unless the dictionary is enabled.
    #[must_use]
    pub fn active_terms(&self) -> &[String] {
        if self.enabled {
            &self.terms
        } else {
            &[]
        }
    }
}

/// Parses comma-separated dictionary terms as entered in Settings.
pub fn parse_dictionary_terms(input: &str) -> Result<Vec<String>, CoreError> {
    let terms = normalize_terms(input.split(','));
    check_terms(&terms)?;
    Ok(terms)
}

fn deserialize_terms<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let raw = Vec::<String>::deserialize(deserializer)?;
    Ok(normalize_terms(raw.iter().map(String::as_str)))
}

fn normalize_terms<'a>(entries: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = Vec::<String>::new();
    let mut terms = Vec::new();
    for term in entries.into_iter().map(str::trim) {
        let key = term.to_lowercase();
        if term.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        terms.push(term.to_string());
    }
    terms
}

fn check_terms(terms: &[String]) -> Result<(), CoreError> {
    if terms.len() > MAX_TERMS {
        return Err(invalid(format!(
            "dictionary.terms must contain at most {MAX_TERMS} terms, found {}",
            terms.len()
        )));
    }
    for term in terms {
        if term.chars().count() > MAX_TERM_CHARS {
            return Err(invalid(format!(
                "dictionary term \"{term}\" must have at most {MAX_TERM_CHARS} characters"
            )));
        }
        if term.split_whitespace().count() > MAX_TERM_WORDS {
            return Err(invalid(format!(
                "dictionary term \"{term}\" must have at most {MAX_TERM_WORDS} words"
            )));
        }
        if let Some(ch) = term.chars().find(|ch| FORBIDDEN_CHARS.contains(ch)) {
            return Err(invalid(format!(
                "dictionary term \"{term}\" must not contain '{ch}' (forbidden: < > {{ }} [ ] \\)"
            )));
        }
    }
    Ok(())
}

fn invalid(message: String) -> CoreError {
    CoreError::InvalidConfig(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppConfig;

    const BASE_CONFIG: &str = r#"[input]
backend = "portal"
shortcut = "<Ctrl><Alt>space"
mode = "toggle"

[control]
enable_cli = true
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

[paste]
mode = "ctrl-v"
append_space = false
clipboard_command = "wl-copy"
type_command = "dotoolc"

[ui]
tray = true
idle_icon = "/tmp/idle.png"
recording_icon = "/tmp/recording.png"
start_sound = "/tmp/start.wav"
stop_sound = "/tmp/stop.wav"

[logging]
level = "info"
journal = true
file = false
"#;

    fn load(dictionary: &str) -> Result<AppConfig, CoreError> {
        AppConfig::from_toml_str(&format!("{BASE_CONFIG}\n{dictionary}"))
    }

    #[test]
    fn config_without_dictionary_should_load_disabled_and_empty() {
        let config = load("").expect("config should parse");
        assert!(!config.dictionary.enabled);
        assert!(config.dictionary.terms.is_empty());
        assert!(config.dictionary.active_terms().is_empty());
    }

    #[test]
    fn enabled_dictionary_should_load_normalized_terms() {
        let config = load(
            "[dictionary]\nenabled = true\nterms = [\" Glossa \", \"\", \"OpenRouter\", \"glossa\", \"Скрайб два\"]\n",
        )
        .expect("config should parse");
        assert_eq!(
            config.dictionary.active_terms(),
            ["Glossa", "OpenRouter", "Скрайб два"]
        );
    }

    #[test]
    fn disabled_dictionary_should_keep_terms_but_send_none() {
        let config = load("[dictionary]\nenabled = false\nterms = [\"Glossa\"]\n")
            .expect("config should parse");
        assert_eq!(config.dictionary.terms, ["Glossa"]);
        assert!(config.dictionary.active_terms().is_empty());
    }

    #[test]
    fn config_breaking_a_term_rule_should_fail_naming_the_rule() {
        let too_many = (0..100)
            .map(|index| format!("\"term{index}\""))
            .collect::<Vec<_>>()
            .join(", ");
        for (terms, rule) in [
            (too_many, "at most 99 terms"),
            (format!("\"{}\"", "a".repeat(51)), "at most 50 characters"),
            ("\"one two three four five six\"".into(), "at most 5 words"),
            ("\"a<b\"".into(), "must not contain"),
            ("\"x\\\\y\"".into(), "must not contain"),
        ] {
            let error = load(&format!(
                "[dictionary]\nenabled = true\nterms = [{terms}]\n"
            ))
            .expect_err("invalid terms should fail validation");
            assert!(
                error.to_string().contains(rule),
                "expected {rule:?} in {error}"
            );
        }
    }

    #[test]
    fn parse_should_split_trim_and_dedupe_in_user_order() {
        assert_eq!(
            parse_dictionary_terms(" Glossa, ,OpenRouter,GLOSSA ,Scribe v2,").expect("valid"),
            ["Glossa", "OpenRouter", "Scribe v2"]
        );
        assert!(parse_dictionary_terms("  ").expect("valid").is_empty());
    }

    #[test]
    fn parse_should_accept_99_terms_and_reject_100() {
        let terms = |count: usize| {
            (0..count)
                .map(|index| format!("term{index}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        assert_eq!(parse_dictionary_terms(&terms(99)).expect("valid").len(), 99);
        let error = parse_dictionary_terms(&terms(100)).expect_err("too many terms");
        assert!(error.to_string().contains("at most 99 terms"));
    }

    #[test]
    fn parse_should_accept_terms_at_the_length_and_word_limits() {
        let fifty = "я".repeat(50);
        assert_eq!(parse_dictionary_terms(&fifty).expect("valid"), [fifty]);
        assert_eq!(
            parse_dictionary_terms("one two three four five").expect("valid"),
            ["one two three four five"]
        );
    }

    #[test]
    fn parse_should_reject_each_forbidden_character() {
        for ch in ['<', '>', '{', '}', '[', ']', '\\'] {
            let error = parse_dictionary_terms(&format!("bad{ch}term"))
                .expect_err("forbidden character should fail");
            assert!(
                error.to_string().contains("must not contain"),
                "{ch}: {error}"
            );
        }
    }
}
