//! Messages between the app and the engine worker, framed as u32 little-endian length + postcard bytes.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// Per-run decode settings the app chooses (the backend fills in the fixed defaults).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodeOptions {
    pub language: Option<String>,
    pub translate: bool,
    pub initial_prompt: Option<String>,
    pub n_threads: i32,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions { language: Some("en".into()), translate: false, initial_prompt: None, n_threads: 4 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Ping,
    Load { model_path: PathBuf, flash_attn: bool, use_gpu: bool },
    Unload,
    Transcribe { id: u64, pcm: Vec<f32>, opts: DecodeOptions },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Pong,
    Loaded { load_ms: u64, resident_bytes: u64 },
    Unloaded,
    Transcript { id: u64, text: String, language: Option<String>, decode_ms: u64, audio_ctx: i32, reran: bool },
    Error { id: Option<u64>, message: String },
}

/// Writes one length-prefixed frame and flushes.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let bytes = postcard::to_allocvec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Reads one frame. `Ok(None)` means a clean EOF at a frame boundary (the peer went away).
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_le_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    postcard::from_bytes(&buf).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_through_vec() {
        let req = Request::Transcribe {
            id: 7,
            pcm: vec![0.1, -0.2, 0.3],
            opts: DecodeOptions { language: Some("en".into()), translate: false, initial_prompt: None, n_threads: 4 },
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        assert_eq!(u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize, buf.len() - 4);
        let mut cursor = std::io::Cursor::new(buf);
        let back: Option<Request> = read_frame(&mut cursor).unwrap();
        match back {
            Some(Request::Transcribe { id, pcm, opts }) => {
                assert_eq!(id, 7);
                assert_eq!(pcm, vec![0.1, -0.2, 0.3]);
                assert_eq!(opts.language.as_deref(), Some("en"));
                assert_eq!(opts.n_threads, 4);
            }
            _ => panic!("wrong message"),
        }
    }

    #[test]
    fn frame_roundtrip_loaded_with_resident_bytes() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &Response::Loaded { load_ms: 480, resident_bytes: 577_000_000 }).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        match read_frame::<_, Response>(&mut cursor).unwrap() {
            Some(Response::Loaded { load_ms, resident_bytes }) => {
                assert_eq!(load_ms, 480);
                assert_eq!(resident_bytes, 577_000_000);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn read_frame_returns_none_at_eof() {
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        let r: Option<Response> = read_frame(&mut cursor).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &Response::Pong).unwrap();
        buf.truncate(buf.len() - 1);
        let mut cursor = std::io::Cursor::new(buf);
        let r: std::io::Result<Option<Response>> = read_frame(&mut cursor);
        assert!(r.is_err());
    }
}
