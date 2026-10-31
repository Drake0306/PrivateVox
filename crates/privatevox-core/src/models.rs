//! The three Whisper tiers PrivateVox ships: what to download, where it lives, what it costs (design §4.6).

use crate::download::{ModelSpec, DISTIL_LARGE_V3_F16, LARGE_V3_Q5_0, TURBO_Q5_0, TURBO_Q8_0};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub file: &'static str,
    pub bytes: u64,
    pub quant: &'static str,
    pub english_only: bool,
    /// Shown in the picker only on machines with at least this much memory (0: always). A file the user already
    /// has is always shown, see `offered`.
    pub offered_from_bytes: u64,
    pub spec: ModelSpec,
}

/// 12 GB: the Q8_0 turbo is only worth its extra memory where it comfortably fits next to an LLM (design §4.6).
pub const Q8_OFFERED_FROM_BYTES: u64 = 12_000_000_000;

/// The catalog, in panel order: fast default, its 8-bit variant, accuracy, English-only.
pub fn catalog() -> Vec<ModelInfo> {
    vec![
        ModelInfo { id: "large-v3-turbo", label: "Fast · large-v3-turbo", file: TURBO_Q5_0.file, bytes: TURBO_Q5_0.bytes, quant: "q5_0", english_only: false, offered_from_bytes: 0, spec: TURBO_Q5_0 },
        ModelInfo { id: "large-v3-turbo-q8", label: "Fast, higher precision · large-v3-turbo Q8", file: TURBO_Q8_0.file, bytes: TURBO_Q8_0.bytes, quant: "q8_0", english_only: false, offered_from_bytes: Q8_OFFERED_FROM_BYTES, spec: TURBO_Q8_0 },
        ModelInfo { id: "large-v3", label: "Accuracy · large-v3", file: LARGE_V3_Q5_0.file, bytes: LARGE_V3_Q5_0.bytes, quant: "q5_0", english_only: false, offered_from_bytes: 0, spec: LARGE_V3_Q5_0 },
        ModelInfo { id: "distil-large-v3", label: "English · distil-large-v3", file: DISTIL_LARGE_V3_F16.file, bytes: DISTIL_LARGE_V3_F16.bytes, quant: "f16", english_only: true, offered_from_bytes: 0, spec: DISTIL_LARGE_V3_F16 },
    ]
}

/// Whether a catalog entry is shown on a machine with `total_bytes` of memory. A model whose file is already
/// present is always shown, so a file downloaded elsewhere (or on a bigger machine) still loads and can be deleted.
pub fn offered(info: &ModelInfo, total_bytes: u64, present: bool) -> bool {
    present || total_bytes >= info.offered_from_bytes
}

/// The catalog entries shown on this machine (`offered` applied with the given memory and models directory).
pub fn offered_catalog(models_dir: &Path, total_bytes: u64) -> Vec<ModelInfo> {
    catalog().into_iter().filter(|m| offered(m, total_bytes, crate::download::is_present(&m.spec, models_dir))).collect()
}

pub fn info(id: &str) -> Option<ModelInfo> {
    catalog().into_iter().find(|m| m.id == id)
}

/// Where a model's file lives, by catalog filename.
pub fn model_file(models_dir: &Path, id: &str) -> Option<PathBuf> {
    info(id).map(|m| models_dir.join(m.file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_four_entries_with_spec_sizes() {
        let c = catalog();
        assert_eq!(c.len(), 4);
        let turbo = c.iter().find(|m| m.id == "large-v3-turbo").unwrap();
        assert_eq!(turbo.bytes, 574_041_195);
        assert_eq!(turbo.quant, "q5_0");
        assert!(!turbo.english_only);
        let q8 = c.iter().find(|m| m.id == "large-v3-turbo-q8").unwrap();
        assert_eq!(q8.bytes, 874_188_075);
        assert_eq!(q8.quant, "q8_0");
        assert_eq!(q8.file, "ggml-large-v3-turbo-q8_0.bin");
        assert_eq!(q8.spec.url, "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin");
        assert_eq!(q8.offered_from_bytes, 12_000_000_000);
        let v3 = c.iter().find(|m| m.id == "large-v3").unwrap();
        assert_eq!(v3.bytes, 1_081_140_203);
        let distil = c.iter().find(|m| m.id == "distil-large-v3").unwrap();
        assert_eq!(distil.bytes, 1_519_521_155);
        assert_eq!(distil.quant, "f16");
        assert!(distil.english_only);
        assert_eq!(distil.file, "ggml-distil-large-v3.bin");
    }

    #[test]
    fn distil_spec_matches_hf() {
        let d = crate::download::DISTIL_LARGE_V3_F16;
        assert_eq!(d.url, "https://huggingface.co/distil-whisper/distil-large-v3-ggml/resolve/main/ggml-distil-large-v3.bin");
        assert_eq!(d.bytes, 1_519_521_155);
    }

    #[test]
    fn q8_is_hidden_below_12gb_unless_present() {
        let q8 = info("large-v3-turbo-q8").unwrap();
        let gb = |n: u64| n * 1_000_000_000;
        assert!(!offered(&q8, gb(8), false), "8 GB: not offered");
        assert!(offered(&q8, gb(16), false), "16 GB: offered");
        assert!(offered(&q8, gb(12), false), "exactly 12 GB: offered");
        assert!(offered(&q8, gb(8), true), "8 GB with the file present: still shown, so it loads and can be deleted");
        for id in ["large-v3-turbo", "large-v3", "distil-large-v3"] {
            assert!(offered(&info(id).unwrap(), gb(8), false), "{id} is offered everywhere");
        }
        // The directory-aware variant: a stray Q8 file on an 8 GB machine keeps it in the list.
        let d = tempfile::tempdir().unwrap();
        assert_eq!(offered_catalog(d.path(), gb(8)).len(), 3);
        assert_eq!(offered_catalog(d.path(), gb(32)).len(), 4);
        let f = std::fs::File::create(d.path().join(q8.file)).unwrap();
        f.set_len(q8.bytes).unwrap();
        assert_eq!(offered_catalog(d.path(), gb(8)).len(), 4);
    }

    #[test]
    fn model_file_uses_catalog_filename() {
        let p = model_file(std::path::Path::new("/data/models"), "distil-large-v3").unwrap();
        assert_eq!(p, std::path::PathBuf::from("/data/models/ggml-distil-large-v3.bin"));
        assert!(model_file(std::path::Path::new("/data/models"), "nope").is_none());
    }
}
