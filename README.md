# agent-kit

The workspace contains two crates:

- **`agent-detect`** — which agent CLIs are installed on a machine, their
  versions, and whether they are logged in. Currently it knows **Claude
  Code**, **Codex**, and **OpenCode** (plus fallback binary names such as
  `openclaude` and `opencode-cli`).
- **`agent-run`** — give a detected agent one task and stream its output
  as normalized events, with a cooperative `Stop` flag.

The crates have no host-app dependency and can be embedded in any app.

## Install

Add the crates as git dependencies in `Cargo.toml`:

```toml
agent-detect = { git = "https://github.com/kacperpaczos/agent-kit" }
agent-run = { git = "https://github.com/kacperpaczos/agent-kit" }
```

`FakeHost` (used in tests) requires the `test-support` feature:
`agent-detect = { git = "https://github.com/kacperpaczos/agent-kit", features = ["test-support"] }`.

Tested with Claude Code 2.1.316, Codex CLI 0.165.0, and OpenCode 2.1.4
(Linux).

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

## Terminal recipes

An app with an embedded terminal can open an agent interactively in a
working directory, with a context text and an approval mode (`Auto`,
`Ask`, `Plan`), without hard-coding any agent's flags. Each agent may carry
an optional `[agent.terminal]` table:

```toml
[agent.terminal]
args = []                 # fixed leading args (optional)
cwd_flag = "-C"           # optional; without it only the process cwd is set
context = { kind = "config_flag", flag = "-c", key = "developer_instructions" }
# or first_arg, system_prompt_flag / prompt_flag (with `flag`), or none

[agent.terminal.modes]    # extra args per supported mode; a missing mode is unsupported
auto = ["-a", "never", "-s", "workspace-write"]
plan = ["-a", "on-request", "-s", "read-only"]
```

```rust
use agent_detect::terminal::{ApprovalMode, context_in_argv, supported_modes, terminal_launch};

let launch = terminal_launch(agent, &bin, &workdir, "Context for the session", ApprovalMode::Ask);
// spawn launch.program with launch.args in launch.cwd (argv only, no shell);
// launch.mode is None when the agent does not support the requested mode.
if !context_in_argv(agent) {
    // type the context as the first message once the agent is up
}
```

argv is built as: recipe `args`, then `cwd_flag <cwd>`, then the mode's
args, then the context (`[flag, context]` for the flag kinds;
`[flag, key="<context as a TOML string>"]` for `config_flag`; `-- context`
last for `first_arg`). An agent without a terminal table starts
as its plain binary with no arguments. The bundled recipes never use
permission-bypass flags; in this list Claude Code and Codex support all
three modes and OpenCode supports `Auto` and `Ask`.


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
