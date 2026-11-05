//! Tests for workspace trust detection and recording. Every test uses a
//! temporary HOME through `FakeHost`; the real config files are never read.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agent_detect::host::FakeHost;
use agent_detect::registry::{AgentDef, builtin_agents, parse_agents};
use agent_detect::trust::{
    TrustError, TrustState, TrustWriteStage, grant_workspace_trust, grant_workspace_trust_hooked,
    trust_file, workspace_trust,
};

fn builtin(id: &str) -> &'static AgentDef {
    builtin_agents()
        .iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("no builtin agent {id}"))
}

/// A real project directory inside the fake HOME, canonicalized.
fn project(host: &FakeHost, name: &str) -> PathBuf {
    let dir = host.home_path().join("work").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn key(dir: &Path) -> &str {
    dir.to_str().unwrap()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&read(path)).unwrap()
}

// ---- Claude Code -------------------------------------------------------

#[test]
fn claude_untrusted_then_granted_then_trusted() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let file = host.home_path().join(".claude.json");
    let original = serde_json::json!({
        "numStartups": 7,
        "projects": {
            "/other": { "hasTrustDialogAccepted": true, "allowedTools": ["x"] },
            key(&dir): { "allowedTools": [], "hasTrustDialogAccepted": false, "z": 1 }
        },
        "zLast": "kept"
    });
    std::fs::write(&file, serde_json::to_string_pretty(&original).unwrap()).unwrap();
    let claude = builtin("claude");

    assert_eq!(workspace_trust(claude, &host, &dir), TrustState::Untrusted);
    grant_workspace_trust(claude, &host, &dir).unwrap();
    assert_eq!(workspace_trust(claude, &host, &dir), TrustState::Trusted);

    let mut expected = original.clone();
    expected["projects"][key(&dir)]["hasTrustDialogAccepted"] = true.into();
    let got = json(&file);
    assert_eq!(got, expected);
    // Key order is preserved (top level and inside the project object).
    let top: Vec<&String> = got.as_object().unwrap().keys().collect();
    assert_eq!(top, ["numStartups", "projects", "zLast"]);
    let inner: Vec<&String> = got["projects"][key(&dir)]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(inner, ["allowedTools", "hasTrustDialogAccepted", "z"]);
}

#[test]
fn claude_project_object_created_with_only_the_trust_key() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let file = host.home_path().join(".claude.json");
    std::fs::write(&file, r#"{"userID":"u","projects":{"/a":{"k":1}}}"#).unwrap();
    let claude = builtin("claude");

    grant_workspace_trust(claude, &host, &dir).unwrap();
    assert_eq!(
        json(&file),
        serde_json::json!({
            "userID": "u",
            "projects": { "/a": { "k": 1 }, key(&dir): { "hasTrustDialogAccepted": true } }
        })
    );
}

#[test]
fn claude_missing_file_is_untrusted_and_created_on_grant() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let claude = builtin("claude");
    let file = host.home_path().join(".claude.json");

    assert_eq!(trust_file(claude, &host), Some(file.clone()));
    assert_eq!(workspace_trust(claude, &host, &dir), TrustState::Untrusted);
    grant_workspace_trust(claude, &host, &dir).unwrap();
    assert_eq!(
        json(&file),
        serde_json::json!({ "projects": { key(&dir): { "hasTrustDialogAccepted": true } } })
    );
    assert_eq!(workspace_trust(claude, &host, &dir), TrustState::Trusted);
}

#[test]
fn claude_config_dir_env_is_honoured() {
    let mut host = FakeHost::new();
    let cfg = host.home_path().join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    host.set_var("CLAUDE_CONFIG_DIR", cfg.to_str().unwrap());
    let dir = project(&host, "p");
    let claude = builtin("claude");

    assert_eq!(trust_file(claude, &host), Some(cfg.join(".claude.json")));
    grant_workspace_trust(claude, &host, &dir).unwrap();
    assert!(cfg.join(".claude.json").exists());
    assert!(!host.home_path().join(".claude.json").exists());
    assert_eq!(workspace_trust(claude, &host, &dir), TrustState::Trusted);
}

#[test]
fn claude_invalid_file_is_unknown_and_grant_leaves_it_untouched() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let file = host.home_path().join(".claude.json");
    let broken = "{\"projects\": {oops";
    std::fs::write(&file, broken).unwrap();
    let claude = builtin("claude");

    assert_eq!(workspace_trust(claude, &host, &dir), TrustState::Unknown);
    let err = grant_workspace_trust(claude, &host, &dir).unwrap_err();
    assert!(matches!(err, TrustError::Parse(_)), "{err:?}");
    assert_eq!(read(&file), broken);
}

#[test]
fn claude_non_object_projects_is_a_parse_error() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let file = host.home_path().join(".claude.json");
    std::fs::write(&file, r#"{"projects":[1]}"#).unwrap();

    let err = grant_workspace_trust(builtin("claude"), &host, &dir).unwrap_err();
    assert!(matches!(err, TrustError::Parse(_)), "{err:?}");
    assert_eq!(read(&file), r#"{"projects":[1]}"#);
}

#[test]
fn grant_keeps_file_permissions() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let file = host.home_path().join(".claude.json");
    std::fs::write(&file, "{}").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();

    grant_workspace_trust(builtin("claude"), &host, &dir).unwrap();
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640);
    // No temp files left behind.
    let leftovers: Vec<_> = std::fs::read_dir(host.home_path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.contains(".claude.json") && n != ".claude.json")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn symlinked_dir_resolves_to_realpath_key() {
    let host = FakeHost::new();
    let real = project(&host, "real");
    let link = host.home_path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let file = host.home_path().join(".claude.json");
    let claude = builtin("claude");

    grant_workspace_trust(claude, &host, &link).unwrap();
    let got = json(&file);
    assert_eq!(got["projects"][key(&real)]["hasTrustDialogAccepted"], true);
    assert!(got["projects"].get(link.to_str().unwrap()).is_none());
    assert_eq!(workspace_trust(claude, &host, &link), TrustState::Trusted);
    assert_eq!(workspace_trust(claude, &host, &real), TrustState::Trusted);
}

#[test]
fn entry_under_the_unresolved_path_also_counts() {
    let host = FakeHost::new();
    let real = project(&host, "real");
    let link = host.home_path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let file = host.home_path().join(".claude.json");
    let body = serde_json::json!({
        "projects": { link.to_str().unwrap(): { "hasTrustDialogAccepted": true } }
    });
    std::fs::write(&file, body.to_string()).unwrap();

    assert_eq!(
        workspace_trust(builtin("claude"), &host, &link),
        TrustState::Trusted
    );
}

// ---- Codex -------------------------------------------------------------

#[test]
fn codex_untrusted_then_granted_preserves_formatting() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let codex_dir = host.home_path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let file = codex_dir.join("config.toml");
    let original = "# my settings\nmodel = \"gpt-5\"   # favourite\n\n[projects.\"/other\"]\ntrust_level = \"trusted\"\n\n[tui]\ntheme = \"dark\" # keep\n";
    std::fs::write(&file, original).unwrap();
    let codex = builtin("codex");

    assert_eq!(trust_file(codex, &host), Some(file.clone()));
    assert_eq!(workspace_trust(codex, &host, &dir), TrustState::Untrusted);
    grant_workspace_trust(codex, &host, &dir).unwrap();
    assert_eq!(workspace_trust(codex, &host, &dir), TrustState::Trusted);

    // Everything else is byte-for-byte; the new table joins its siblings.
    let expected = format!(
        "# my settings\nmodel = \"gpt-5\"   # favourite\n\n[projects.\"/other\"]\ntrust_level = \"trusted\"\n\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n\n[tui]\ntheme = \"dark\" # keep\n",
        key(&dir)
    );
    assert_eq!(read(&file), expected);
}

#[test]
fn codex_untrusted_level_is_replaced() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let codex_dir = host.home_path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let file = codex_dir.join("config.toml");
    std::fs::write(
        &file,
        format!(
            "[projects.\"{}\"]\ntrust_level = \"untrusted\"\nother = 1\n",
            key(&dir)
        ),
    )
    .unwrap();
    let codex = builtin("codex");

    assert_eq!(workspace_trust(codex, &host, &dir), TrustState::Untrusted);
    grant_workspace_trust(codex, &host, &dir).unwrap();
    assert_eq!(
        read(&file),
        format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\nother = 1\n",
            key(&dir)
        )
    );
}

#[test]
fn codex_home_env_is_honoured_and_missing_file_created() {
    let mut host = FakeHost::new();
    let codex_home = host.home_path().join("ch");
    host.set_var("CODEX_HOME", codex_home.to_str().unwrap());
    let dir = project(&host, "p");
    let codex = builtin("codex");
    let file = codex_home.join("config.toml");

    assert_eq!(trust_file(codex, &host), Some(file.clone()));
    assert_eq!(workspace_trust(codex, &host, &dir), TrustState::Untrusted);
    grant_workspace_trust(codex, &host, &dir).unwrap();
    assert_eq!(
        read(&file),
        format!("[projects.\"{}\"]\ntrust_level = \"trusted\"\n", key(&dir))
    );
    assert!(!host.home_path().join(".codex").exists());
    assert_eq!(workspace_trust(codex, &host, &dir), TrustState::Trusted);
}

#[test]
fn codex_invalid_file_is_unknown_and_grant_leaves_it_untouched() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let codex_dir = host.home_path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let file = codex_dir.join("config.toml");
    let broken = "model = \n[projects\n";
    std::fs::write(&file, broken).unwrap();
    let codex = builtin("codex");

    assert_eq!(workspace_trust(codex, &host, &dir), TrustState::Unknown);
    let err = grant_workspace_trust(codex, &host, &dir).unwrap_err();
    assert!(matches!(err, TrustError::Parse(_)), "{err:?}");
    assert_eq!(read(&file), broken);
}

// ---- Agents without a trust concept ------------------------------------

#[test]
fn agent_without_trust_table_is_unknown_and_not_supported() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let agents = parse_agents(
        r#"
[[agent]]
id = "mine"
name = "Mine"
bin = "mine"
config_dirs = ["~/.mine"]
version_args = ["--version"]
transport = "mine-transport"
"#,
    )
    .unwrap();
    let mine = &agents[0];

    assert_eq!(trust_file(mine, &host), None);
    assert_eq!(workspace_trust(mine, &host, &dir), TrustState::Unknown);
    assert_eq!(
        grant_workspace_trust(mine, &host, &dir),
        Err(TrustError::NotSupported)
    );
    assert_eq!(
        workspace_trust(builtin("opencode"), &host, &dir),
        TrustState::Unknown
    );
}

#[test]
fn custom_agent_can_declare_a_trust_kind() {
    let agents = parse_agents(
        r#"
[[agent]]
id = "mine"
name = "Mine"
bin = "mine"
config_dirs = ["~/.mine"]
version_args = ["--version"]
transport = "mine-transport"

[agent.trust]
kind = "codex_toml"
"#,
    )
    .unwrap();
    let host = FakeHost::new();
    assert_eq!(
        trust_file(&agents[0], &host),
        Some(host.home_path().join(".codex/config.toml"))
    );
}

#[test]
fn missing_dir_cannot_be_granted() {
    let host = FakeHost::new();
    let gone = host.home_path().join("nope");
    let err = grant_workspace_trust(builtin("claude"), &host, &gone).unwrap_err();
    assert!(matches!(err, TrustError::Io(_)), "{err:?}");
    assert!(!host.home_path().join(".claude.json").exists());
}

// ---- Write safety ------------------------------------------------------

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn codex_file(host: &FakeHost, body: &str) -> PathBuf {
    let codex_dir = host.home_path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let file = codex_dir.join("config.toml");
    std::fs::write(&file, body).unwrap();
    file
}

#[test]
fn temp_file_is_private_at_creation_and_gets_the_original_mode() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let file = host.home_path().join(".claude.json");
    std::fs::write(&file, "{}").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut seen = Vec::new();

    grant_workspace_trust_hooked(builtin("claude"), &host, &dir, &mut |stage| match stage {
        TrustWriteStage::TempCreated { tmp } => seen.push(("created", mode_of(tmp))),
        TrustWriteStage::BeforeRename { tmp, .. } => seen.push(("rename", mode_of(tmp))),
    })
    .unwrap();
    assert_eq!(seen, [("created", 0o600), ("rename", 0o644)]);
    assert_eq!(mode_of(&file), 0o644);
}

#[test]
fn external_change_between_read_and_rename_is_kept_by_the_retry() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let file = host.home_path().join(".claude.json");
    std::fs::write(&file, r#"{"a":1}"#).unwrap();
    let mut attempts = Vec::new();

    grant_workspace_trust_hooked(builtin("claude"), &host, &dir, &mut |stage| {
        if let TrustWriteStage::BeforeRename { attempt, file, .. } = stage {
            attempts.push(attempt);
            if attempt == 0 {
                // Same length, so a metadata-only check could miss it.
                std::fs::write(file, r#"{"b":2}"#).unwrap();
            }
        }
    })
    .unwrap();
    assert_eq!(attempts, [0, 1]);
    assert_eq!(
        json(&file),
        serde_json::json!({ "b": 2, "projects": { key(&dir): { "hasTrustDialogAccepted": true } } })
    );
}

#[test]
fn file_that_keeps_changing_gives_conflict_and_no_leftovers() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let file = host.home_path().join(".claude.json");
    std::fs::write(&file, "{}").unwrap();
    let mut n = 0;

    let err = grant_workspace_trust_hooked(builtin("claude"), &host, &dir, &mut |stage| {
        if let TrustWriteStage::BeforeRename { file, .. } = stage {
            n += 1;
            std::fs::write(file, format!(r#"{{"n":{n}}}"#)).unwrap();
        }
    })
    .unwrap_err();
    assert_eq!(err, TrustError::Conflict);
    assert_eq!(n, 3);
    assert_eq!(read(&file), r#"{"n":3}"#);
    let names: Vec<String> = std::fs::read_dir(host.home_path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.contains(".claude.json"))
        .collect();
    assert_eq!(names, [".claude.json"]);
}

#[test]
fn symlinked_config_file_is_written_through_to_its_target() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let dotfiles = host.home_path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    let target = dotfiles.join("claude.json");
    std::fs::write(&target, r#"{"x":1}"#).unwrap();
    let link = host.home_path().join(".claude.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    grant_workspace_trust(builtin("claude"), &host, &dir).unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        json(&target)["projects"][key(&dir)]["hasTrustDialogAccepted"],
        true
    );
    assert_eq!(json(&target)["x"], 1);
}

#[test]
fn dangling_config_symlink_is_an_error_and_untouched() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let target = host.home_path().join("missing").join("claude.json");
    let link = host.home_path().join(".claude.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = grant_workspace_trust(builtin("claude"), &host, &dir).unwrap_err();
    assert!(matches!(err, TrustError::Io(_)), "{err:?}");
    assert_eq!(std::fs::read_link(&link).unwrap(), target);
    assert!(!host.home_path().join("missing").exists());
}

#[test]
fn already_trusted_entries_are_not_rewritten() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let codex_body = format!(
        "[projects.\"{}\"]\ntrust_level   =  'trusted'   # ok\n",
        key(&dir)
    );
    let codex = codex_file(&host, &codex_body);
    let claude_body = format!(
        "{{ \"projects\" : {{ \"{}\" : {{ \"hasTrustDialogAccepted\" : true }} }} }}",
        key(&dir)
    );
    let claude = host.home_path().join(".claude.json");
    std::fs::write(&claude, &claude_body).unwrap();

    for id in ["codex", "claude"] {
        grant_workspace_trust_hooked(builtin(id), &host, &dir, &mut |_| {
            panic!("{id}: no write expected")
        })
        .unwrap();
    }
    assert_eq!(read(&codex), codex_body);
    assert_eq!(read(&claude), claude_body);
}

#[test]
fn codex_comment_and_spacing_on_the_entry_are_kept() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let body = |level: &str| {
        format!(
            "# header\n[projects.\"{k}\"]\n# why\ntrust_level   =   \"{level}\"   # set by me\nother = 1\n",
            k = key(&dir)
        )
    };
    let file = codex_file(&host, &body("untrusted"));

    grant_workspace_trust(builtin("codex"), &host, &dir).unwrap();
    assert_eq!(read(&file), body("trusted"));
}

#[test]
fn codex_inline_tables_are_supported() {
    let host = FakeHost::new();
    let dir = project(&host, "p");
    let file = codex_file(
        &host,
        &format!(
            "projects = {{ \"{}\" = {{ trust_level = \"untrusted\" }}, \"/a\" = {{ trust_level = \"trusted\" }} }}\n",
            key(&dir)
        ),
    );
    let codex = builtin("codex");

    grant_workspace_trust(codex, &host, &dir).unwrap();
    assert_eq!(
        read(&file),
        format!(
            "projects = {{ \"{}\" = {{ trust_level = \"trusted\" }}, \"/a\" = {{ trust_level = \"trusted\" }} }}\n",
            key(&dir)
        )
    );

    let other = project(&host, "q");
    grant_workspace_trust(codex, &host, &other).unwrap();
    assert_eq!(workspace_trust(codex, &host, &other), TrustState::Trusted);
    assert_eq!(workspace_trust(codex, &host, &dir), TrustState::Trusted);
    assert!(read(&file).starts_with(&format!(
        "projects = {{ \"{}\" = {{ trust_level = \"trusted\" }}, \"/a\" = {{ trust_level = \"trusted\" }}",
        key(&dir)
    )));
}
