//! History export as Markdown or JSON (design §4.4).

use crate::store::Dictation;

fn stamp(ms: i64) -> String {
    // UTC, good enough for a file heading; the panel shows local time.
    let secs = ms / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// One `##` heading per dictation with a metadata line and the text.
pub fn to_markdown(rows: &[Dictation]) -> String {
    let mut out = String::from("# PrivateVox history\n\n");
    for r in rows {
        let text = r.polished_text.as_deref().unwrap_or(&r.raw_text);
        out.push_str(&format!("## {} · {}\n", stamp(r.created_at_ms), r.app_name.as_deref().unwrap_or("unknown app")));
        out.push_str(&format!(
            "_{} · {} · {} · {:.1} s · {}{}{}_\n\n",
            r.mode,
            r.model,
            r.language.as_deref().unwrap_or("auto"),
            r.duration_ms as f32 / 1000.0,
            r.status,
            if r.polish_status == "unpolished" { " · unpolished" } else { "" },
            if r.pinned { " · pinned" } else { "" }
        ));
        out.push_str(text);
        out.push_str("\n\n");
    }
    out
}

pub fn to_json(rows: &[Dictation]) -> String {
    serde_json::to_string_pretty(rows).unwrap_or_else(|_| "[]".into())
}

/// `privatevox-history-YYYYMMDD-HHMM.<ext>` for the given UTC time.
pub fn export_filename(ext: &str, now_ms: i64) -> String {
    let secs = now_ms / 1000;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!("privatevox-history-{y:04}{m:02}{d:02}-{:02}{:02}.{ext}", rem / 3600, (rem % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Dictation;

    fn row(id: i64, text: &str) -> Dictation {
        Dictation {
            id,
            created_at_ms: 1_761_600_000_000 + id * 60_000,
            app_name: Some("TextEdit".into()),
            mode: "raw".into(),
            model: "large-v3-turbo".into(),
            language: Some("en".into()),
            duration_ms: 1500,
            raw_text: text.into(),
            polished_text: None,
            polished_by: None,
            polish_status: "none".into(),
            status: "ok".into(),
            timing_json: None,
            pinned: id == 2,
        }
    }

    #[test]
    fn markdown_has_one_heading_per_row() {
        let md = to_markdown(&[row(1, "first line"), row(2, "second\nwith break")]);
        assert_eq!(md.matches("\n## ").count() + usize::from(md.starts_with("## ")), 2);
        assert!(md.contains("first line"));
        assert!(md.contains("second\nwith break"));
        assert!(md.contains("TextEdit"));
        assert!(md.contains("large-v3-turbo"));
        assert!(md.contains("pinned"), "pinned rows are marked");
    }

    #[test]
    fn json_roundtrips_rows() {
        let rows = vec![row(1, "a"), row(2, "b")];
        let json = to_json(&rows);
        let back: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[1]["raw_text"], "b");
        assert_eq!(back[1]["pinned"], true);
    }

    #[test]
    fn export_filename_has_date_and_extension() {
        let n = export_filename("md", 1_761_600_000_000);
        assert!(n.starts_with("privatevox-history-2025"), "{n}");
        assert!(n.ends_with(".md"));
    }
}
