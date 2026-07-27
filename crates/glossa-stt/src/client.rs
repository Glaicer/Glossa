use std::sync::Arc;

use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use reqwest::StatusCode;
use tokio::fs;
use tracing::debug;

use glossa_app::{
    ports::{ApiKeyProvider, SttClient},
    AppError,
};
use glossa_core::{CapturedAudio, ProviderConfig, ProviderKind};

use crate::dto::TranscriptionResponse;

/// Shared OpenAI-style multipart transcription client.
#[derive(Debug, Clone)]
pub struct HttpSttClient {
    provider_name: &'static str,
    endpoint: String,
    model: String,
    api_key: Arc<dyn ApiKeyProvider>,
    client: reqwest::Client,
}

impl HttpSttClient {
    #[must_use]
    pub fn new(
        provider_name: &'static str,
        endpoint: String,
        model: String,
        api_key: Arc<dyn ApiKeyProvider>,
    ) -> Self {
        Self {
            provider_name,
            endpoint,
            model,
            api_key,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl SttClient for HttpSttClient {
    fn provider_name(&self) -> &'static str {
        self.provider_name
    }

    async fn transcribe(&self, audio: &CapturedAudio) -> Result<String, AppError> {
        debug!(
            provider = self.provider_name,
            path = %audio.path,
            duration_ms = audio.duration_ms,
            "uploading captured audio for transcription"
        );
        let bytes = fs::read(audio.path.as_std_path())
            .await
            .map_err(|error| AppError::io("failed to read captured audio", error))?;
        let filename = audio.path.file_name().unwrap_or("capture.wav").to_string();
        let mime = match audio.path.extension() {
            Some("wav") => "audio/wav",
            Some("flac") => "audio/flac",
            _ => "application/octet-stream",
        };
        let file_part = Part::bytes(bytes)
            .file_name(filename)
            .mime_str(mime)
            .map_err(|error| {
                AppError::message(format!("failed to encode upload mime type: {error}"))
            })?;
        let form = Form::new()
            .text("model", self.model.clone())
            .part("file", file_part);

        let api_key = self.api_key.api_key().await?;
        let mut request = self.client.post(&self.endpoint);
        if !api_key.is_empty() {
            request = request.bearer_auth(&api_key);
        }

        let response =
            request.multipart(form).send().await.map_err(|error| {
                AppError::message(format!("transcription request failed: {error}"))
            })?;

        let status = response.status();
        if status != StatusCode::OK {
            return Err(AppError::message(http_status_error_message(status)));
        }

        let parsed: TranscriptionResponse = response.json().await.map_err(|error| {
            AppError::message(format!("failed to decode transcription response: {error}"))
        })?;
        Ok(parsed.text)
    }
}

fn http_status_error_message(status: StatusCode) -> String {
    let code = status.as_u16();
    let (label, description) = match code {
        206 => Some((
            "Partial Content",
            "the provider returned only a partial response for the transcription request",
        )),
        400 => Some((
            "Bad Request",
            "the request was invalid or malformed",
        )),
        401 => Some((
            "Unauthorized",
            "authentication failed or the API key is invalid",
        )),
        403 => Some((
            "Forbidden",
            "the request is not allowed, possibly due to permissions or region restrictions",
        )),
        404 => Some((
            "Not Found",
            "the requested resource, model, or endpoint could not be found",
        )),
        409 => Some((
            "Conflict",
            "the request conflicted with the provider's current state; retry the request",
        )),
        413 => Some((
            "Request Entity Too Large",
            "the audio payload is too large for the provider to accept",
        )),
        422 => Some((
            "Unprocessable Entity",
            "the request was well-formed but could not be processed because the input was semantically invalid",
        )),
        424 => Some((
            "Failed Dependency",
            "an upstream or dependent service failed while processing the request",
        )),
        429 => Some((
            "Too Many Requests",
            "rate limit or quota exceeded",
        )),
        498 => Some((
            "Flex Tier Capacity Exceeded",
            "the provider does not currently have enough capacity to process the request",
        )),
        499 => Some((
            "Request Cancelled",
            "the request was cancelled before the provider could finish processing it",
        )),
        500 => Some((
            "Internal Server Error",
            "the provider encountered an internal error",
        )),
        502 => Some((
            "Bad Gateway",
            "the provider received an invalid upstream response",
        )),
        503 => Some((
            "Service Unavailable",
            "the provider is temporarily unavailable or overloaded",
        )),
        504 => Some((
            "Gateway Timeout",
            "the provider timed out while waiting for an upstream response",
        )),
        _ => None,
    }
    .unwrap_or_else(|| (status.canonical_reason().unwrap_or("Unknown Status"), ""));

    if description.is_empty() {
        format!("transcription request failed with status {code} {label}")
    } else {
        format!("transcription request failed with status {code} {label}: {description}")
    }
}

#[must_use]
pub fn build_http_client(
    config: &ProviderConfig,
    api_key: Arc<dyn ApiKeyProvider>,
    provider_name: &'static str,
) -> Arc<dyn SttClient> {
    let base_url = config
        .base_url
        .clone()
        .unwrap_or_else(|| match config.kind {
            ProviderKind::Groq => "https://api.groq.com/openai/v1".into(),
            ProviderKind::OpenAi => "https://api.openai.com/v1".into(),
            ProviderKind::OpenAiCompatible => unreachable!("validated elsewhere"),
        });
    let endpoint = format!("{}/audio/transcriptions", base_url.trim_end_matches('/'));
    Arc::new(HttpSttClient::new(
        provider_name,
        endpoint,
        config.model.clone(),
        api_key,
    ))
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::http_status_error_message;

    #[test]
    fn known_status_should_render_code_and_description() {
        assert_eq!(
            http_status_error_message(StatusCode::TOO_MANY_REQUESTS),
            "transcription request failed with status 429 Too Many Requests: rate limit or quota exceeded"
        );
    }

    #[test]
    fn custom_status_should_render_code_and_description() {
        let status = StatusCode::from_u16(498).expect("status should be valid");
        assert_eq!(
            http_status_error_message(status),
            "transcription request failed with status 498 Flex Tier Capacity Exceeded: the provider does not currently have enough capacity to process the request"
        );
    }

    #[test]
    fn documented_statuses_should_use_expected_endpoint_agnostic_messages() {
        let cases = [
            (
                206,
                "transcription request failed with status 206 Partial Content: the provider returned only a partial response for the transcription request",
            ),
            (
                400,
                "transcription request failed with status 400 Bad Request: the request was invalid or malformed",
            ),
            (
                401,
                "transcription request failed with status 401 Unauthorized: authentication failed or the API key is invalid",
            ),
            (
                403,
                "transcription request failed with status 403 Forbidden: the request is not allowed, possibly due to permissions or region restrictions",
            ),
            (
                404,
                "transcription request failed with status 404 Not Found: the requested resource, model, or endpoint could not be found",
            ),
            (
                409,
                "transcription request failed with status 409 Conflict: the request conflicted with the provider's current state; retry the request",
            ),
            (
                413,
                "transcription request failed with status 413 Request Entity Too Large: the audio payload is too large for the provider to accept",
            ),
            (
                422,
                "transcription request failed with status 422 Unprocessable Entity: the request was well-formed but could not be processed because the input was semantically invalid",
            ),
            (
                424,
                "transcription request failed with status 424 Failed Dependency: an upstream or dependent service failed while processing the request",
            ),
            (
                429,
                "transcription request failed with status 429 Too Many Requests: rate limit or quota exceeded",
            ),
            (
                499,
                "transcription request failed with status 499 Request Cancelled: the request was cancelled before the provider could finish processing it",
            ),
            (
                500,
                "transcription request failed with status 500 Internal Server Error: the provider encountered an internal error",
            ),
            (
                502,
                "transcription request failed with status 502 Bad Gateway: the provider received an invalid upstream response",
            ),
            (
                503,
                "transcription request failed with status 503 Service Unavailable: the provider is temporarily unavailable or overloaded",
            ),
            (
                504,
                "transcription request failed with status 504 Gateway Timeout: the provider timed out while waiting for an upstream response",
            ),
        ];

        for (code, expected) in cases {
            let status = StatusCode::from_u16(code).expect("status should be valid");
            assert_eq!(http_status_error_message(status), expected);
        }
    }

    #[test]
    fn unknown_status_should_fall_back_to_reason_phrase() {
        assert_eq!(
            http_status_error_message(StatusCode::IM_A_TEAPOT),
            "transcription request failed with status 418 I'm a teapot"
        );
    }
}
