//! Speech-to-text clients for Groq, OpenAI, OpenRouter, and compatible APIs.

mod client;
mod compatible;
mod dto;
mod factory;
mod groq;
mod llm_enhancer;
mod openai;
mod openrouter;

pub use self::factory::{build_client, build_text_enhancer};
