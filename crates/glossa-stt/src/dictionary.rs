use glossa_core::ProviderKind;
use tracing::warn;

// why: Whisper-family endpoints read about 224 prompt tokens and Glossa has no tokenizer;
// 200 characters stays under that limit even for Cyrillic, which can cost a token per letter.
const PROMPT_CHAR_BUDGET: usize = 200;
const PROMPT_SEPARATOR: &str = ", ";

/// How the dictionary travels in a transcription request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DictionaryRequest {
    None,
    Prompt { text: String, terms: usize },
    Keywords(Vec<String>),
    Keyterms(Vec<String>),
}

impl DictionaryRequest {
    pub(crate) fn new(kind: ProviderKind, model: &str, terms: &[String]) -> Self {
        if terms.is_empty() {
            return Self::None;
        }
        let alias = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        match kind {
            ProviderKind::OpenRouter if alias.starts_with("scribe") => {
                Self::Keyterms(terms.to_vec())
            }
            ProviderKind::OpenAi if alias.starts_with("gpt-transcribe") => {
                Self::Keywords(terms.to_vec())
            }
            _ => {
                let (text, dropped) = prompt_within_budget(terms);
                if dropped > 0 {
                    warn!(
                        dropped,
                        sent = terms.len() - dropped,
                        "dictionary does not fit the transcription prompt; dropped terms from the end of the list"
                    );
                }
                Self::Prompt {
                    text,
                    terms: terms.len() - dropped,
                }
            }
        }
    }

    pub(crate) fn mode_name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Prompt { .. } => "prompt",
            Self::Keywords(_) => "keywords",
            Self::Keyterms(_) => "keyterms",
        }
    }

    pub(crate) fn term_count(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Prompt { terms, .. } => *terms,
            Self::Keywords(terms) | Self::Keyterms(terms) => terms.len(),
        }
    }
}

/// Joins terms into a prompt, dropping whole terms from the end to fit the budget.
/// Returns the prompt and the number of dropped terms.
pub(crate) fn prompt_within_budget(terms: &[String]) -> (String, usize) {
    let mut prompt = String::new();
    let mut used = 0;
    for term in terms {
        let separator = if used == 0 { "" } else { PROMPT_SEPARATOR };
        if prompt.chars().count() + separator.len() + term.chars().count() > PROMPT_CHAR_BUDGET {
            break;
        }
        prompt.push_str(separator);
        prompt.push_str(term);
        used += 1;
    }
    (prompt, terms.len() - used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_should_follow_provider_kind_and_model_alias() {
        use ProviderKind::{Groq, OpenAi, OpenAiCompatible, OpenRouter};
        let terms = vec!["Glossa".to_string()];
        for (kind, model, expected) in [
            (OpenRouter, "elevenlabs/scribe-v2", "keyterms"),
            (OpenRouter, "ElevenLabs/Scribe-V1", "keyterms"),
            (OpenAi, "gpt-transcribe", "keywords"),
            (OpenAi, "GPT-Transcribe-2026-08-01", "keywords"),
            (OpenRouter, "openai/gpt-transcribe", "prompt"),
            (OpenRouter, "openai/gpt-4o-transcribe", "prompt"),
            (OpenAi, "gpt-4o-transcribe", "prompt"),
            (OpenAi, "gpt-4o-mini-transcribe", "prompt"),
            (Groq, "whisper-large-v3", "prompt"),
            (OpenRouter, "openai/whisper-large-v3", "prompt"),
            (OpenAi, "scribe-v2", "prompt"),
            (Groq, "some-unknown-model", "prompt"),
            (OpenAiCompatible, "scribe-v2", "prompt"),
            (OpenAiCompatible, "gpt-transcribe", "prompt"),
        ] {
            assert_eq!(
                DictionaryRequest::new(kind, model, &terms).mode_name(),
                expected,
                "{kind:?} {model}"
            );
        }
    }

    #[test]
    fn prompt_should_join_terms_that_fit() {
        let terms = vec![
            "Glossa".to_string(),
            "OpenRouter".into(),
            "Scribe v2".into(),
        ];
        assert_eq!(
            prompt_within_budget(&terms),
            ("Glossa, OpenRouter, Scribe v2".to_string(), 0)
        );
    }

    #[test]
    fn prompt_should_drop_whole_terms_from_the_end_to_fit_the_budget() {
        let terms: Vec<String> = (0..99).map(|index| format!("Термин{index:02}")).collect();
        let (prompt, dropped) = prompt_within_budget(&terms);
        assert_eq!(dropped, 79);
        assert!(prompt.starts_with("Термин00, Термин01, "));
        assert!(prompt.ends_with(", Термин19"));
        assert_eq!(prompt.chars().count(), 198);
    }
}

#[cfg(test)]
mod request_tests {
    use glossa_core::{AppConfig, AudioFormat, ProviderKind};

    use crate::test_support::transcribe_once;

    fn config(kind: ProviderKind, model: &str, enabled: bool, terms: &[&str]) -> AppConfig {
        let mut config = AppConfig::default();
        config.provider.kind = kind;
        config.provider.model = model.into();
        config.dictionary.enabled = enabled;
        config.dictionary.terms = terms.iter().map(|term| (*term).to_string()).collect();
        config
    }

    #[tokio::test]
    async fn prompt_mode_should_send_joined_terms_in_multipart_prompt() {
        let exchange = transcribe_once(
            config(
                ProviderKind::Groq,
                "whisper-large-v3",
                true,
                &["Glossa", "Скрайб"],
            ),
            "/openai/v1",
            "wav",
        )
        .await;
        assert_eq!(exchange.result.expect("transcribe"), "Привет, мир!");
        assert!(exchange.request.contains("multipart/form-data; boundary="));
        assert!(exchange
            .request
            .contains("name=\"prompt\"\r\n\r\nGlossa, Скрайб\r\n"));
        assert!(!exchange.request.contains("keywords"));
    }

    #[tokio::test]
    async fn prompt_mode_should_cut_an_overlong_list_at_whole_terms() {
        let terms: Vec<String> = (0..99).map(|index| format!("Термин{index:02}")).collect();
        let terms: Vec<&str> = terms.iter().map(String::as_str).collect();
        let exchange = transcribe_once(
            config(
                ProviderKind::OpenRouter,
                "openai/gpt-transcribe",
                true,
                &terms,
            ),
            "/api/v1",
            "wav",
        )
        .await;
        assert!(exchange
            .request
            .contains("name=\"prompt\"\r\n\r\nТермин00, Термин01, "));
        assert!(exchange.request.contains(", Термин19\r\n"));
        assert!(!exchange.request.contains("Термин20"));
    }

    #[tokio::test]
    async fn keywords_mode_should_repeat_the_keywords_array_part_per_term() {
        let exchange = transcribe_once(
            config(
                ProviderKind::OpenAi,
                "gpt-transcribe",
                true,
                &["Glossa", "Scribe v2"],
            ),
            "/v1",
            "wav",
        )
        .await;
        assert_eq!(exchange.result.expect("transcribe"), "Привет, мир!");
        assert!(exchange
            .request
            .contains("name=\"keywords[]\"\r\n\r\nGlossa\r\n"));
        assert!(exchange
            .request
            .contains("name=\"keywords[]\"\r\n\r\nScribe v2\r\n"));
        assert!(!exchange.request.contains("name=\"prompt\""));
    }

    #[tokio::test]
    async fn keyterms_mode_should_send_json_with_base64_audio_for_every_format() {
        for format in [
            AudioFormat::Wav,
            AudioFormat::Mp3,
            AudioFormat::M4a,
            AudioFormat::Flac,
            AudioFormat::Ogg,
            AudioFormat::Aac,
        ] {
            let exchange = transcribe_once(
                config(
                    ProviderKind::OpenRouter,
                    "elevenlabs/scribe-v2",
                    true,
                    &["Glossa", "Скрайб"],
                ),
                "/api/v1/",
                &format.extension().to_ascii_uppercase(),
            )
            .await;
            assert_eq!(
                exchange.result.as_deref().expect("transcribe"),
                "Привет, мир!"
            );
            assert!(exchange
                .request
                .starts_with("POST /api/v1/audio/transcriptions HTTP/1.1\r\n"));
            let lower = exchange.request.to_ascii_lowercase();
            assert!(lower.contains("content-type: application/json\r\n"));
            assert!(lower.contains("authorization: bearer test-key\r\n"));
            let body: serde_json::Value = serde_json::from_str(exchange.body()).expect("JSON body");
            assert_eq!(
                body,
                serde_json::json!({
                    "model": "elevenlabs/scribe-v2",
                    "input_audio": { "data": "UklGRi10ZXN0LWF1ZGlv", "format": format.extension() },
                    "keyterms": ["Glossa", "Скрайб"],
                })
            );
        }
    }

    #[tokio::test]
    async fn disabled_or_empty_dictionary_should_keep_the_plain_multipart_request() {
        for (enabled, terms) in [(false, &["Glossa"][..]), (true, &[][..])] {
            let exchange = transcribe_once(
                config(
                    ProviderKind::OpenRouter,
                    "elevenlabs/scribe-v2",
                    enabled,
                    terms,
                ),
                "/api/v1",
                "wav",
            )
            .await;
            assert!(exchange.request.contains("multipart/form-data; boundary="));
            assert!(exchange.request.contains("name=\"file\""));
            assert!(!exchange.request.contains("keyterms"));
            assert!(!exchange.request.contains("name=\"prompt\""));
            assert!(!exchange.request.contains("Glossa"));
        }
    }
}
