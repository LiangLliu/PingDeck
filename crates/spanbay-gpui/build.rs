//! Emits compile-time build metadata (`SPANBAY_BUILD_DATE`, `SPANBAY_GIT_SHA`) for the About
//! page without adding any build-time crate dependencies.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    // Watch git HEAD when building from a checkout so commit SHA stays accurate without
    // triggering rebuilds on unrelated working-tree edits.
    let git_head = Path::new("../../.git/HEAD");
    if git_head.exists() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
        if let Ok(head_text) = std::fs::read_to_string(git_head)
            && let Some(rel_ref) = head_text.trim().strip_prefix("ref: ")
        {
            let ref_path = Path::new("../../.git").join(rel_ref);
            if ref_path.exists() {
                println!("cargo:rerun-if-changed=../../.git/{rel_ref}");
            }
        }
    }

    let epoch_secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_secs()).ok())
        })
        .unwrap_or(0);

    let build_date = format_utc_ymd(epoch_secs);
    println!("cargo:rustc-env=SPANBAY_BUILD_DATE={build_date}");

    let git_sha = read_git_short_sha().unwrap_or_default();
    println!("cargo:rustc-env=SPANBAY_GIT_SHA={git_sha}");
}

/// Converts Unix timestamp (seconds) into UTC `YYYY-MM-DD` via Howard Hinnant's civil-from-days
/// algorithm.
fn format_utc_ymd(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}

fn read_git_short_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    if sha.is_empty() { None } else { Some(sha) }
}
