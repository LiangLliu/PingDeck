#![forbid(unsafe_code)]

//! Repository-level automation that stays independent of any frontend runtime (Node/React/WebView).
//!
//! Run via `cargo xtask <command>` (configured in `.cargo/config.toml`).

mod arch;
mod bundle;
mod docs;
mod docs_lint;
mod i18n;
mod markdown;
mod models_snapshot;
mod run_limits;
mod schema;
mod tokens;
mod version;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Repository root resolved from `xtask`'s compile-time manifest directory.
pub fn repo_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.to_path_buf())
}

/// Writes an error line to stderr without triggering `clippy::print_stderr`.
pub fn err_line(msg: &str) {
    let _ = writeln!(io::stderr(), "{msg}");
}

/// Writes a line to stdout.
pub fn out_line(msg: &str) {
    let _ = writeln!(io::stdout(), "{msg}");
}

fn usage() -> &'static str {
    "usage: cargo xtask <command> [args...]\n\
     commands:\n  \
       check-docs [--list]                   verify doc links, anchors, section refs, paths, and PRD run limits\n  \
       docs-lint [--all] [--summary] [path]  verify Chinese technical writing style rules\n  \
       docs                                  run both check-docs and docs-lint\n  \
       check-arch                            verify 4-layer Core / Desktop Host / GPUI UI isolation rules\n  \
       check-i18n                            verify locales/ bilingual parity, placeholders, CJK spacing, copy rules\n  \
       check-tokens                          verify tokens/tokens.json OKLCH gamut and WCAG contrast invariants\n  \
       check-version [--tag vX.Y.Z]          verify Cargo.toml and optional UI manifest versions\n  \
       bundle [--binary <p>] [--out <dir>]   package native macOS .app / Linux release bundle\n  \
       schema-fingerprint [storage-dir]      print the pingdeck-store SQLite schema user_version\n  \
       free-port                             print an available 127.0.0.1 TCP port\n  \
       models-snapshot <in> [out] [--date]   trim models.dev api.json into fixtures/models-dev-snapshot.json"
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        err_line(usage());
        return ExitCode::from(2);
    };
    let root = repo_root();
    match cmd.as_str() {
        "check-docs" => docs::run(&root, rest),
        "docs-lint" => docs_lint::run(&root, rest),
        "docs" => {
            if !rest.is_empty() {
                err_line("usage: cargo xtask docs");
                return ExitCode::from(2);
            }
            let c1 = docs::run(&root, &[]);
            if c1 != ExitCode::SUCCESS {
                return c1;
            }
            docs_lint::run(&root, &[])
        }
        "check-arch" => arch::run(&root, rest),
        "check-i18n" => i18n::run(&root, rest),
        "check-tokens" => tokens::run(&root, rest),
        "check-version" => version::run(&root, rest),
        "bundle" => bundle::run(&root, rest),
        "schema-fingerprint" => schema::run_fingerprint(&root, rest),
        "free-port" => schema::run_free_port(rest),
        "models-snapshot" => models_snapshot::run(&root, rest),
        "-h" | "--help" | "help" => {
            out_line(usage());
            ExitCode::SUCCESS
        }
        other => {
            err_line(&format!("unknown xtask command '{other}'\n{}", usage()));
            ExitCode::from(2)
        }
    }
}
