# agent-kit

Reusable Rust crate that detects installed AI-agent CLIs on a machine:
which binaries exist, their versions, and whether they are logged in.
Currently it knows **Claude Code**, **Codex**, and **OpenCode** (plus
fallback binary names such as `openclaude` and `opencode-cli`).

SkillCraft uses this crate to drive its "work on a skill with an agent"
feature; the crate itself has no SkillCraft dependency and can be embedded
in any app that needs to find agent CLIs.

## Detection states

Each agent ends up in one state:

- `Absent` — no binary and no config directory.
- `ConfigOnly` — a config directory exists (`~/.claude`, `~/.codex`, …) but no working binary.
- `BinaryOnly` — a binary exists but `--version` fails or is unparseable.
- `Runnable` — version works, but the auth probe fails (needs login).
- `Ready` — version works and the agent is logged in.

## Usage

```rust
use agent_detect::detect::detect_all;
use agent_detect::host::NativeHost;

let host = NativeHost;
for status in detect_all(agent_detect::registry::builtin_agents(), &host) {
    println!("{}: {:?}", status.agent_id, status.state);
}
```

All OS access (processes, environment variables, clock, filesystem) goes
through the `Host` trait, so tests substitute `FakeHost` (feature
`test-support`) and nothing real is touched.

## Your own agent list

The bundled `agents.toml` lists the agents above. To ship your own list,
parse it and pass the result to `detect_all`:

```rust
let agents = agent_detect::registry::parse_agents(&my_toml)?;
let statuses = agent_detect::detect::detect_all(&agents, &host);
```

Each `[[agent]]` table names the binary (and fallbacks), the config
directories (path templates like `$CLAUDE_CONFIG_DIR || ~/.claude`), the
version command, an optional auth probe, and a transport name.


## Running a turn: `agent-run`

The `agent-run` crate builds on `agent-detect`: find an agent, give it a
task, and stream its output as normalized events.

```rust
use agent_detect::detect::detect_all;
use agent_detect::host::NativeHost;
use agent_detect::registry::builtin_agents;
use agent_run::event::AgentEvent;
use agent_run::transport::{Transport, TurnSpec};
use agent_run::turn::{Stop, run_turn};

let host = NativeHost;
let agents = builtin_agents();
let statuses = detect_all(agents, &host);
let (agent, bin) = statuses.iter().find_map(|s| match &s.state {
    agent_detect::status::AgentState::Ready { bin, .. } => agents
        .iter()
        .find(|a| a.id == s.agent_id)
        .map(|a| (a, bin.clone())), // pick whichever agent is logged in
    _ => None,
})
.expect("no ready agent");

let spec = TurnSpec {
    transport: Transport::from_registry(&agent.transport).unwrap(),
    cwd: workdir.path().to_path_buf(), // a working copy, see below
    model: None,
    resume: None,
    message: "Fix the typo in README".to_string(),
};
let mut summary = None;
run_turn(&host, bin, &[], &spec, &Stop::new(), &mut |event| {
    if let AgentEvent::TurnEnd { ok } = event { summary = Some(ok); }
    // Text / ToolCall / Error / NeedsLogin / SessionStarted stream here
});
```

Every turn ends with exactly one `TurnEnd`. `Stop` is a cooperative stop
flag shared with the UI thread; stopping kills the agent's whole process
group, not just the CLI process, so shell-spawned grandchildren die too.

The agent runs with the user's own configuration (no extra sandbox) and
permission prompts disabled, like OpenDesign does — so a turn can write
anywhere the user could. Point `cwd` at a working copy, never at
anything precious.

### Transports and CLI flags

| Transport | Registry name | Agent CLI flags (prompt goes on stdin, never argv) |
|---|---|---|
| ClaudeStreamJson | `claude-stream-json` | `-p --input-format stream-json --output-format stream-json --verbose --permission-mode bypassPermissions` (+ `--model M`, `--resume ID`) |
| CodexExecJson | `codex-exec-json` | `exec --json --skip-git-repo-check --sandbox workspace-write -C <cwd>` (+ `-m M`); resume: `exec resume --json --skip-git-repo-check -c sandbox_mode="workspace-write" -m M <ID>` |
| OpencodeRunJson | `opencode-run-json` | `run --format json --dangerously-skip-permissions --dir <cwd>` (+ `-s ID`, `-m M`) |

## License

Apache-2.0 (see `LICENSE`).
