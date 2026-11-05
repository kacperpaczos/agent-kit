//! Test helpers: a temporary HOME filled from `tests/fixtures`.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_sessions::FakeEnv;

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A temp HOME with Claude and Codex fixtures at their default places.
pub fn home() -> (tempfile::TempDir, FakeEnv) {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(
        &fixtures().join("claude"),
        &dir.path().join(".claude/projects"),
    );
    copy_dir(
        &fixtures().join("codex"),
        &dir.path().join(".codex/sessions"),
    );
    let env = FakeEnv::with_home(dir.path());
    (dir, env)
}

pub fn at(rfc3339_secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(rfc3339_secs)
}
