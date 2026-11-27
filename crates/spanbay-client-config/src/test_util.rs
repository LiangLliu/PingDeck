//! Temp-dir home and apply helpers for the writer tests.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use super::{
    AppError, ApplyResult, ClientApp, ClientConfigPlan, ClientConfigRequest, Clock, apply,
    backup::is_backup_of,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// 2026-11-07 10:00:00 UTC, no zone offset.
pub const CLOCK: Clock = Clock {
    now_ms: 1_794_045_600_000,
    tz_offset_minutes: 0,
};

/// A unique temp directory removed on drop.
pub struct TempHome(pub PathBuf);

impl TempHome {
    pub fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("spanbay-client-config-{}-{n}", std::process::id()));
        let home = Self(dir);
        // A leftover of an earlier run with the same pid must not leak into this test.
        let _ = fs::remove_dir_all(&home.0);
        let _ = fs::remove_dir_all(home.data());
        fs::create_dir_all(&home.0).unwrap();
        home
    }

    pub fn write(&self, rel: &str, text: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    pub fn read(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.0.join(rel)).ok()
    }

    /// The data folder that holds the backups: a sibling of the home directory.
    pub fn data(&self) -> PathBuf {
        let name = self.0.file_name().map(|n| n.to_string_lossy().into_owned());
        self.0
            .with_file_name(format!("{}-data", name.unwrap_or_default()))
    }

    /// The timestamped backups of `app`, safety backups included (the kept original is not one).
    pub fn backups(&self, app: ClientApp) -> Vec<String> {
        self.backup_folder_names(app, |n| !n.contains(".spanbay-original"))
    }

    /// The kept originals (`.spanbay-original`, `-absent`) in `app`'s backup folder, sorted.
    pub fn originals(&self, app: ClientApp) -> Vec<String> {
        self.backup_folder_names(app, |n| n.contains(".spanbay-original"))
    }

    /// Names in `app`'s backup folder that `keep` accepts, sorted (empty when there is none).
    fn backup_folder_names(&self, app: ClientApp, keep: impl Fn(&str) -> bool) -> Vec<String> {
        let dir = self.data().join("backups").join(app.as_str());
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .filter(|name| keep(name))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Names in `rel` (a directory), sorted.
    pub fn list(&self, rel: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.0.join(rel))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        let _ = fs::remove_dir_all(self.data());
    }
}

pub fn request(app: ClientApp, alias: Option<&str>) -> ClientConfigRequest {
    ClientConfigRequest {
        app,
        client_name: "Test client".to_owned(),
        gateway_url: "http://127.0.0.1:4141/".to_owned(),
        token: "pd-token".to_owned(),
        alias: alias.map(str::to_owned),
        fast_alias: None,
        facts: super::ModelFacts::default(),
    }
}

/// [`apply`] with the fixed [`CLOCK`] and the home's data folder.
pub fn apply_now(home: &TempHome, request: &ClientConfigRequest) -> Result<ApplyResult, AppError> {
    apply(&home.0, &home.data(), request, None, CLOCK)
}

/// [`CLOCK`] moved `secs_after` seconds on.
pub fn at(secs_after: i64) -> Clock {
    Clock {
        now_ms: CLOCK.now_ms + secs_after * 1000,
        tz_offset_minutes: 0,
    }
}

/// The timestamped backups of the file called `name` among `app`'s backups.
pub fn backups_of(home: &TempHome, app: ClientApp, name: &str) -> Vec<String> {
    home.backups(app)
        .into_iter()
        .filter(|n| is_backup_of(n, name))
        .collect()
}

/// [`apply`] into the home's data folder.
pub fn apply_req(
    home: &TempHome,
    req: &ClientConfigRequest,
    reviewed: Option<&ClientConfigPlan>,
    clock: Clock,
) -> Result<ApplyResult, AppError> {
    apply(&home.0, &home.data(), req, reviewed, clock)
}

/// Applies Claude Code with a different alias each time so every apply changes the file.
pub fn apply_alias(home: &TempHome, alias: &str, clock: Clock) -> ApplyResult {
    apply_req(
        home,
        &request(ClientApp::ClaudeCode, Some(alias)),
        None,
        clock,
    )
    .unwrap()
}
