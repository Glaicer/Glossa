//! Speech-to-text clients for Groq, OpenAI, OpenRouter, and compatible APIs.

mod base64;
mod client;
mod compatible;
mod dictionary;
mod dto;
mod factory;
mod groq;
mod llm_enhancer;
mod openai;
mod openrouter;
#[cfg(test)]
mod test_support;

pub use self::factory::{build_client, build_text_enhancer};
