//! Tests for effective PATH computation and agent detection.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agent_detect::detect::{detect_agent, detect_all, effective_path};
use agent_detect::host::{CmdOutput, FakeHost, SpawnError};
use agent_detect::registry::AgentDef;
use agent_detect::status::AgentState;

fn ok_cmd(stdout: &str) -> Result<CmdOutput, SpawnError> {
    Ok(CmdOutput {
        status: Some(0),
        stdout: stdout.to_string(),
        stderr: String::new(),
    })
}

fn status_cmd(status: i32) -> Result<CmdOutput, SpawnError> {
    Ok(CmdOutput {
        status: Some(status),
        stdout: String::new(),
        stderr: String::new(),
    })
}

/// Create an executable (mode 0o755) stub file `dir/name`; returns its path.
fn make_exec(dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Claude-like definition: `CLAUDE_BIN` override, `~/.claude` config,
/// `openclaude` fallback, version + auth probe commands.
fn claude_def() -> AgentDef {
    AgentDef {
        id: "claude".to_string(),
        name: "Claude Code".to_string(),
        bin: "claude".to_string(),
        fallback_bins: vec!["openclaude".to_string()],
        bin_env: Some("CLAUDE_BIN".to_string()),
        config_dirs: vec!["$CLAUDE_CONFIG_DIR || ~/.claude".to_string()],
        version_args: vec!["--version".to_string()],
        version_regex: None,
        auth_probe: Some(vec!["auth".to_string(), "status".to_string()]),
        transport: "claude-stream-json".to_string(),
        models: None,
        fallback_models: vec!["default".to_string()],
        sessions: None,
        terminal: None,
        trust: None,
    }
}

/// OpenCode-like definition: no auth probe, config under `~/.config/opencode`.
fn opencode_def() -> AgentDef {
    AgentDef {
        id: "opencode".to_string(),
        name: "OpenCode".to_string(),
        bin: "opencode".to_string(),
        fallback_bins: vec!["opencode-cli".to_string()],
        bin_env: Some("OPENCODE_BIN".to_string()),
        config_dirs: vec!["$XDG_CONFIG_HOME/opencode || ~/.config/opencode".to_string()],
        version_args: vec!["--version".to_string()],
        version_regex: None,
        auth_probe: None,
        transport: "acp".to_string(),
        models: None,
        fallback_models: vec!["default".to_string()],
        sessions: None,
        terminal: None,
        trust: None,
    }
}

fn empty_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn claude_config(host: &FakeHost) -> PathBuf {
    let dir = host.home_path().join(".claude");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn absent_when_nothing() {
    let host = FakeHost::new();
    let scratch = empty_dir();
    let path = vec![scratch.path().to_path_buf()];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(status.agent_id, "claude");
    assert_eq!(status.state, AgentState::Absent);
    assert!(status.diagnostics.is_empty());
    assert_eq!(status.searched_dirs, path);
}

#[test]
fn config_only_when_config_dir_without_binary() {
    let host = FakeHost::new();
    let config = claude_config(&host);
    let scratch = empty_dir();
    let path = vec![scratch.path().to_path_buf()];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(status.state, AgentState::ConfigOnly { config_dir: config });
}

#[test]
fn skips_broken_shim_and_uses_next_candidate() {
    let mut host = FakeHost::new();
    let base = empty_dir();
    let dir_a = base.path().join("a");
    let dir_b = base.path().join("b");
    let broken = make_exec(&dir_a, "claude");
    let working = make_exec(&dir_b, "claude");
    host.script(
        broken.clone(),
        &["--version"],
        Err(SpawnError::PermissionDenied),
    );
    host.script(working.clone(), &["--version"], ok_cmd("claude 2.1.0"));
    claude_config(&host);
    host.script(working.clone(), &["auth", "status"], ok_cmd(""));
    let path = vec![dir_a, dir_b];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(
        status.state,
        AgentState::Ready {
            bin: working,
            version: "2.1.0".to_string(),
        }
    );
    assert_eq!(status.diagnostics.len(), 1);
    assert_eq!(
        status.diagnostics[0],
        format!("{}: not invocable (permission denied)", broken.display())
    );
}

#[test]
fn bin_env_override_wins() {
    let mut host = FakeHost::new();
    let base = empty_dir();
    let dir_a = base.path().join("a");
    make_exec(&dir_a, "claude"); // must be ignored despite existing
    let custom = make_exec(&base.path().join("custom"), "claude-bin");
    host.set_var("CLAUDE_BIN", custom.to_str().unwrap());
    host.script(custom.clone(), &["--version"], ok_cmd("claude 1.5.0"));
    host.script(custom.clone(), &["auth", "status"], ok_cmd(""));
    let path = vec![dir_a];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(
        status.state,
        AgentState::Ready {
            bin: custom.clone(),
            version: "1.5.0".to_string(),
        }
    );
    // Only the override binary was ever invoked.
    assert!(host.calls().iter().all(|(program, _)| program == &custom));
}

#[test]
fn fallback_bin_used() {
    let mut host = FakeHost::new();
    let base = empty_dir();
    let dir_a = base.path().join("a");
    let dir_b = base.path().join("b");
    std::fs::create_dir_all(&dir_a).unwrap();
    let fallback = make_exec(&dir_b, "openclaude");
    host.script(fallback.clone(), &["--version"], ok_cmd("openclaude 0.9.1"));
    host.script(fallback.clone(), &["auth", "status"], ok_cmd(""));
    let path = vec![dir_a, dir_b];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(
        status.state,
        AgentState::Ready {
            bin: fallback,
            version: "0.9.1".to_string(),
        }
    );
    assert!(status.diagnostics.is_empty());
}

#[test]
fn ready_when_auth_probe_succeeds() {
    let mut host = FakeHost::new(); // no config dir
    let base = empty_dir();
    let bin = make_exec(&base.path().join("a"), "claude");
    host.script(bin.clone(), &["--version"], ok_cmd("claude 2.1.0"));
    host.script(bin.clone(), &["auth", "status"], ok_cmd(""));
    let path = vec![base.path().join("a")];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(
        status.state,
        AgentState::Ready {
            bin,
            version: "2.1.0".to_string(),
        }
    );
    assert!(status.diagnostics.is_empty());
}

#[test]
fn runnable_when_auth_probe_fails() {
    let mut host = FakeHost::new();
    let base = empty_dir();
    let bin = make_exec(&base.path().join("a"), "claude");
    host.script(bin.clone(), &["--version"], ok_cmd("claude 2.1.0"));
    host.script(bin.clone(), &["auth", "status"], status_cmd(1));
    let path = vec![base.path().join("a")];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(
        status.state,
        AgentState::Runnable {
            bin,
            version: "2.1.0".to_string(),
        }
    );
    assert_eq!(status.diagnostics, vec!["auth probe failed".to_string()]);
}

#[test]
fn ready_without_auth_probe_when_config_exists() {
    let mut host = FakeHost::new();
    let config = host.home_path().join(".config/opencode");
    std::fs::create_dir_all(&config).unwrap();
    let base = empty_dir();
    let bin = make_exec(&base.path().join("a"), "opencode");
    host.script(bin.clone(), &["--version"], ok_cmd("opencode 0.4.2"));
    let path = vec![base.path().join("a")];
    let status = detect_agent(&opencode_def(), &host, &path);
    assert_eq!(
        status.state,
        AgentState::Ready {
            bin,
            version: "0.4.2".to_string(),
        }
    );
    assert!(status.diagnostics.is_empty());
}

#[test]
fn nonzero_version_exit_is_binary_only() {
    let mut host = FakeHost::new(); // no config dir
    let base = empty_dir();
    let dir_a = base.path().join("a");
    let dir_b = base.path().join("b");
    let broken = make_exec(&dir_a, "claude");
    // A working second candidate proves detection stops after the failure.
    let working = make_exec(&dir_b, "claude");
    host.script(broken.clone(), &["--version"], status_cmd(1));
    host.script(working.clone(), &["--version"], ok_cmd("claude 2.1.0"));
    let path = vec![dir_a, dir_b];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(status.state, AgentState::BinaryOnly { bin: broken });
    assert_eq!(status.diagnostics.len(), 1);
    assert!(status.diagnostics[0].contains("exited with status 1"));
}

#[test]
fn version_regex_respected() {
    let mut host = FakeHost::new(); // no config dir
    let mut def = claude_def();
    def.auth_probe = None;
    def.version_regex = Some(r"build v(\d+\.\d+\.\d+)".to_string());
    let base = empty_dir();
    let bin = make_exec(&base.path().join("a"), "claude");
    // Default pattern would match "2.0"; whole regex match would be
    // "build v1.2.3"; only the capture group yields "1.2.3".
    host.script(
        bin.clone(),
        &["--version"],
        ok_cmd("2.0 server (build v1.2.3)"),
    );
    let path = vec![base.path().join("a")];
    let status = detect_agent(&def, &host, &path);
    assert_eq!(
        status.state,
        AgentState::Runnable {
            bin,
            version: "1.2.3".to_string(),
        }
    );
}

#[test]
fn effective_path_dedupes_and_orders() {
    let mut host = FakeHost::new();
    host.set_var("PATH", "/a:/b");
    host.set_login_path(Some(vec![PathBuf::from("/b"), PathBuf::from("/c")]));
    let home = host.home_path();
    let expected = vec![
        PathBuf::from("/a"),
        PathBuf::from("/b"),
        PathBuf::from("/c"),
        home.join(".local/bin"),
        home.join(".cargo/bin"),
        home.join(".bun/bin"),
        home.join(".npm-global/bin"),
        home.join(".volta/bin"),
        home.join(".local/share/mise/shims"),
        home.join(".local/share/fnm/aliases/default/bin"),
        home.join(".opencode/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ];
    assert_eq!(effective_path(&host), expected);
}

#[test]
fn effective_path_survives_login_shell_failure() {
    let mut host = FakeHost::new(); // login_shell_path -> None
    host.set_var("PATH", "/x");
    let home = host.home_path();
    let expected = vec![
        PathBuf::from("/x"),
        home.join(".local/bin"),
        home.join(".cargo/bin"),
        home.join(".bun/bin"),
        home.join(".npm-global/bin"),
        home.join(".volta/bin"),
        home.join(".local/share/mise/shims"),
        home.join(".local/share/fnm/aliases/default/bin"),
        home.join(".opencode/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ];
    assert_eq!(effective_path(&host), expected);
}

#[test]
fn nvm_dirs_sorted_descending() {
    let host = FakeHost::new();
    let node = host.home_path().join(".nvm/versions/node");
    for version in ["v18.0.0", "v20.11.0", "v22.1.0"] {
        std::fs::create_dir_all(node.join(version).join("bin")).unwrap();
    }
    // A version directory without a `bin` subdir is not a PATH candidate.
    std::fs::create_dir_all(node.join("v25.0.0")).unwrap();
    let home = host.home_path();
    let expected = vec![
        home.join(".local/bin"),
        home.join(".cargo/bin"),
        home.join(".bun/bin"),
        home.join(".npm-global/bin"),
        home.join(".volta/bin"),
        home.join(".local/share/mise/shims"),
        home.join(".local/share/fnm/aliases/default/bin"),
        node.join("v22.1.0/bin"),
        node.join("v20.11.0/bin"),
        node.join("v18.0.0/bin"),
        home.join(".opencode/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ];
    assert_eq!(effective_path(&host), expected);
}

// Extra tests pinning algorithm branches not covered above.

#[test]
fn effective_path_skips_empty_path_components() {
    let mut host = FakeHost::new();
    host.set_var("PATH", "/a::/b:");
    let result = effective_path(&host);
    assert_eq!(&result[..2], &[PathBuf::from("/a"), PathBuf::from("/b")]);
    assert!(!result.contains(&PathBuf::new()));
}

#[test]
fn timeout_stops_candidate_search() {
    let mut host = FakeHost::new(); // no config dir
    let base = empty_dir();
    let dir_a = base.path().join("a");
    let dir_b = base.path().join("b");
    let hung = make_exec(&dir_a, "claude");
    // A working second candidate proves detection stops after the timeout.
    let working = make_exec(&dir_b, "claude");
    host.script(hung.clone(), &["--version"], Err(SpawnError::Timeout));
    host.script(working, &["--version"], ok_cmd("claude 2.1.0"));
    let path = vec![dir_a, dir_b];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(status.state, AgentState::BinaryOnly { bin: hung });
    assert_eq!(status.diagnostics.len(), 1);
    assert!(status.diagnostics[0].contains("timed out"));
}

#[test]
fn unparseable_version_is_binary_only() {
    let mut host = FakeHost::new(); // no config dir
    let base = empty_dir();
    let dir_a = base.path().join("a");
    let dir_b = base.path().join("b");
    let bin = make_exec(&dir_a, "claude");
    let working = make_exec(&dir_b, "claude");
    host.script(
        bin.clone(),
        &["--version"],
        ok_cmd("no version number here"),
    );
    host.script(working, &["--version"], ok_cmd("claude 2.1.0"));
    let path = vec![dir_a, dir_b];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(status.state, AgentState::BinaryOnly { bin });
    assert_eq!(status.diagnostics, vec!["unparseable version".to_string()]);
}

#[test]
fn unversioned_binary_with_config_is_config_only() {
    let mut host = FakeHost::new();
    let config = claude_config(&host);
    let base = empty_dir();
    let bin = make_exec(&base.path().join("a"), "claude");
    host.script(bin, &["--version"], status_cmd(2));
    let path = vec![base.path().join("a")];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(status.state, AgentState::ConfigOnly { config_dir: config });
}

#[test]
fn non_executable_file_is_not_a_candidate() {
    let host = FakeHost::new();
    let base = empty_dir();
    let dir_a = base.path().join("a");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::write(dir_a.join("claude"), b"not executable").unwrap();
    std::fs::set_permissions(dir_a.join("claude"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let path = vec![dir_a.clone()];
    let status = detect_agent(&claude_def(), &host, &path);
    assert_eq!(status.state, AgentState::Absent);
    assert!(status.diagnostics.is_empty());
    assert!(host.calls().is_empty());
}

#[test]
fn detect_all_returns_registry_order() {
    let mut host = FakeHost::new();
    let local_bin = host.home_path().join(".local/bin");
    let claude_bin = make_exec(&local_bin, "claude");
    host.script(claude_bin.clone(), &["--version"], ok_cmd("claude 2.1.0"));
    host.script(claude_bin.clone(), &["auth", "status"], ok_cmd(""));

    let statuses = detect_all(&[claude_def(), opencode_def()], &host);
    assert_eq!(statuses.len(), 2);
    assert_eq!(statuses[0].agent_id, "claude");
    assert_eq!(
        statuses[0].state,
        AgentState::Ready {
            bin: claude_bin,
            version: "2.1.0".to_string(),
        }
    );
    assert_eq!(statuses[1].agent_id, "opencode");
    assert_eq!(statuses[1].state, AgentState::Absent);
    let expected_path = effective_path(&host);
    assert_eq!(statuses[0].searched_dirs, expected_path);
    assert_eq!(statuses[1].searched_dirs, expected_path);
}
