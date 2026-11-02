//! Streaming child processes: `Host::spawn` + `ChildProcess`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_detect::host::{FakeHost, Host, NativeHost, SpawnSpec};

fn native_spec(args: &[&str], stdin: &str, cwd: PathBuf) -> SpawnSpec {
    SpawnSpec {
        program: PathBuf::from("/bin/sh"),
        args: args.iter().map(|a| a.to_string()).collect(),
        cwd,
        path_env: vec![PathBuf::from("/bin"), PathBuf::from("/usr/bin")],
        stdin: stdin.to_string(),
    }
}

#[test]
fn fake_stream_yields_lines_then_status() {
    let host = FakeHost::new();
    host.script_stream(
        "/bin/sh",
        vec!["first".to_string(), "second".to_string()],
        "noise",
        Some(0),
    );
    let spec = native_spec(
        &["-c", "ignored"],
        "prompt\n",
        host.home_path().to_path_buf(),
    );
    let mut child = host.spawn(&spec).unwrap();
    assert_eq!(child.next_line().as_deref(), Some("first"));
    assert_eq!(child.next_line().as_deref(), Some("second"));
    assert_eq!(child.next_line(), None);
    assert_eq!(child.stderr(), "noise");
    assert_eq!(child.wait(), Some(0));

    let spawned = host.spawned();
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].program, PathBuf::from("/bin/sh"));
    assert_eq!(spawned[0].stdin, "prompt\n");
}

#[test]
fn native_spawn_reads_stdout_and_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let host = NativeHost;
    let spec = native_spec(&["-c", "cat; echo done"], "hi\n", dir.path().to_path_buf());
    let mut child = host.spawn(&spec).unwrap();
    assert_eq!(child.next_line().as_deref(), Some("hi"));
    assert_eq!(child.next_line().as_deref(), Some("done"));
    assert_eq!(child.next_line(), None);
    assert_eq!(child.wait(), Some(0));
}

#[test]
fn native_spawn_kill_stops_child() {
    let dir = tempfile::tempdir().unwrap();
    let host = NativeHost;
    let spec = native_spec(&["-c", "sleep 30"], "", dir.path().to_path_buf());
    let mut child = host.spawn(&spec).unwrap();
    child.kill();
    let start = Instant::now();
    let status = child.wait();
    assert!(start.elapsed() < Duration::from_secs(2));
    // Killed by a signal: no exit code.
    assert_eq!(status, None);
}

#[test]
fn large_stdin_does_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let host = NativeHost;
    let big = "x".repeat(1024 * 1024);
    let spec = native_spec(
        &[
            "-c",
            "head -c 1048576 /dev/zero | tr '\\0' x; echo; echo started; cat >/dev/null; echo done",
        ],
        &big,
        dir.path().to_path_buf(),
    );
    let start = Instant::now();
    let mut child = host.spawn(&spec).unwrap();
    // The whole MiB cannot fit in the pipe buffer: `spawn` must hand the
    // writing to its own thread and return before the child consumes it.
    assert!(start.elapsed() < Duration::from_secs(2));
    let flood = child.next_line().expect("flood line");
    assert_eq!(flood.len(), 1048576);
    assert_eq!(child.next_line().as_deref(), Some("started"));
    assert_eq!(child.next_line().as_deref(), Some("done"));
    assert_eq!(child.next_line(), None);
    assert_eq!(child.wait(), Some(0));
}

#[test]
fn wait_kills_a_child_that_outlives_its_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let script = format!("echo $$ > {}; exec 1>&-; sleep 60", pidfile.display());
    let host = NativeHost;
    let mut child = host
        .spawn(&native_spec(&["-c", &script], "", dir.path().to_path_buf()))
        .unwrap();
    assert_eq!(child.next_line(), None);
    let started = Instant::now();
    let _ = child.wait();
    assert!(started.elapsed() < Duration::from_secs(10));
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "child must not be left running"
    );
}
