use serde::{Deserialize, Serialize};

/// Normal transcription response shape returned by OpenAI-compatible APIs.
#[derive(Debug, Deserialize)]
pub struct TranscriptionResponse {
    pub text: String,
}

/// OpenRouter JSON transcription request, used when sending `keyterms`.
#[derive(Debug, Serialize)]
pub struct JsonTranscriptionRequest<'a> {
    pub model: &'a str,
    pub input_audio: InputAudio,
    pub keyterms: &'a [String],
}

/// Base64 audio payload of a JSON transcription request.
#[derive(Debug, Serialize)]
pub struct InputAudio {
    pub data: String,
    pub format: &'static str,
}

/// Chat completion request for OpenAI-compatible APIs.
#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

/// A single message in a chat completion request.
#[derive(Debug, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Chat completion response shape returned by OpenAI-compatible APIs.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    pub choices: Vec<Choice>,
}

/// A single choice in a chat completion response.
#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: Message,
}

/// The message object inside a choice.
#[derive(Debug, Deserialize)]
pub struct Message {
    pub content: String,
}
