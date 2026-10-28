//! JSON shapes shared by the host server and the client (design §8.6). PCM travels as base64 f32 LE at 16 kHz.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

pub const DEFAULT_HOST_PORT: u16 = 8765;
/// Five minutes at 16 kHz: the capture cap, so a client cannot post more than one maximum clip.
pub const MAX_PCM_SAMPLES: usize = 16_000 * 300;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GpuInfo {
    pub name: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WhisperInfo {
    pub model: String,
    pub loaded: bool,
    pub resident_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PolishInfo {
    pub enabled: bool,
    pub endpoint_label: Option<String>,
    pub model: Option<String>,
    pub resident_bytes: u64,
}

/// `GET /v1/status`: the host's specifications, as the client's memory strip shows them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StatusReply {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub gpu: GpuInfo,
    pub whisper: WhisperInfo,
    pub polish: PolishInfo,
    pub clients: u32,
}

/// `POST /v1/dictations`: one chunk of a session. `final` marks the last chunk of the dictation.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DictationRequest {
    pub client: String,
    pub session: String,
    pub index: u32,
    #[serde(rename = "final")]
    pub final_: bool,
    pub pcm: String,
    pub language: Option<String>,
    pub translate: bool,
    pub mode: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DictationReply {
    pub text: String,
    pub language: Option<String>,
    pub model: String,
    pub decode_ms: u64,
}

/// `POST /v1/polish`: the client's joined transcript for a session; the host polishes and stores the row.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PolishRequest {
    pub client: String,
    pub session: String,
    pub mode: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PolishReply {
    pub text: String,
    /// `ok`, `unpolished` or `none`.
    pub polish_status: String,
    pub polished_by: Option<String>,
    pub ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteRow {
    pub id: i64,
    pub created_at_ms: i64,
    pub app_name: Option<String>,
    pub mode: String,
    pub model: String,
    pub duration_ms: u32,
    pub raw_text: String,
    pub polished_text: Option<String>,
    pub polish_status: String,
    pub engine_kind: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HistoryReply {
    pub rows: Vec<RemoteRow>,
    pub total: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteModel {
    pub id: String,
    pub label: String,
    pub downloaded: bool,
    pub active: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ModelsReply {
    pub models: Vec<RemoteModel>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SelectModelRequest {
    pub id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ErrorReply {
    pub error: String,
}

pub fn encode_pcm(pcm: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(pcm.len() * 4);
    for s in pcm {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode_pcm(b64: &str) -> Result<Vec<f32>, String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|e| format!("pcm is not valid base64: {e}"))?;
    if bytes.len() % 4 != 0 {
        return Err(format!("pcm byte length {} is not a multiple of 4 (expected f32 little-endian samples)", bytes.len()));
    }
    if bytes.len() / 4 > MAX_PCM_SAMPLES {
        return Err(format!("pcm has {} samples; the cap is 5 minutes at 16 kHz ({MAX_PCM_SAMPLES})", bytes.len() / 4));
    }
    Ok(bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_round_trips() {
        let v = vec![0.0f32, 0.5, -1.0, 1.0e-3];
        assert_eq!(decode_pcm(&encode_pcm(&v)).unwrap(), v);
        assert_eq!(decode_pcm(&encode_pcm(&[])).unwrap(), Vec::<f32>::new());
    }

    #[test]
    fn decode_rejects_bad_payloads() {
        assert!(decode_pcm("not base64!!").is_err());
        assert!(decode_pcm(&base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3])).unwrap_err().contains("multiple of 4"));
        let too_long = vec![0u8; (MAX_PCM_SAMPLES + 1) * 4];
        assert!(decode_pcm(&base64::engine::general_purpose::STANDARD.encode(too_long)).unwrap_err().contains("5 minutes"));
    }

    #[test]
    fn dictation_request_uses_final_on_the_wire() {
        let r = DictationRequest { client: "Mac".into(), session: "s1".into(), index: 0, final_: true, pcm: "".into(), language: Some("en".into()), translate: false, mode: "clean".into() };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"final\":true"), "{j}");
        assert_eq!(serde_json::from_str::<DictationRequest>(&j).unwrap(), r);
    }

    #[test]
    fn error_reply_shape() {
        assert_eq!(serde_json::to_string(&ErrorReply { error: "x".into() }).unwrap(), r#"{"error":"x"}"#);
    }

    #[test]
    fn status_reply_round_trips() {
        let s = StatusReply {
            name: "PC".into(),
            version: "0.3.0".into(),
            platform: "windows".into(),
            gpu: GpuInfo { name: "RX 9070".into(), total_bytes: 16_000_000_000, used_bytes: 5_000_000_000 },
            whisper: WhisperInfo { model: "large-v3-turbo".into(), loaded: true, resident_bytes: 1_300_000_000 },
            polish: PolishInfo { enabled: true, endpoint_label: Some("LM Studio".into()), model: Some("qwen2.5-7b".into()), resident_bytes: 5_000_000_000 },
            clients: 1,
        };
        assert_eq!(serde_json::from_str::<StatusReply>(&serde_json::to_string(&s).unwrap()).unwrap(), s);
        assert_eq!(DEFAULT_HOST_PORT, 8765);
    }
}
