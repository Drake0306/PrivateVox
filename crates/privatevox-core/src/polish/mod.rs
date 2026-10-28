//! LLM polish (design §7, §8): modes and prompts, request shapes, the endpoint registry, the HTTP client and the orchestrator.

pub mod modes;
pub mod polisher;
pub mod client;
pub mod endpoints;
#[cfg(any(test, feature = "test-support"))]
pub mod fake_server;
pub mod request;

pub use client::{HttpLlmClient, LlmClient, Probe};
pub use endpoints::{backoff, endpoint_id, is_local, local_candidates, normalize_base_url, ordered, Endpoint, Health, Registry, Source};
pub use polisher::{PolishOutcome, PolishStatus, Polisher};
pub use modes::{default_prompt, effective_prompt, Mode};
pub use request::{chat_body, chat_path, clean_output, max_tokens_for, parse_chat_reply, unload_body, warm_body, Flavor};
