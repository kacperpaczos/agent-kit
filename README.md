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

## License

Apache-2.0 (see `LICENSE`).
