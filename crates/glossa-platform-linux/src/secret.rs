//! API-key storage backed by the desktop Secret Service.

use std::{fmt, fs, path::Path, time::Duration};

use async_trait::async_trait;
use glossa_app::{ports::ApiKeyProvider, AppError};
use glossa_core::{AppConfig, SecretSource};
use keyring::Entry;
use tokio::sync::OnceCell;

const SERVICE: &str = "com.github.glaicer.glossa";
pub const PROVIDER_SLOT: &str = "provider";
pub const LLM_SLOT: &str = "llm";

/// Upper bound on a single Secret Service round-trip.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolves an API-key source without exposing the value in diagnostics.
pub fn resolve(source: &SecretSource) -> Result<String, AppError> {
    match source {
        SecretSource::SecretService(slot) => read(slot),
        _ => source.resolve().map_err(Into::into),
    }
}

/// API-key provider that resolves on first use and caches the result.
///
/// The daemon starts at `graphical-session.target`, which can win the race
/// against the desktop keyring. Resolving eagerly meant a locked keyring turned
/// the startup path into an unbounded wait on an unlock prompt that nothing was
/// there to answer, so the daemon hung before it ever bound its IPC socket or
/// tray. Resolving lazily moves that call to a point where the session is fully
/// up, and a failure is retried on the next attempt instead of being cached.
pub struct LazyApiKey {
    source: SecretSource,
    cached: OnceCell<String>,
}

impl LazyApiKey {
    #[must_use]
    pub fn new(source: SecretSource) -> Self {
        Self {
            source,
            cached: OnceCell::new(),
        }
    }
}

impl fmt::Debug for LazyApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LazyApiKey")
            .field("source", &self.source.describe())
            .field("resolved", &self.cached.initialized())
            .finish()
    }
}

#[async_trait]
impl ApiKeyProvider for LazyApiKey {
    async fn api_key(&self) -> Result<String, AppError> {
        self.cached
            .get_or_try_init(|| resolve_off_runtime(self.source.clone()))
            .await
            .cloned()
    }
}

/// Runs the blocking Secret Service call off the runtime under a timeout.
async fn resolve_off_runtime(source: SecretSource) -> Result<String, AppError> {
    let described = source.describe();
    let resolving = tokio::task::spawn_blocking(move || resolve(&source));

    match tokio::time::timeout(RESOLVE_TIMEOUT, resolving).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(AppError::message(format!(
            "failed to resolve {described}: {error}"
        ))),
        Err(_) => Err(AppError::message(format!(
            "timed out after {}s resolving {described}; is the desktop keyring unlocked?",
            RESOLVE_TIMEOUT.as_secs()
        ))),
    }
}

/// Stores a secret received on stdin or from a settings dialog.
pub fn store(slot: &str, secret: &str) -> Result<(), AppError> {
    validate_slot(slot)?;
    let entry = Entry::new(SERVICE, slot).map_err(|error| secret_service_error(slot, error))?;
    entry
        .set_password(secret)
        .map_err(|error| secret_service_error(slot, error))
}

/// Converts a settings value into a non-plaintext source, storing literals in Secret Service.
pub fn secure_input(value: &str, slot: &str) -> Result<String, AppError> {
    secure_input_with(value, slot, store)
}

fn secure_input_with<F>(value: &str, slot: &str, store_secret: F) -> Result<String, AppError>
where
    F: FnOnce(&str, &str) -> Result<(), AppError>,
{
    let source = SecretSource::try_from(value.to_owned())?;
    match source {
        SecretSource::Literal(secret) => {
            store_secret(slot, &secret)?;
            Ok(format!("secret-service:{slot}"))
        }
        source => Ok(String::from(source)),
    }
}

/// Moves legacy literal keys to Secret Service and rewrites only their TOML values.
pub fn migrate_plaintext_api_keys(path: &Path, config: &mut AppConfig) -> Result<bool, AppError> {
    let provider = migrate_source(&config.provider.api_key, PROVIDER_SLOT)?;
    let llm = migrate_source(&config.llm.api_key, LLM_SLOT)?;
    if provider.is_none() && llm.is_none() {
        return Ok(false);
    }

    let source = fs::read_to_string(path)
        .map_err(|error| AppError::io("failed to read config.toml for secret migration", error))?;
    let updated = rewrite_api_key_sources(&source, provider.as_ref(), llm.as_ref());
    let updated_config = AppConfig::from_toml_str(&updated)?;
    fs::write(path, updated)
        .map_err(|error| AppError::io("failed to write migrated config.toml", error))?;
    *config = updated_config;
    Ok(true)
}

fn read(slot: &str) -> Result<String, AppError> {
    validate_slot(slot)?;
    let entry = Entry::new(SERVICE, slot).map_err(|error| secret_service_error(slot, error))?;
    entry
        .get_password()
        .map_err(|error| secret_service_error(slot, error))
}

fn validate_slot(slot: &str) -> Result<(), AppError> {
    if matches!(slot, PROVIDER_SLOT | LLM_SLOT) {
        Ok(())
    } else {
        Err(AppError::message(format!(
            "unknown Glossa Secret Service slot: {slot}"
        )))
    }
}

fn secret_service_error(slot: &str, error: keyring::Error) -> AppError {
    AppError::message(format!(
        "failed to access Secret Service entry {slot}: {error}"
    ))
}

fn migrate_source(source: &SecretSource, slot: &str) -> Result<Option<SecretSource>, AppError> {
    let SecretSource::Literal(secret) = source else {
        return Ok(None);
    };
    store(slot, secret)?;
    Ok(Some(SecretSource::SecretService(slot.to_owned())))
}

fn rewrite_api_key_sources(
    source: &str,
    provider: Option<&SecretSource>,
    llm: Option<&SecretSource>,
) -> String {
    let mut section = "";
    let mut output = String::with_capacity(source.len());

    for raw_line in source.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let newline = if raw_line.ends_with('\n') { "\n" } else { "" };
        let trimmed = line.trim();
        if let Some(name) = trimmed
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            section = name.trim();
        }

        let replacement = match section {
            "provider" => provider,
            "LLM" => llm,
            _ => None,
        };
        if let Some(replacement) = replacement.filter(|_| is_api_key_line(trimmed)) {
            let value = format!("\"{}\"", String::from(replacement.clone()));
            output.push_str(&replace_toml_value(line, &value));
        } else {
            output.push_str(line);
        }
        output.push_str(newline);
    }
    output
}

fn is_api_key_line(line: &str) -> bool {
    line.strip_prefix("api_key")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn replace_toml_value(line: &str, new_value: &str) -> String {
    let Some(eq_index) = line.find('=') else {
        return line.to_owned();
    };
    let prefix = &line[..=eq_index];
    let after_eq = &line[eq_index + 1..];
    let comment_index = find_comment_start(after_eq).unwrap_or(after_eq.len());
    let value = &after_eq[..comment_index];
    let leading_whitespace = &value[..value.len() - value.trim_start().len()];
    let trailing_whitespace = &value[value.trim_end().len()..];
    let suffix = &after_eq[comment_index..];
    format!("{prefix}{leading_whitespace}{new_value}{trailing_whitespace}{suffix}")
}

fn find_comment_start(value: &str) -> Option<usize> {
    let mut in_basic_string = false;
    let mut in_literal_string = false;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' && in_basic_string {
            escaped = true;
        } else if ch == '"' && !in_literal_string {
            in_basic_string = !in_basic_string;
        } else if ch == '\'' && !in_basic_string {
            in_literal_string = !in_literal_string;
        } else if ch == '#' && !in_basic_string && !in_literal_string {
            return Some(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::env;

    use super::{rewrite_api_key_sources, secure_input_with, LazyApiKey};
    use glossa_app::ports::ApiKeyProvider;
    use glossa_core::SecretSource;

    #[test]
    fn lazy_key_should_not_touch_secret_service_before_first_use() {
        let key = LazyApiKey::new(SecretSource::SecretService("provider".into()));

        assert!(
            !key.cached.initialized(),
            "constructing the provider must not resolve; startup would block on a locked keyring"
        );
    }

    #[tokio::test]
    async fn lazy_key_should_resolve_and_cache_on_first_use() {
        let key = LazyApiKey::new(SecretSource::Literal("literal-key".into()));

        assert_eq!(key.api_key().await.expect("should resolve"), "literal-key");
        assert!(key.cached.initialized());
        assert_eq!(
            key.api_key().await.expect("should hit cache"),
            "literal-key"
        );
    }

    #[tokio::test]
    async fn lazy_key_should_retry_after_a_failed_resolve() {
        const VAR: &str = "GLOSSA_TEST_LAZY_RETRY_KEY";
        env::remove_var(VAR);
        let key = LazyApiKey::new(SecretSource::Env(VAR.into()));

        assert!(
            key.api_key().await.is_err(),
            "a missing secret should surface as an error"
        );

        env::set_var(VAR, "arrived-later");
        assert_eq!(
            key.api_key().await.expect("should resolve on retry"),
            "arrived-later",
            "a failed resolve must not be cached, or the daemon would need a restart to recover"
        );
        env::remove_var(VAR);
    }

    #[test]
    fn lazy_key_debug_should_describe_the_source_without_the_value() {
        let key = LazyApiKey::new(SecretSource::Literal("top-secret".into()));
        let rendered = format!("{key:?}");

        assert!(rendered.contains("literal"));
        assert!(!rendered.contains("top-secret"));
    }

    #[test]
    fn rewrite_should_only_replace_selected_api_key_lines() {
        let source = "[provider]\napi_key = \"plain#provider\" # keep\n\n[LLM]\napi_key = \"plain-llm\"\n\n[other]\napi_key = \"untouched\"\n";
        let updated = rewrite_api_key_sources(
            source,
            Some(&SecretSource::SecretService("provider".into())),
            Some(&SecretSource::SecretService("llm".into())),
        );

        assert_eq!(
            updated,
            "[provider]\napi_key = \"secret-service:provider\" # keep\n\n[LLM]\napi_key = \"secret-service:llm\"\n\n[other]\napi_key = \"untouched\"\n"
        );
    }

    #[test]
    fn secure_input_should_store_literal_and_return_reference() {
        let stored = RefCell::new(None);
        let source = secure_input_with("top-secret", "provider", |slot, secret| {
            *stored.borrow_mut() = Some((slot.to_owned(), secret.to_owned()));
            Ok(())
        })
        .expect("literal key should be stored");

        assert_eq!(source, "secret-service:provider");
        assert_eq!(
            stored.into_inner(),
            Some(("provider".into(), "top-secret".into()))
        );
    }

    #[test]
    fn secure_input_should_preserve_environment_reference_without_storing() {
        let source = secure_input_with("env:GROQ_API_KEY", "provider", |_, _| {
            panic!("environment references must not be stored")
        })
        .expect("environment reference should remain supported");

        assert_eq!(source, "env:GROQ_API_KEY");
    }
}
