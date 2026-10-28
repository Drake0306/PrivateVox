//! Model downloads: resumable, size-verified, with progress (design §4.6).

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// A downloadable model file.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub id: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: Option<&'static str>,
}

/// large-v3-turbo, 5-bit, from the whisper.cpp model repository.
pub const TURBO_Q5_0: ModelSpec = ModelSpec {
    id: "large-v3-turbo",
    file: "ggml-large-v3-turbo-q5_0.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
    bytes: 574_041_195,
    sha256: None,
};

/// large-v3, 5-bit ("Accuracy" tier; downloaded on demand in a later plan).
pub const LARGE_V3_Q5_0: ModelSpec = ModelSpec {
    id: "large-v3",
    file: "ggml-large-v3-q5_0.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q5_0.bin",
    bytes: 1_081_140_203,
    sha256: None,
};

/// distil-large-v3, f16 (the only ggml file published), English only.
pub const DISTIL_LARGE_V3_F16: ModelSpec = ModelSpec {
    id: "distil-large-v3",
    file: "ggml-distil-large-v3.bin",
    url: "https://huggingface.co/distil-whisper/distil-large-v3-ggml/resolve/main/ggml-distil-large-v3.bin",
    bytes: 1_519_521_155,
    sha256: None,
};

/// True when the finished file exists with the expected size.
pub fn is_present(spec: &ModelSpec, dir: &Path) -> bool {
    std::fs::metadata(dir.join(spec.file)).map(|m| m.len() == spec.bytes).unwrap_or(false)
}

/// Downloads `spec` into `dir`, resuming a `.part` file, verifying the size, then renaming into place.
/// `progress(received, total)` is called as bytes arrive.
pub async fn download(spec: &ModelSpec, dir: &Path, mut progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let final_path = dir.join(spec.file);
    if is_present(spec, dir) {
        progress(spec.bytes, spec.bytes);
        return Ok(final_path);
    }
    let part = dir.join(format!("{}.part", spec.file));
    let have = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    let have = if have > spec.bytes { 0 } else { have };

    let client = reqwest::Client::builder().build()?;
    let mut req = client.get(spec.url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req.send().await.context("request model")?.error_for_status().context("model download")?;
    let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let mut file = tokio::fs::OpenOptions::new().create(true).append(resumed).write(true).truncate(!resumed).open(&part).await?;
    let mut received = if resumed { have } else { 0 };
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read model bytes")?;
        file.write_all(&chunk).await?;
        received += chunk.len() as u64;
        progress(received.min(spec.bytes), spec.bytes);
    }
    file.flush().await?;
    drop(file);
    let size = tokio::fs::metadata(&part).await?.len();
    if size != spec.bytes {
        bail!("downloaded size {size} does not match expected size {} for {}", spec.bytes, spec.file);
    }
    tokio::fs::rename(&part, &final_path).await?;
    progress(spec.bytes, spec.bytes);
    Ok(final_path)
}

/// Blocking wrapper for callers without a runtime (tests, worker threads).
pub fn download_blocking(spec: &ModelSpec, dir: &Path, progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(download(spec, dir, progress))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A local HTTP server serving one body with Range support; counts requests.
    fn serve(body: Arc<Vec<u8>>, hits: Arc<AtomicUsize>) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                hits.fetch_add(1, Ordering::SeqCst);
                let range = req.headers().iter().find(|h| h.field.equiv("Range")).map(|h| h.value.to_string());
                let start = range.and_then(|r| r.strip_prefix("bytes=").and_then(|v| v.split('-').next().and_then(|s| s.parse::<usize>().ok()))).unwrap_or(0);
                let slice = body[start..].to_vec();
                let total = body.len();
                let mut resp = tiny_http::Response::from_data(slice);
                if start > 0 {
                    resp = resp.with_status_code(206).with_header(tiny_http::Header::from_bytes("Content-Range", format!("bytes {start}-{}/{total}", total - 1)).unwrap());
                }
                let _ = req.respond(resp);
            }
        });
        format!("http://{addr}/model.bin")
    }

    fn body(n: usize) -> Arc<Vec<u8>> {
        Arc::new((0..n).map(|i| (i % 251) as u8).collect())
    }

    #[test]
    fn download_resumes_partial_file() {
        let data = body(1_000_000);
        let hits = Arc::new(AtomicUsize::new(0));
        let url = serve(data.clone(), hits.clone());
        let dir = tempfile::tempdir().unwrap();
        let spec = ModelSpec { id: "test", file: "model.bin", url: Box::leak(url.into_boxed_str()), bytes: data.len() as u64, sha256: None };
        // Pretend a previous attempt stopped at 400 000 bytes.
        std::fs::write(dir.path().join("model.bin.part"), &data[..400_000]).unwrap();
        let mut last = (0u64, 0u64);
        let path = download_blocking(&spec, dir.path(), |got, total| last = (got, total)).unwrap();
        assert_eq!(path, dir.path().join("model.bin"));
        let mut out = Vec::new();
        std::fs::File::open(&path).unwrap().read_to_end(&mut out).unwrap();
        assert_eq!(out, *data, "resumed file must equal the original");
        assert_eq!(last, (data.len() as u64, data.len() as u64));
        assert!(!dir.path().join("model.bin.part").exists());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn download_verifies_size() {
        let data = body(10_000);
        let url = serve(data.clone(), Arc::new(AtomicUsize::new(0)));
        let dir = tempfile::tempdir().unwrap();
        let spec = ModelSpec { id: "test", file: "model.bin", url: Box::leak(url.into_boxed_str()), bytes: 12_345, sha256: None };
        let err = download_blocking(&spec, dir.path(), |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("size"), "{err}");
        assert!(!dir.path().join("model.bin").exists(), "a wrong-size file must not be renamed into place");
    }

    #[test]
    fn is_present_checks_size() {
        let dir = tempfile::tempdir().unwrap();
        let spec = ModelSpec { id: "test", file: "model.bin", url: "http://unused", bytes: 3, sha256: None };
        assert!(!is_present(&spec, dir.path()));
        std::fs::write(dir.path().join("model.bin"), b"ab").unwrap();
        assert!(!is_present(&spec, dir.path()));
        std::fs::write(dir.path().join("model.bin"), b"abc").unwrap();
        assert!(is_present(&spec, dir.path()));
    }

    #[test]
    fn turbo_spec_matches_hugging_face_file() {
        assert_eq!(TURBO_Q5_0.file, "ggml-large-v3-turbo-q5_0.bin");
        assert_eq!(TURBO_Q5_0.bytes, 574_041_195);
        assert!(TURBO_Q5_0.url.starts_with("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/"));
    }
}
