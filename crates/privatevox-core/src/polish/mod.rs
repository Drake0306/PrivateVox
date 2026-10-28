//! LLM polish (design §7, §8): modes and prompts, request shapes, the endpoint registry, the HTTP client and the orchestrator.

pub mod modes;
pub mod request;

pub use modes::{default_prompt, effective_prompt, Mode};
pub use request::{chat_body, chat_path, clean_output, max_tokens_for, parse_chat_reply, unload_body, warm_body, Flavor};
