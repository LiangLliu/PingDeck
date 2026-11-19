//! Verifies that `Cargo.toml` (`[workspace.package].version`) and any present UI manifests
//! (`package.json`, `src-tauri/tauri.conf.json`) declare the same SemVer version, and optionally
//! match a release tag (`--tag vX.Y.Z`).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use regex::Regex;
use toml_edit::DocumentMut;

use crate::{err_line, out_line};

const USAGE: &str = "usage: cargo xtask check-version [--tag vX.Y.Z]";

pub fn parse_cargo_workspace_version(toml_text: &str) -> Option<String> {
    let doc: DocumentMut = toml_text.parse().ok()?;
    doc.get("workspace")?
        .get("package")?
        .get("version")?
        .as_str()
        .map(str::to_owned)
}

pub fn parse_json_version(json_text: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(json_text).ok()?;
    val.get("version")?.as_str().map(str::to_owned)
}

pub fn verify_versions(
    manifests: &[(&str, Option<String>)],
    tag: Option<&str>,
) -> Result<String, Vec<String>> {
    let semver = Regex::new(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$")
        .map_err(|e| vec![format!("regex error: {e}")])?;

    let mut errors = Vec::new();
    for (name, ver) in manifests {
        match ver {
            Some(v) if semver.is_match(v) => {}
            other => errors.push(format!("{name}: invalid or missing version ({other:?})")),
        }
    }
    let unique: BTreeSet<&str> = manifests.iter().filter_map(|(_, v)| v.as_deref()).collect();
    if unique.len() > 1 {
        let summary = manifests
            .iter()
            .map(|(n, v)| format!("{n}={}", v.as_deref().unwrap_or("<missing>")))
            .collect::<Vec<_>>()
            .join(", ");
        errors.push(format!("version mismatch: {summary}"));
    }
    let version = manifests
        .iter()
        .find_map(|(_, v)| v.clone())
        .unwrap_or_default();
    if let Some(raw_tag) = tag {
        let clean = raw_tag.strip_prefix("refs/tags/").unwrap_or(raw_tag);
        if clean.strip_prefix('v') != Some(version.as_str()) {
            errors.push(format!(
                "tag '{clean}' does not match manifest version 'v{version}'"
            ));
        }
    }
    if errors.is_empty() {
        Ok(version)
    } else {
        Err(errors)
    }
}

pub fn run(root: &Path, args: &[String]) -> ExitCode {
    let mut tag: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tag" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    err_line(&format!("missing value for --tag\n{USAGE}"));
                    return ExitCode::from(2);
                };
                tag = Some(val.clone());
            }
            arg if arg.starts_with("--tag=") => {
                tag = Some(arg["--tag=".len()..].to_owned());
            }
            other => {
                err_line(&format!("unexpected argument '{other}'\n{USAGE}"));
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    let cargo_text = match fs::read_to_string(root.join("Cargo.toml")) {
        Ok(t) => t,
        Err(e) => {
            err_line(&format!("FAIL: cannot read Cargo.toml: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let mut manifests: Vec<(&str, Option<String>)> =
        vec![("Cargo.toml", parse_cargo_workspace_version(&cargo_text))];

    // Optional UI manifests: validated whenever present so swapping UI shells never breaks version checks.
    for rel in ["package.json", "src-tauri/tauri.conf.json"] {
        let p = root.join(rel);
        if p.exists() {
            let ver = fs::read_to_string(&p)
                .ok()
                .and_then(|s| parse_json_version(&s));
            manifests.push((rel, ver));
        }
    }

    match verify_versions(&manifests, tag.as_deref()) {
        Ok(v) => {
            if let Some(t) = tag {
                out_line(&format!("version ok: v{v} (matches {t})"));
            } else {
                out_line(&format!("version ok: v{v}"));
            }
            ExitCode::SUCCESS
        }
        Err(errs) => {
            for e in errs {
                err_line(&format!("FAIL: {e}"));
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates_versions() {
        assert_eq!(
            parse_cargo_workspace_version(
                "[workspace.package]\nedition = \"2024\"\nversion = \"1.2.3\"\n"
            ),
            Some("1.2.3".to_owned())
        );
        assert_eq!(
            parse_cargo_workspace_version("[package]\nversion = \"0.1.0\"\n"),
            None
        );
        let ok = verify_versions(
            &[
                ("Cargo.toml", Some("0.1.0".to_owned())),
                ("package.json", Some("0.1.0".to_owned())),
            ],
            Some("v0.1.0"),
        );
        assert_eq!(ok.unwrap(), "0.1.0");
        let mismatch = verify_versions(
            &[
                ("Cargo.toml", Some("0.1.0".to_owned())),
                ("package.json", Some("0.2.0".to_owned())),
            ],
            None,
        );
        assert!(mismatch.is_err());
        let bad_tag = verify_versions(&[("Cargo.toml", Some("0.1.0".to_owned()))], Some("v0.2.0"));
        assert!(bad_tag.is_err());
    }
}
