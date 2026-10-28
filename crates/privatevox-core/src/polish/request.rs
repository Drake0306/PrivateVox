//! Chat request and reply shapes for OpenAI-compatible servers and Ollama's native API (design §7, §4.3 step 5).

use super::modes::Mode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Which API dialect an endpoint speaks. Ollama gets its native chat so `keep_alive` applies (§4.3 5b).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flavor {
    Ollama,
    OpenAi,
}

/// Rough token count: four tokens per three words, plus a little slack.
pub fn estimate_tokens(text: &str) -> u32 {
    let words = text.split_whitespace().count() as u32;
    (words * 4).div_ceil(3) + 8
}

/// Spec §7: `max_tokens` sized to 2× the input tokens plus 64.
pub fn max_tokens_for(input: &str) -> u32 {
    2 * estimate_tokens(input) + 64
}

/// Polish modes rewrite the input, so §7's formula fits; Ask answers and Code expands, so they get a floor.
pub fn max_tokens_for_mode(mode: Mode, input: &str) -> u32 {
    let base = max_tokens_for(input);
    match mode {
        Mode::Ask => base.max(512),
        Mode::Code => base.max(256),
        _ => base,
    }
}

pub fn chat_path(flavor: Flavor) -> &'static str {
    match flavor {
        Flavor::Ollama => "/api/chat",
        Flavor::OpenAi => "/v1/chat/completions",
    }
}

pub fn chat_body(flavor: Flavor, model: &str, system: &str, user: &str, max_tokens: u32) -> Value {
    let messages = json!([{ "role": "system", "content": system }, { "role": "user", "content": user }]);
    match flavor {
        Flavor::OpenAi => json!({ "model": model, "messages": messages, "temperature": 0.2, "max_tokens": max_tokens, "stream": false }),
        Flavor::Ollama => json!({ "model": model, "messages": messages, "stream": false, "keep_alive": -1, "options": { "temperature": 0.2, "num_predict": max_tokens } }),
    }
}

pub fn parse_chat_reply(flavor: Flavor, body: &Value) -> Result<String, String> {
    if let Some(err) = body.get("error") {
        let msg = err.get("message").and_then(|m| m.as_str()).or_else(|| err.as_str()).unwrap_or("unknown error");
        return Err(format!("server error: {msg}"));
    }
    let content = match flavor {
        Flavor::OpenAi => body.pointer("/choices/0/message/content"),
        Flavor::Ollama => body.pointer("/message/content"),
    };
    content.and_then(|c| c.as_str()).map(|s| s.to_string()).ok_or_else(|| "no content in reply".to_string())
}

fn strip_wrapping_quotes(s: &str) -> &str {
    let pairs = [('"', '"'), ('\u{201c}', '\u{201d}'), ('\u{2018}', '\u{2019}')];
    for (open, close) in pairs {
        if s.starts_with(open) && s.ends_with(close) && s.chars().count() >= 2 {
            let inner = &s[open.len_utf8()..s.len() - close.len_utf8()];
            // Only a wrapper when the inside has no further quote of the same kind.
            if !inner.contains(open) && !inner.contains(close) {
                return inner.trim();
            }
        }
    }
    s
}

fn strip_wrapping_fence(s: &str) -> &str {
    if s.len() >= 3 && s.bytes().all(|b| b == b'`') {
        return ""; // nothing but fences: an empty reply, which the polisher treats as unpolished
    }
    let Some(body) = s.strip_prefix("```").and_then(|b| b.strip_suffix("```")) else { return s };
    // Optional language tag on the opening line.
    let body = match body.find('\n') {
        Some(i) if body[..i].chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') => &body[i + 1..],
        _ => body,
    };
    body.trim()
}

/// A first line like "Here is the cleaned text:" is chatter, not content. Only lines made of such words qualify:
/// code (`def f():`), YAML keys and headings keep their first line.
fn is_preamble(line: &str) -> bool {
    let line = line.trim_end();
    if !line.ends_with(':') || line.split_whitespace().count() > 8 {
        return false;
    }
    let words: Vec<String> = line.trim_end_matches(':').split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_lowercase()).collect();
    const CHATTER: &[&str] = &["here", "here's", "is", "are", "the", "your", "a", "an", "sure", "okay", "ok", "certainly", "of", "course", "cleaned", "clean", "polished", "corrected", "edited", "revised", "rewritten", "fixed", "final", "updated", "text", "version", "result", "output", "transcript", "answer", "email", "bullets", "list", "code", "translation"];
    !words.is_empty() && words.iter().all(|w| CHATTER.contains(&w.as_str()))
}

fn strip_preamble_line(s: &str) -> &str {
    if let Some((first, rest)) = s.split_once('\n') {
        if is_preamble(first) && !rest.trim().is_empty() {
            return rest.trim_start();
        }
    }
    s
}

/// Trim, drop a wrapping quote pair or code fence, drop a short "Here is …:" preamble line.
pub fn clean_output(s: &str) -> String {
    let s = s.trim();
    let s = strip_wrapping_fence(s);
    let s = strip_preamble_line(s);
    let s = strip_wrapping_quotes(s);
    s.trim().to_string()
}

/// Ollama preloads a model with an empty generate request; OpenAI-style servers have no such call.
pub fn warm_body(flavor: Flavor, model: &str) -> Option<Value> {
    match flavor {
        Flavor::Ollama => Some(json!({ "model": model, "keep_alive": -1 })),
        Flavor::OpenAi => None,
    }
}

/// Ollama unload: `keep_alive: 0` on a generate request.
pub fn unload_body(model: &str) -> Value {
    json!({ "model": model, "keep_alive": 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn max_tokens_is_twice_input_plus_64() {
        assert_eq!(estimate_tokens("one two three"), 12);
        assert_eq!(max_tokens_for("one two three"), 88);
    }

    #[test]
    fn openai_body_shape() {
        let b = chat_body(Flavor::OpenAi, "m", "sys", "hi", 84);
        assert_eq!(b["model"], "m");
        assert_eq!(b["temperature"], 0.2);
        assert_eq!(b["stream"], false);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][0]["content"], "sys");
        assert_eq!(b["messages"][1]["role"], "user");
        assert_eq!(b["messages"][1]["content"], "hi");
        assert_eq!(b["max_tokens"], 84);
        assert_eq!(chat_path(Flavor::OpenAi), "/v1/chat/completions");
    }

    #[test]
    fn ollama_body_keeps_alive_forever() {
        let b = chat_body(Flavor::Ollama, "m", "sys", "hi", 84);
        assert_eq!(b["keep_alive"], -1);
        assert_eq!(b["stream"], false);
        assert_eq!(b["options"]["num_predict"], 84);
        assert_eq!(b["options"]["temperature"], 0.2);
        assert_eq!(chat_path(Flavor::Ollama), "/api/chat");
    }

    #[test]
    fn parses_both_reply_shapes() {
        assert_eq!(parse_chat_reply(Flavor::OpenAi, &json!({"choices":[{"message":{"content":"a"}}]})).unwrap(), "a");
        assert_eq!(parse_chat_reply(Flavor::Ollama, &json!({"message":{"content":"b"}})).unwrap(), "b");
        assert!(parse_chat_reply(Flavor::OpenAi, &json!({})).is_err());
        assert!(parse_chat_reply(Flavor::Ollama, &json!({"error":"model not found"})).unwrap_err().contains("model not found"));
    }

    #[test]
    fn clean_output_strips_wrappers() {
        assert_eq!(clean_output("\"Hello there.\""), "Hello there.");
        assert_eq!(clean_output("“Hello there.”"), "Hello there.");
        assert_eq!(clean_output("```text\nHello\n```"), "Hello");
        assert_eq!(clean_output("```\nlet x = 1;\n```"), "let x = 1;");
        assert_eq!(clean_output("Here is the cleaned text:\nHello"), "Hello");
        assert_eq!(clean_output("Note: keep this"), "Note: keep this");
        assert_eq!(clean_output("  padded  "), "padded");
        assert_eq!(clean_output("He said \"hi\" and left."), "He said \"hi\" and left.");
    }

    #[test]
    fn unload_body_is_keep_alive_zero() {
        assert_eq!(unload_body("m")["keep_alive"], 0);
        assert_eq!(unload_body("m")["model"], "m");
        assert!(warm_body(Flavor::OpenAi, "m").is_none());
        assert_eq!(warm_body(Flavor::Ollama, "m").unwrap()["keep_alive"], -1);
    }

    #[test]
    fn preamble_rule_keeps_code_and_headings() {
        assert_eq!(clean_output("```python\ndef add(a, b):\n    return a + b\n```"), "def add(a, b):\n    return a + b");
        assert_eq!(clean_output("Shopping list:\n- milk\n- eggs"), "Shopping list:\n- milk\n- eggs");
        assert_eq!(clean_output("services:\n  web:\n    image: nginx"), "services:\n  web:\n    image: nginx");
        assert_eq!(clean_output("Here's the polished version:\nHello."), "Hello.");
        assert_eq!(clean_output("Sure! Here is the cleaned text:\nHello."), "Hello.");
        assert_eq!(clean_output("Cleaned text:\nHello."), "Hello.");
    }

    #[test]
    fn lone_fences_do_not_panic() {
        assert_eq!(clean_output("```"), "");
        assert_eq!(clean_output("````"), "");
        assert_eq!(clean_output("`````"), "");
        assert_eq!(clean_output("``````"), "");
    }

    #[test]
    fn ask_mode_gets_a_real_answer_budget() {
        use crate::polish::Mode;
        assert_eq!(max_tokens_for_mode(Mode::Clean, "one two three"), 88);
        assert!(max_tokens_for_mode(Mode::Ask, "explain tcp") >= 512);
        assert!(max_tokens_for_mode(Mode::Code, "write a function that adds") >= 256);
    }
}
