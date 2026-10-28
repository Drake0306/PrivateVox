//! The three Whisper tiers PrivateVox ships: what to download, where it lives, what it costs (design §4.6).

use crate::download::{ModelSpec, DISTIL_LARGE_V3_F16, LARGE_V3_Q5_0, TURBO_Q5_0};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub file: &'static str,
    pub bytes: u64,
    pub quant: &'static str,
    pub english_only: bool,
    pub spec: ModelSpec,
}

/// The catalog, in panel order: fast default, accuracy, English-only.
pub fn catalog() -> Vec<ModelInfo> {
    vec![
        ModelInfo { id: "large-v3-turbo", label: "Fast · large-v3-turbo", file: TURBO_Q5_0.file, bytes: TURBO_Q5_0.bytes, quant: "q5_0", english_only: false, spec: TURBO_Q5_0 },
        ModelInfo { id: "large-v3", label: "Accuracy · large-v3", file: LARGE_V3_Q5_0.file, bytes: LARGE_V3_Q5_0.bytes, quant: "q5_0", english_only: false, spec: LARGE_V3_Q5_0 },
        ModelInfo { id: "distil-large-v3", label: "English · distil-large-v3", file: DISTIL_LARGE_V3_F16.file, bytes: DISTIL_LARGE_V3_F16.bytes, quant: "f16", english_only: true, spec: DISTIL_LARGE_V3_F16 },
    ]
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
    fn catalog_has_three_tiers_with_spec_sizes() {
        let c = catalog();
        assert_eq!(c.len(), 3);
        let turbo = c.iter().find(|m| m.id == "large-v3-turbo").unwrap();
        assert_eq!(turbo.bytes, 574_041_195);
        assert_eq!(turbo.quant, "q5_0");
        assert!(!turbo.english_only);
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
    fn model_file_uses_catalog_filename() {
        let p = model_file(std::path::Path::new("/data/models"), "distil-large-v3").unwrap();
        assert_eq!(p, std::path::PathBuf::from("/data/models/ggml-distil-large-v3.bin"));
        assert!(model_file(std::path::Path::new("/data/models"), "nope").is_none());
    }
}
