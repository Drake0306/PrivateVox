//! Memory budget for the GPU strip: what the selected model and LLM will take, and which tiers fit (design §9).

use serde::Serialize;
use std::collections::HashMap;

/// A Whisper tier. Lower `accuracy_rank` is more accurate; lower `speed_rank` is faster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tier {
    pub id: &'static str,
    pub label: &'static str,
    pub accuracy_rank: u8,
    pub speed_rank: u8,
}

pub const TIERS: [Tier; 3] = [
    Tier { id: "large-v3", label: "Accuracy (large-v3)", accuracy_rank: 0, speed_rank: 2 },
    Tier { id: "large-v3-turbo", label: "Fast (large-v3-turbo)", accuracy_rank: 1, speed_rank: 0 },
    Tier { id: "distil-large-v3", label: "English (distil-large-v3)", accuracy_rank: 2, speed_rank: 1 },
];

/// Planning estimates until a tier has been loaded once and measured.
pub fn estimate_footprint(model_id: &str) -> u64 {
    match model_id {
        "large-v3-turbo" => 550_000_000,
        "large-v3" => 1_000_000_000,
        "distil-large-v3" => 1_600_000_000,
        _ => 1_000_000_000,
    }
}

pub struct MemoryInput {
    pub total: u64,
    pub used_by_others: u64,
    /// Measured bytes per model id; missing ids use `estimate_footprint`.
    pub footprints: HashMap<String, u64>,
    pub llm_bytes: u64,
    pub selected: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Good,
    Tight,
    WontFit,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryReport {
    pub total: u64,
    pub used_by_others: u64,
    pub selected: String,
    pub selected_bytes: u64,
    pub llm_bytes: u64,
    pub projected: u64,
    pub percent: u8,
    pub verdict: Verdict,
    pub recommended: Option<String>,
    pub best: Option<String>,
    pub free_after: u64,
    /// Footprint used for each tier (measured or estimate), for the tiles.
    pub tier_bytes: HashMap<String, u64>,
    /// Set when the figures describe a host on the LAN rather than this machine ("On PC").
    pub source_label: Option<String>,
    /// The host's adapter name, when the figures come from a host.
    pub adapter: Option<String>,
}

fn footprint(input: &MemoryInput, id: &str) -> u64 {
    input.footprints.get(id).copied().unwrap_or_else(|| estimate_footprint(id))
}

fn fits(input: &MemoryInput, id: &str) -> bool {
    let projected = input.used_by_others + footprint(input, id) + input.llm_bytes;
    projected as f64 <= input.total as f64 * 0.8
}

/// The strip's numbers for a host on the LAN, from its `/v1/status` (design §8.6, §9): the host's Whisper and
/// polish models are "ours", everything else it uses is "other apps".
pub fn budget_from_host(status: &crate::net::StatusReply, label: &str) -> MemoryReport {
    let mut footprints = HashMap::new();
    if status.whisper.resident_bytes > 0 {
        footprints.insert(status.whisper.model.clone(), status.whisper.resident_bytes);
    }
    let input = MemoryInput {
        total: status.gpu.total_bytes,
        used_by_others: status.gpu.used_bytes.saturating_sub(status.whisper.resident_bytes).saturating_sub(status.polish.resident_bytes),
        footprints,
        llm_bytes: status.polish.resident_bytes,
        selected: status.whisper.model.clone(),
    };
    let mut report = budget(&input);
    report.source_label = Some(label.to_string());
    report.adapter = Some(status.gpu.name.clone());
    report
}

/// The strip's numbers and verdicts (design §9).
pub fn budget(input: &MemoryInput) -> MemoryReport {
    let selected_bytes = footprint(input, &input.selected);
    let projected = input.used_by_others + selected_bytes + input.llm_bytes;
    let ratio = if input.total == 0 { 1.0 } else { projected as f64 / input.total as f64 };
    let percent = (ratio * 100.0).round().min(999.0) as u8;
    let verdict = if ratio <= 0.8 {
        Verdict::Good
    } else if ratio <= 0.9 {
        Verdict::Tight
    } else {
        Verdict::WontFit
    };
    let mut fitting: Vec<&Tier> = TIERS.iter().filter(|t| fits(input, t.id)).collect();
    fitting.sort_by_key(|t| t.speed_rank);
    let recommended = fitting.first().map(|t| t.id.to_string());
    fitting.sort_by_key(|t| t.accuracy_rank);
    let best = fitting.first().map(|t| t.id.to_string());
    let tier_bytes = TIERS.iter().map(|t| (t.id.to_string(), footprint(input, t.id))).collect();
    MemoryReport {
        total: input.total,
        used_by_others: input.used_by_others,
        selected: input.selected.clone(),
        selected_bytes,
        llm_bytes: input.llm_bytes,
        projected,
        percent,
        verdict,
        recommended,
        best,
        free_after: input.total.saturating_sub(projected),
        tier_bytes,
        source_label: None,
        adapter: None,
    }
}

/// (total, used) system memory in bytes. On Apple Silicon this is the unified memory the GPU shares.
pub fn system_memory() -> (u64, u64) {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let total = sys.total_memory();
    #[cfg(target_os = "macos")]
    {
        if let Some(used) = macos_memory_used() {
            return (total, used);
        }
    }
    (total, sys.used_memory())
}

/// Activity Monitor's "Memory Used": app memory (anonymous pages minus purgeable) + wired + compressed.
/// sysinfo's `used_memory` also counts active file-cache pages, which churn with every build or download and
/// hide what the models actually take.
#[cfg(target_os = "macos")]
#[allow(deprecated)] // libc::mach_host_self is marked deprecated in favour of the mach2 crate; one call is not worth a dependency.
fn macos_memory_used() -> Option<u64> {
    use std::mem::MaybeUninit;
    let mut stats = MaybeUninit::<libc::vm_statistics64>::uninit();
    let mut count = (std::mem::size_of::<libc::vm_statistics64>() / std::mem::size_of::<libc::integer_t>()) as libc::mach_msg_type_number_t;
    // SAFETY: host_statistics64 fills `stats` with exactly `count` integers for HOST_VM_INFO64; we check the return code.
    let (stats, page_size) = unsafe {
        let kr = libc::host_statistics64(libc::mach_host_self(), libc::HOST_VM_INFO64, stats.as_mut_ptr() as *mut libc::integer_t, &mut count);
        if kr != libc::KERN_SUCCESS {
            return None;
        }
        (stats.assume_init(), libc::sysconf(libc::_SC_PAGESIZE) as u64)
    };
    let app = (stats.internal_page_count as u64).saturating_sub(stats.purgeable_count as u64);
    let used = (app + stats.wire_count as u64 + stats.compressor_page_count as u64) * page_size;
    Some(used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const GB: u64 = 1_000_000_000;

    fn input(total_gb: f64, others_gb: f64, llm_gb: f64, selected: &str) -> MemoryInput {
        MemoryInput {
            total: (total_gb * GB as f64) as u64,
            used_by_others: (others_gb * GB as f64) as u64,
            footprints: HashMap::new(),
            llm_bytes: (llm_gb * GB as f64) as u64,
            selected: selected.into(),
        }
    }

    #[test]
    fn recommended_is_turbo_when_it_fits() {
        let r = budget(&input(16.0, 1.6, 5.0, "large-v3-turbo"));
        assert_eq!(r.recommended.as_deref(), Some("large-v3-turbo"));
        assert!(matches!(r.verdict, Verdict::Good));
        assert_eq!(r.selected_bytes, estimate_footprint("large-v3-turbo"));
        assert_eq!(r.projected, r.used_by_others + r.selected_bytes + r.llm_bytes);
        assert_eq!(r.free_after, r.total - r.projected);
    }

    #[test]
    fn best_is_large_v3_under_80_percent() {
        let r = budget(&input(16.0, 1.6, 5.0, "large-v3-turbo"));
        assert_eq!(r.best.as_deref(), Some("large-v3"));
    }

    #[test]
    fn tight_between_80_and_90() {
        // 8 GB: 3.2 used + 2.3 llm + 1.0 (large-v3 estimate) = 6.5 → 81 %
        let r = budget(&input(8.0, 3.2, 2.3, "large-v3"));
        assert!(matches!(r.verdict, Verdict::Tight), "{:?} at {} %", r.verdict, r.percent);
        assert_eq!(r.percent, 81);
    }

    #[test]
    fn wont_fit_above_90_and_steps_down() {
        // 7.8 GB: 3.2 used + 2.3 llm + 1.6 (distil) = 7.1 → 91 % won't fit; turbo 6.05 → 78 % fits; large-v3 6.5 → 83 % does not
        let r = budget(&input(7.8, 3.2, 2.3, "distil-large-v3"));
        assert!(matches!(r.verdict, Verdict::WontFit), "{:?} at {} %", r.verdict, r.percent);
        assert_eq!(r.recommended.as_deref(), Some("large-v3-turbo"), "turbo (0.55) still fits under 80 %");
        assert_eq!(r.best.as_deref(), Some("large-v3-turbo"), "large-v3 (1.0 GB) would exceed 80 %");
    }

    #[test]
    fn nothing_fits_when_memory_is_exhausted() {
        let r = budget(&input(8.0, 6.5, 1.0, "large-v3-turbo"));
        assert_eq!(r.recommended, None);
        assert_eq!(r.best, None);
        assert!(matches!(r.verdict, Verdict::WontFit));
    }

    #[test]
    fn measured_footprint_overrides_estimate() {
        let mut i = input(16.0, 1.6, 0.0, "large-v3-turbo");
        i.footprints.insert("large-v3-turbo".into(), 703_000_000);
        let r = budget(&i);
        assert_eq!(r.selected_bytes, 703_000_000);
    }

    #[test]
    fn system_memory_reports_sane_numbers() {
        let (total, used) = system_memory();
        assert!(total > 1 * GB);
        assert!(used > 0 && used <= total);
    }

    #[test]
    fn used_memory_is_plausible() {
        let (total, used) = system_memory();
        assert!(total > 1_000_000_000);
        assert!(used > 100_000_000 && used < total, "used={used} total={total}");
        let (_, again) = system_memory();
        assert!(again > 0, "every call samples afresh");
    }

    /// `cargo test -p privatevox-core prints_live_system_memory -- --ignored --nocapture`: eyeball the sampled numbers.
    #[test]
    #[ignore]
    fn prints_live_system_memory() {
        let (total, used) = system_memory();
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        println!("SYSMEM total={} used={} free={} available={}", total, used, sys.free_memory(), sys.available_memory());
    }

    #[test]
    fn budget_from_host_maps_status() {
        use crate::net::{GpuInfo, PolishInfo, StatusReply, WhisperInfo};
        let st = StatusReply {
            name: "PC".into(),
            version: "0.3.0".into(),
            platform: "windows".into(),
            gpu: GpuInfo { name: "RX 9070".into(), total_bytes: 16_000_000_000, used_bytes: 5_000_000_000 },
            whisper: WhisperInfo { model: "large-v3-turbo".into(), loaded: true, resident_bytes: 1_300_000_000 },
            polish: PolishInfo { enabled: true, endpoint_label: Some("LM Studio".into()), model: Some("qwen".into()), resident_bytes: 2_000_000_000 },
            clients: 1,
        };
        let r = budget_from_host(&st, "PC");
        assert_eq!(r.total, 16_000_000_000);
        assert_eq!(r.used_by_others, 1_700_000_000);
        assert_eq!(r.selected, "large-v3-turbo");
        assert_eq!(r.selected_bytes, 1_300_000_000);
        assert_eq!(r.llm_bytes, 2_000_000_000);
        assert_eq!(r.projected, 5_000_000_000);
        assert_eq!(r.percent, 31);
        assert_eq!(r.verdict, Verdict::Good);
        assert_eq!(r.source_label.as_deref(), Some("PC"));
        assert_eq!(r.adapter.as_deref(), Some("RX 9070"));
        assert!(budget(&MemoryInput { total: 8, used_by_others: 1, footprints: Default::default(), llm_bytes: 0, selected: "large-v3-turbo".into() }).source_label.is_none());
    }
}
