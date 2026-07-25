# Testing your host

A config-merge backend only ever writes the target tool's own config files, and every path it
resolves derives from the environment. Point `HOME` and the XDG dirs at a temp root, then spawn
your binary and read back what it wrote. No docker, no auth, no harness installed on the machine.

Every host under [`examples/`](https://github.com/uwuclxdy/agentgear/tree/HEAD/examples) tests
this way in plain `cargo test`. The crate's own `host-fixture` package tests the same way with
fuller per-backend assertions; it is `publish = false` and internal, so copy its pattern rather
than depending on it.

## The shape

One test file with a temp root per test, driving the binary as a subprocess:

```rust
// tests/lifecycle.rs
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_mytool");

/// Every per-tool config-dir override a backend reads. Redirecting `HOME` does not
/// reach these: one of them exported on the dev box points that backend at a real
/// config dir, which both passes its `detect()` and receives the test's writes.
/// Clear each one, so the redirected `HOME` is the only config root the run sees.
const OVERRIDES: &[&str] = &[
    "CODEX_HOME",
    "KIRO_HOME",
    "QWEN_HOME",
    "KIMI_CODE_HOME",
    "CRUSH_GLOBAL_CONFIG",
    "GOOSE_PATH_ROOT",
    "PI_CODING_AGENT_DIR",
    "PI_CONFIG_DIR",
    "CLINE_DIR",
    "CLINE_DATA_DIR",
    "CLINE_MCP_SETTINGS_PATH",
    "OPENCLAW_CONFIG_PATH",
    "OPENCLAW_STATE_DIR",
    "OPENCLAW_HOME",
];

/// A foreign server and an unrelated key that must outlive install and uninstall.
const SEED: &str = r#"{
  "theme": "dark",
  "mcpServers": { "theirs": { "command": "their-server", "args": [] } }
}
"#;

struct Env {
    root: PathBuf,
    gemini: PathBuf,
}

impl Env {
    fn new(name: &str) -> Self {
        // `process::id()` is constant across a binary's tests, so `name` keeps two
        // tests' roots (and their lock files) distinct.
        let root = std::env::temp_dir().join(format!("mytool-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let env = Env { gemini: root.join(".gemini"), root };
        for dir in ["config", "data", "run"] {
            fs::create_dir_all(env.root.join(dir)).unwrap();
        }
        // Pre-creating `~/.gemini` is what makes gemini's `detect()` pass.
        fs::create_dir_all(&env.gemini).unwrap();
        fs::write(env.gemini.join("settings.json"), SEED).unwrap();
        env
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            // PATH holding only our own binary's dir: no harness CLI resolves,
            // so detection rides purely on the dirs this env pre-creates.
            .env("PATH", Path::new(BIN).parent().unwrap());
        for var in OVERRIDES {
            cmd.env_remove(var);
        }
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn gemini_lifecycle() {
    let env = Env::new("gemini");
    let settings = env.gemini.join("settings.json");

    let (ok, out) = env.run(&["setup"]);
    assert!(ok, "setup failed: {out}");
    assert_eq!(out, "Installed", "first setup should install, got {out}");

    let s = fs::read_to_string(&settings).unwrap();
    assert!(s.contains("mytool"), "our mcp server key missing:\n{s}");
    assert!(s.contains("their-server"), "the seeded server was clobbered:\n{s}");
    assert!(s.contains("dark"), "the seeded top-level key was clobbered:\n{s}");

    // A second identical reconcile is a true NoOp.
    let (ok, out) = env.run(&["setup"]);
    assert!(ok && out == "NoOp", "second setup should no-op, got {out}");

    let (ok, out) = env.run(&["uninstall"]);
    assert!(ok && out == "Removed", "uninstall failed: {out}");

    let s = fs::read_to_string(&settings).unwrap();
    assert!(!s.contains("mytool"), "our mcp server survived uninstall:\n{s}");
    assert!(s.contains("their-server"), "uninstall removed the seeded server:\n{s}");
}
```

The assertions that carry the weight: our entries landed, and the seeded foreign entries survived
both directions. Seed before install, not after: `remove` prunes a container our own keys emptied,
and on a json config it deletes a file left holding nothing, so an unseeded fixture may have no
file to read back. A yaml config keeps its file either way, since the editor cannot preserve comments
and dropping the file would risk more than an empty one costs. A repeat `setup` reporting `NoOp`
covers a third thing for free, since the backend has to read the written file back to compare it.

## What each redirect covers

| var | covers |
|---|---|
| `HOME` | every HOME-based config dir (`~/.gemini`, `~/.cursor`, `~/.codex`, …) and the fallback under each XDG var |
| `XDG_CONFIG_HOME` | the config-dir family (amp, crush, zed, kilo, devin, jetbrains-copilot, goose, opencode) |
| `XDG_DATA_HOME` | agentgear's own `<data dir>/<plugin-name>/`: the materialized tree, the per-client `current@<client>` pointer, the stamp markers, the restart flag |
| `XDG_RUNTIME_DIR` | the shared lock file (see below) |
| `PATH` | every backend's `which(<tool>)` detection arm |

Backends with their own config-dir env var are the `OVERRIDES` list above, described per backend in
[Detection § environment overrides](Capability-Detection#environment-overrides). Setting one
redirects that tool without moving `HOME`, which is how `examples/multi-installer` keeps a codex
config beside a gemini config in one temp root. Clear the rest in the same call: an override wins
over the redirected `HOME`, so one left inherited from the dev's shell aims that backend at a real
config dir.

## Getting a backend detected

`detect()` is `which(<tool>)` or the tool's config dir existing. Stripping `PATH` down to your own
binary's directory kills the `which` arm for every backend at once, so pre-creating a config dir
becomes the only detection switch you hold. That is what makes a single-backend test single-backend:
the other 24 stay undetected and are skipped, whatever the developer has installed.

Scope is the second gate: a backend is skipped when the requested scope is missing from its
`capabilities().scopes`, so a `Scope::Project { path }` run only reaches the backends declaring
`project`. Test project scope by pointing `path` at a directory inside the same temp root.

## The shared lock

`install`, `install_into`, `update`, `uninstall`, and `self_heal` each take one exclusive `flock` at
`agentgear.lock` under `dirs::runtime_dir()`, falling back to `std::env::temp_dir()`. `doctor` takes
no lock.

On Linux `runtime_dir()` is `$XDG_RUNTIME_DIR`, so giving every test root its own value keeps
parallel tests independent. On macOS and Windows it returns `None`, so every test in the process
falls back to one temp-dir lock and the lifecycle calls serialize. Tests stay correct, they just run
one at a time.

## What a hermetic test cannot cover

The two plugin-native backends, `claude` and `copilot-cli`. Both detect on `which` alone and both
drive the real CLI as the transaction boundary, so registry state has no file-level path to assert
against. One exception: a host that declares a [status line](Capability-Status-Line) can assert
the `statusLine` key in `<CLAUDE_CONFIG_DIR>/settings.json` like any config-merge write, since
that slot is a plain file the backend read-modify-writes.

`examples/from-github/tests/wiring.rs` shows how far a stub gets you: a `/bin/sh` shim answering
`--version` with a supported version and `list --json` with `[]` is enough for the claude backend to
be detected and for `reconcile` to run as far as materialize. That reaches materialize errors
deterministically. It proves nothing about registry state, which only the real CLI holds.

Real coverage is an `#[ignore]`d test against an installed `claude`, run with
`cargo test -- --ignored`, isolated by `CLAUDE_CONFIG_DIR` on top of the temp `HOME`. The crate's
own `crates/host-fixture/tests/e2e.rs` is that test. `CLAUDE_CONFIG_DIR` is deliberately kept out of
the env scrub agentgear applies to CLI children, so an isolated test root survives into the spawned
`claude`; the same applies to `COPILOT_HOME` and the GitHub token vars for `copilot`.

## Gotchas

> [!IMPORTANT]
> An MCP server or hook command carrying `${CLAUDE_PLUGIN_ROOT}` is skipped by all 23 config-merge
> backends. The token expands only inside Claude Code's plugin runtime. Nothing errors and nothing
> warns, so a test asserting that server reached gemini's `settings.json` fails with no explanation
> anywhere. Use a bare binary name in any component you expect a non-CC harness to receive.

- `env!("CARGO_BIN_EXE_<name>")` only resolves in a test inside the binary's own package. A test in
  a separate crate has to locate the binary itself.
- Redirect the environment on the child `Command`, never in-process. `std::env::set_var` is `unsafe`
  in edition 2024, which a crate carrying `unsafe_code = "forbid"` cannot call.
- `assert!(path.starts_with(&temp_root))` on a path the test itself joined under the root is true
  by construction and can never fail. Isolation comes from the redirected env, not from that line:
  clear the per-tool overrides, and a misresolved path shows up as the `read_to_string` failing.
  The check earns its place only on a path the binary reports back, such as a `doctor` line.
- Give each test its own root name. `std::process::id()` is constant across every test in one
  binary, so a shared root means one test wiping another's files mid-run.

## Where to copy from

| file | shows |
|---|---|
| `examples/kitchen-sink/tests/lifecycle.rs` | two backends, one HOME-based and one XDG-based, with hooks and a command asserted |
| `examples/multi-installer/tests/picker.rs` | `--agent` filtering proven by a second detected backend staying byte-identical |
| `examples/from-github/tests/wiring.rs` | the fake-`claude` shim and a network-gated GitHub install |
| `examples/hello-mcp/tests/wiring.rs` | derive metadata and embed assertions with no environment at all |
