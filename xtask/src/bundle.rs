//! Pure-Rust native desktop packager (`cargo xtask bundle`).
//!
//! Produces distributable OS bundles directly from the compiled `pingdeck` GPUI release binary
//! without Node.js or Tauri CLI:
//! - macOS: `<out>/PingDeck.app` (Info.plist + icon.icns + `Contents/MacOS/pingdeck`)
//! - Linux: `<out>/pingdeck-linux/` (`bin/pingdeck`, `.desktop` entry, icons)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::version::parse_cargo_workspace_version;
use crate::{err_line, out_line};

const USAGE: &str = "usage: cargo xtask bundle [--binary <path>] [--out <dir>] [--target <triple>]";
const BUNDLE_ID: &str = "com.pingdeck.desktop";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleConfig {
    pub binary: PathBuf,
    pub out_dir: PathBuf,
    pub version: String,
    pub target: Option<String>,
}

/// Generates macOS `Contents/Info.plist` for `PingDeck.app`.
#[must_use]
pub fn render_macos_info_plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>PingDeck</string>
  <key>CFBundleExecutable</key>
  <string>pingdeck</string>
  <key>CFBundleIconFile</key>
  <string>icon.icns</string>
  <key>CFBundleIdentifier</key>
  <string>{BUNDLE_ID}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>PingDeck</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>{version}</string>
  <key>CFBundleVersion</key>
  <string>{version}</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key>
  <true/>
</dict>
</plist>
"#
    )
}

/// Generates Linux freedesktop `.desktop` entry.
#[must_use]
pub fn render_linux_desktop_entry(version: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=PingDeck\n\
         Comment=Local-first API Workbench, AI Gateway & Coding Agent\n\
         Exec=pingdeck\n\
         Icon=pingdeck\n\
         Terminal=false\n\
         Categories=Development;Network;\n\
         StartupWMClass=pingdeck\n\
         X-AppImage-Version={version}\n"
    )
}

fn package_macos(root: &Path, cfg: &BundleConfig) -> Result<PathBuf, String> {
    let app_dir = cfg.out_dir.join("PingDeck.app");
    let macos_dir = app_dir.join("Contents/MacOS");
    let res_dir = app_dir.join("Contents/Resources");
    if app_dir.exists() {
        fs::remove_dir_all(&app_dir)
            .map_err(|e| format!("failed to clear {}: {e}", app_dir.display()))?;
    }
    fs::create_dir_all(&macos_dir)
        .map_err(|e| format!("failed to create {}: {e}", macos_dir.display()))?;
    fs::create_dir_all(&res_dir)
        .map_err(|e| format!("failed to create {}: {e}", res_dir.display()))?;

    let dst_bin = macos_dir.join("pingdeck");
    fs::copy(&cfg.binary, &dst_bin).map_err(|e| {
        format!(
            "failed to copy {} -> {}: {e}",
            cfg.binary.display(),
            dst_bin.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dst_bin, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod failed on {}: {e}", dst_bin.display()))?;
    }

    let icns = root.join("assets/icons/icon.icns");
    if icns.exists() {
        fs::copy(&icns, res_dir.join("icon.icns"))
            .map_err(|e| format!("failed to copy {}: {e}", icns.display()))?;
    }

    fs::write(
        app_dir.join("Contents/Info.plist"),
        render_macos_info_plist(&cfg.version),
    )
    .map_err(|e| format!("failed to write Info.plist: {e}"))?;

    Ok(app_dir)
}

fn package_linux(root: &Path, cfg: &BundleConfig) -> Result<PathBuf, String> {
    let stage = cfg.out_dir.join("pingdeck-linux");
    let bin_dir = stage.join("bin");
    let app_dir = stage.join("share/applications");
    let icon_dir = stage.join("share/icons/hicolor/128x128/apps");
    if stage.exists() {
        fs::remove_dir_all(&stage)
            .map_err(|e| format!("failed to clear {}: {e}", stage.display()))?;
    }
    for dir in [&bin_dir, &app_dir, &icon_dir] {
        fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    }
    let dst_bin = bin_dir.join("pingdeck");
    fs::copy(&cfg.binary, &dst_bin).map_err(|e| {
        format!(
            "failed to copy {} -> {}: {e}",
            cfg.binary.display(),
            dst_bin.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dst_bin, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod failed on {}: {e}", dst_bin.display()))?;
    }
    let icon = root.join("assets/icons/128x128.png");
    if icon.exists() {
        fs::copy(&icon, icon_dir.join("pingdeck.png"))
            .map_err(|e| format!("failed to copy {}: {e}", icon.display()))?;
    }
    fs::write(
        app_dir.join("com.pingdeck.desktop.desktop"),
        render_linux_desktop_entry(&cfg.version),
    )
    .map_err(|e| format!("failed to write .desktop entry: {e}"))?;
    Ok(stage)
}

pub fn run(root: &Path, args: &[String]) -> ExitCode {
    let mut binary: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut target: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--binary" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    err_line(&format!("missing value for --binary\n{USAGE}"));
                    return ExitCode::from(2);
                };
                binary = Some(PathBuf::from(val));
            }
            "--out" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    err_line(&format!("missing value for --out\n{USAGE}"));
                    return ExitCode::from(2);
                };
                out_dir = Some(PathBuf::from(val));
            }
            "--target" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    err_line(&format!("missing value for --target\n{USAGE}"));
                    return ExitCode::from(2);
                };
                target = Some(val.clone());
            }
            other => {
                err_line(&format!("unexpected argument '{other}'\n{USAGE}"));
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    let cargo_toml = match fs::read_to_string(root.join("Cargo.toml")) {
        Ok(t) => t,
        Err(e) => {
            err_line(&format!("FAIL: cannot read Cargo.toml: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let version = parse_cargo_workspace_version(&cargo_toml).unwrap_or_else(|| "0.1.0".to_owned());

    let release_dir = match &target {
        Some(triple) => root.join("target").join(triple).join("release"),
        None => root.join("target/release"),
    };
    let binary = binary.unwrap_or_else(|| {
        let primary = release_dir.join("pingdeck");
        if primary.exists() {
            primary
        } else {
            release_dir.join("pingdeck-gpui")
        }
    });
    if !binary.exists() {
        err_line(&format!(
            "FAIL: release binary not found at {} (run `cargo build --release -p pingdeck-gpui --locked` first)",
            binary.display()
        ));
        return ExitCode::FAILURE;
    }
    let out_dir = out_dir.unwrap_or_else(|| release_dir.join("bundle"));

    let cfg = BundleConfig {
        binary,
        out_dir,
        version,
        target,
    };

    let is_macos = cfg
        .target
        .as_deref()
        .map_or(cfg!(target_os = "macos"), |t| t.contains("apple-darwin"));

    let result = if is_macos {
        package_macos(root, &cfg)
    } else {
        package_linux(root, &cfg)
    };
    match result {
        Ok(path) => {
            out_line(&format!("bundle ok: {}", path.display()));
            ExitCode::SUCCESS
        }
        Err(err) => {
            err_line(&format!("FAIL: {err}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_valid_macos_plist_and_linux_desktop() {
        let plist = render_macos_info_plist("1.2.3");
        assert!(plist.contains("<string>com.pingdeck.desktop</string>"));
        assert!(plist.contains("<string>1.2.3</string>"));
        assert!(plist.contains("<string>pingdeck</string>"));

        let desktop = render_linux_desktop_entry("1.2.3");
        assert!(desktop.contains("Exec=pingdeck"));
        assert!(desktop.contains("X-AppImage-Version=1.2.3"));
    }
}
