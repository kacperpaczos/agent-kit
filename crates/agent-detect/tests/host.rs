//! Tests for `Host` implementations: `FakeHost` scripting and `NativeHost` process handling.

use agent_detect::host::{CmdOutput, FakeHost, Host, NativeHost, SpawnError};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn path_env() -> Vec<PathBuf> {
    ["/usr/local/bin", "/usr/bin", "/bin"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

fn ok_output(stdout: &str) -> Result<CmdOutput, SpawnError> {
    Ok(CmdOutput {
        status: Some(0),
        stdout: stdout.to_string(),
        stderr: String::new(),
    })
}

#[test]
fn fake_host_unscripted_run_is_not_found() {
    let h = FakeHost::new();
    let r = h.run(
        Path::new("/bin/true"),
        &args(&["x"]),
        &[],
        Duration::from_secs(1),
    );
    assert_eq!(r, Err(SpawnError::NotFound));
}

#[test]
fn fake_host_records_calls() {
    let mut h = FakeHost::new();
    h.script(Path::new("/bin/echo"), &["hi"], ok_output("hi\n"));
    let out = h
        .run(
            Path::new("/bin/echo"),
            &args(&["hi"]),
            &[],
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(out.stdout, "hi\n");
    assert_eq!(h.calls(), vec![(PathBuf::from("/bin/echo"), args(&["hi"]))]);
}

#[test]
fn fake_host_script_match_is_exact() {
    let mut h = FakeHost::new();
    h.script(Path::new("/bin/echo"), &["hi"], ok_output(""));
    assert_eq!(
        h.run(
            Path::new("/bin/other"),
            &args(&["hi"]),
            &[],
            Duration::from_secs(1)
        ),
        Err(SpawnError::NotFound)
    );
    assert_eq!(
        h.run(
            Path::new("/bin/echo"),
            &args(&["bye"]),
            &[],
            Duration::from_secs(1)
        ),
        Err(SpawnError::NotFound)
    );
}

#[test]
fn fake_host_script_can_return_error() {
    let mut h = FakeHost::new();
    h.script(Path::new("/bin/x"), &["a"], Err(SpawnError::Timeout));
    assert_eq!(
        h.run(
            Path::new("/bin/x"),
            &args(&["a"]),
            &[],
            Duration::from_secs(1)
        ),
        Err(SpawnError::Timeout)
    );
}

#[test]
fn fake_host_var_empty_is_none() {
    let mut h = FakeHost::new();
    h.set_var("A", "");
    h.set_var("B", "x");
    assert_eq!(h.var("A"), None);
    assert_eq!(h.var("B"), Some("x".to_string()));
    assert_eq!(h.var("C"), None);
}

#[test]
fn fake_host_setters() {
    let mut h = FakeHost::new();
    assert_eq!(h.cwd(), None);
    assert_eq!(h.login_shell_path(Duration::from_secs(1)), None);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(7);
    h.set_now(now);
    h.set_cwd(Some(PathBuf::from("/tmp")));
    h.set_login_path(Some(vec![PathBuf::from("/a")]));
    assert_eq!(h.now(), now);
    assert_eq!(h.cwd(), Some(PathBuf::from("/tmp")));
    assert_eq!(
        h.login_shell_path(Duration::from_secs(1)),
        Some(vec![PathBuf::from("/a")])
    );
}

#[test]
fn native_host_run_times_out() {
    let r = NativeHost.run(
        Path::new("/bin/sh"),
        &args(&["-c", "sleep 5"]),
        &path_env(),
        Duration::from_millis(100),
    );
    assert_eq!(r, Err(SpawnError::Timeout));
}

#[test]
fn native_host_run_captures_stdout() {
    let out = NativeHost
        .run(
            Path::new("/bin/sh"),
            &args(&["-c", "echo hello; echo oops >&2; exit 0"]),
            &path_env(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(out.status, Some(0));
    assert_eq!(out.stdout, "hello\n");
    assert_eq!(out.stderr, "oops\n");
}

#[test]
fn native_host_run_reads_large_output() {
    // >64 KiB overflows the OS pipe buffer; the pipes must be drained while
    // the child runs or the child blocks on write and never exits.
    let out = NativeHost
        .run(
            Path::new("/bin/sh"),
            &args(&["-c", r#"head -c 300000 /dev/zero | tr "\0" a"#]),
            &path_env(),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(out.status, Some(0));
    assert_eq!(out.stdout.len(), 300000);
    assert!(out.stdout.chars().all(|c| c == 'a'));
}

#[test]
fn native_host_run_maps_spawn_errors() {
    assert_eq!(
        NativeHost.run(
            Path::new("/nonexistent-skillcraft-bin"),
            &[],
            &path_env(),
            Duration::from_secs(1)
        ),
        Err(SpawnError::NotFound)
    );
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    std::fs::write(&plain, "#!/bin/sh\n").unwrap();
    assert_eq!(
        NativeHost.run(&plain, &[], &path_env(), Duration::from_secs(1)),
        Err(SpawnError::PermissionDenied)
    );
}

#[test]
fn native_host_env_helpers() {
    assert!(NativeHost.home().is_absolute());
    assert!(NativeHost.var("PATH").is_some_and(|v| v.contains('/')));
    assert!(NativeHost.cwd().is_some_and(|p| p.is_absolute()));
    assert!(
        NativeHost
            .now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            > Duration::from_secs(1_700_000_000)
    );
}

#[test]
fn native_host_login_shell_path() {
    // `-lc` (a *non-interactive* login shell) cannot grab the controlling
    // terminal, so the call is safe in the background process group a GUI
    // session (or cargo-mutants) runs tests in; no skip is needed.
    let path = NativeHost
        .login_shell_path(Duration::from_secs(10))
        .expect("login shell PATH should resolve in this environment");
    assert!(!path.is_empty());
    assert!(path.iter().any(|d| d.is_absolute()));
}

#[test]
fn native_host_children_never_read_stdin() {
    // Every spawned child (login shells included) gets /dev/null on
    // stdin: an interactive child would otherwise SIGTTIN-stop a
    // backgrounded process group and wedge its parent forever.
    let out = NativeHost
        .run(
            Path::new("/bin/sh"),
            &args(&["-c", "readlink /proc/self/fd/0"]),
            &path_env(),
            Duration::from_secs(10),
        )
        .expect("the probe should run");
    assert_eq!(out.status, Some(0), "stdout: {}", out.stdout);
    assert_eq!(out.stdout.trim_end(), "/dev/null");
}
