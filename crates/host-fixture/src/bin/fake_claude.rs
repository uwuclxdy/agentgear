//! A test double for the `claude` CLI, copied onto a scratch `PATH` as `claude` by
//! the hermetic Claude-backend tests.
//!
//! The real CLI is the transaction boundary the claude backend orchestrates, so
//! every backend behavior that is NOT the registry — the `statusLine` slot, the
//! stamp-marker stash, doctor's local checks — was previously reachable only from
//! the `--ignored` e2e leg, which needs a real `claude` and an auth'd machine. This
//! binary models just enough of the registry (`plugin list`/`install`/`uninstall`/
//! `enable`/`disable`, `marketplace list`/`add`/`update`/`remove`, `--version`,
//! `validate`) for those tests to run in the normal `cargo test` job.
//!
//! It is a DOUBLE, never a spec: `docs/design.md`'s ground-truth CLI schemas and the
//! `--ignored` e2e leg against the real binary stay the authority on CC's behavior.
//! It deliberately models the ordering constraint that matters (`plugin install`
//! fails when the plugin's marketplace is not registered) and deliberately omits
//! `version`/`installPath` from its entries, which the crate's tolerant serde models
//! read as "unknown, assume fine" rather than letting the double invent a version.
//!
//! State lives in `<config-dir>/fake-claude-state.json`, where `<config-dir>` is
//! `CLAUDE_CONFIG_DIR` (else `$HOME/.claude`) — the same resolution the real CLI uses.
//!
//! It is also SCOPE-BLIND: one flat registry per config dir, with `--scope` and the
//! process cwd swallowed by the argument rest-patterns. Every test driving it today is
//! user-scope; a project-scope test would pass here for the wrong reason, so widen this
//! before writing one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

/// Exactly the crate's 2.1.196 floor, which `ensure_min_version` gates with `<`, so
/// equal passes.
const VERSION_LINE: &str = "2.1.196 (Claude Code)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match argv.as_slice() {
        ["--version"] | ["-v"] => {
            println!("{VERSION_LINE}");
            ExitCode::SUCCESS
        }
        ["plugin", rest @ ..] => plugin(rest),
        other => usage(other),
    }
}

fn plugin(argv: &[&str]) -> ExitCode {
    match argv {
        ["list", ..] => {
            println!("{}", render(&State::load().plugins));
            ExitCode::SUCCESS
        }
        ["marketplace", rest @ ..] => marketplace(rest),
        ["install", id, ..] => install(id),
        ["uninstall", id, ..] => mutate(|state| state.plugins.retain(|p| p["id"] != json!(id))),
        ["enable", id, ..] => set_enabled(id, true),
        ["disable", id, ..] => set_enabled(id, false),
        // The real CLI validates a manifest tree; the double accepts any path, since
        // nothing here can vouch for CC's schema.
        ["validate", ..] => ExitCode::SUCCESS,
        other => usage(other),
    }
}

fn marketplace(argv: &[&str]) -> ExitCode {
    match argv {
        ["list", ..] => {
            println!("{}", render(&State::load().marketplaces));
            ExitCode::SUCCESS
        }
        ["add", source, ..] => mutate(|state| {
            let entry = marketplace_entry(source);
            state.marketplaces.retain(|m| m["name"] != entry["name"]);
            state.marketplaces.push(entry);
        }),
        // A no-op for a local marketplace: the source dir IS the live copy.
        ["update", ..] => ExitCode::SUCCESS,
        ["remove", name, ..] => mutate(|state| state.marketplaces.retain(|m| m["name"] != json!(name))),
        other => usage(other),
    }
}

/// `plugin install <name>@<marketplace>`. Refuses when that marketplace is not
/// registered — the one ordering constraint the backend's `ensure_marketplace` step
/// exists to satisfy, so the double must not let a regression there pass.
fn install(id: &str) -> ExitCode {
    let Some((_, wanted)) = id.split_once('@') else {
        eprintln!("fake-claude: `{id}` is not <plugin>@<marketplace>");
        return ExitCode::FAILURE;
    };
    let mut state = State::load();
    if !state.marketplaces.iter().any(|m| m["name"] == json!(wanted)) {
        eprintln!("fake-claude: marketplace `{wanted}` is not registered");
        return ExitCode::FAILURE;
    }
    if !state.plugins.iter().any(|p| p["id"] == json!(id)) {
        state.plugins.push(json!({"id": id, "enabled": true}));
    }
    state.save();
    ExitCode::SUCCESS
}

fn set_enabled(id: &str, enabled: bool) -> ExitCode {
    mutate(|state| {
        for entry in state.plugins.iter_mut().filter(|p| p["id"] == json!(id)) {
            entry["enabled"] = json!(enabled);
        }
    })
}

fn mutate(edit: impl FnOnce(&mut State)) -> ExitCode {
    let mut state = State::load();
    edit(&mut state);
    state.save();
    ExitCode::SUCCESS
}

fn usage(argv: &[&str]) -> ExitCode {
    eprintln!("fake-claude: unmodeled invocation {argv:?}");
    ExitCode::from(2)
}

/// A registered marketplace entry. A local dir carries `path` (what the backend's
/// dangling-source check reads); a `owner/repo@ref` source carries `ref` instead,
/// matching `MarketplaceEntry`'s two shapes.
fn marketplace_entry(source: &str) -> Value {
    let path = Path::new(source);
    if path.is_dir() {
        return json!({"name": local_marketplace_name(path), "path": source});
    }
    let (repo, ref_) = source.split_once('@').unwrap_or((source, ""));
    json!({"name": repo.rsplit('/').next().unwrap_or(repo), "ref": ref_})
}

/// The `name` out of a local marketplace's generated `.claude-plugin/marketplace.json`,
/// falling back to the directory name.
fn local_marketplace_name(dir: &Path) -> String {
    let manifest = fs::read(dir.join(".claude-plugin").join("marketplace.json")).ok();
    let name = manifest
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|doc| doc.get("name").and_then(Value::as_str).map(str::to_string));
    name.unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

fn render(entries: &[Value]) -> String {
    serde_json::to_string_pretty(entries).unwrap_or_else(|_| "[]".to_string())
}

struct State {
    plugins: Vec<Value>,
    marketplaces: Vec<Value>,
}

impl State {
    fn load() -> Self {
        let root: Value = fs::read(state_path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
        let array = |key: &str| root.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        Self { plugins: array("plugins"), marketplaces: array("marketplaces") }
    }

    fn save(&self) {
        let path = state_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let root = json!({"plugins": self.plugins, "marketplaces": self.marketplaces});
        if let Ok(bytes) = serde_json::to_vec_pretty(&root) {
            let _ = fs::write(&path, bytes);
        }
    }
}

fn state_path() -> PathBuf {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
        .unwrap_or_else(|| PathBuf::from(".claude"));
    dir.join("fake-claude-state.json")
}
