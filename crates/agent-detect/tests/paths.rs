//! Tests for `agent_detect::paths::expand`.

use std::path::PathBuf;

use agent_detect::host::FakeHost;
use agent_detect::paths::expand;

#[test]
fn expand_tilde() {
    let h = FakeHost::new();
    assert_eq!(
        expand("~/.claude/skills", &h),
        Some(h.home_path().join(".claude/skills"))
    );
}

#[test]
fn expand_alternative_prefers_set_var() {
    let mut h = FakeHost::new();
    h.set_var("CLAUDE_CONFIG_DIR", "/x");
    assert_eq!(
        expand("$CLAUDE_CONFIG_DIR/skills || ~/.claude/skills", &h),
        Some(PathBuf::from("/x/skills"))
    );
}

#[test]
fn expand_alternative_falls_back_when_unset_or_empty() {
    let mut h = FakeHost::new();
    let expected = Some(h.home_path().join(".claude/skills"));
    assert_eq!(
        expand("$CLAUDE_CONFIG_DIR/skills || ~/.claude/skills", &h),
        expected
    );
    h.set_var("CLAUDE_CONFIG_DIR", "");
    assert_eq!(
        expand("$CLAUDE_CONFIG_DIR/skills || ~/.claude/skills", &h),
        expected
    );
}

#[test]
fn expand_braced_var() {
    let mut h = FakeHost::new();
    h.set_var("XDG_STATE_HOME", "/s");
    assert_eq!(
        expand("${XDG_STATE_HOME}/a", &h),
        Some(PathBuf::from("/s/a"))
    );
}

#[test]
fn expand_none_when_no_alternative_resolves() {
    let h = FakeHost::new();
    assert_eq!(expand("$NOPE/x", &h), None);
}

#[test]
fn expand_var_name_stops_at_first_non_name_char() {
    let mut h = FakeHost::new();
    h.set_var("AB", "1");
    h.set_var("A_B2", "/z");
    assert_eq!(expand("$AB", &h), Some(PathBuf::from("1")));
    assert_eq!(expand("$A_B2/x", &h), Some(PathBuf::from("/z/x")));
}

#[test]
fn expand_dollar_without_name_is_literal() {
    let h = FakeHost::new();
    assert_eq!(expand("/a$/b", &h), Some(PathBuf::from("/a$/b")));
    assert_eq!(expand("${X", &h), Some(PathBuf::from("${X")));
}
