//! A test double for the `copilot` CLI, copied onto a scratch `PATH` as `copilot` by
//! the hermetic copilot-cli-backend tests.
//!
//! The real CLI is the transaction boundary the copilot-cli backend orchestrates, so
//! every backend behavior that is NOT the registry — the `statusLine` slot in
//! `$COPILOT_HOME/settings.json`, the stamp-marker stash, doctor's local checks — is
//! otherwise reachable only from the docker leg, which needs a real (auth'd) `copilot`
//! and cannot run under `cargo test`. This binary models just enough of the registry
//! (`plugin list`/`install`/`update`/`uninstall`, `plugin marketplace list`/`add`,
//! `--version`) for those tests to run in the normal `cargo test` job.
//!
//! It is a DOUBLE, never a spec: `docs/research/verify-copilot-cli.md`'s live findings
//! and the docker leg against the real binary stay the authority on copilot's behavior.
//! Its TEXT output is shaped to parse under the real `parse_plugin_list` /
//! `parse_marketplace_list` (`crates/agentgear/src/cli.rs`), whose fixtures in
//! `crates/agentgear/tests/unit/copilot_cli.rs` are the ground truth for these shapes.
//!
//! It deliberately models the ordering constraint that matters (`plugin install` fails
//! when the plugin's marketplace is not registered) and deliberately omits the `(v…)`
//! version column from its rows, which the crate's tolerant parser reads as "unknown,
//! never churn" rather than letting the double invent a version the backend would then
//! compare against the baked one.
//!
//! State lives in `<copilot-home>/fake-copilot-state.json`, where `<copilot-home>` is
//! `COPILOT_HOME` (else `$HOME/.copilot`) — copilot's own nullish-coalescing resolution,
//! kept RAW on purpose, because a double models the vendor rather than us. The backend
//! treats an empty `COPILOT_HOME` as unset (`copilot_home`'s rustdoc says why), so the
//! two diverge on exactly that input: here it resolves relative to the CWD, there to
//! `~/.copilot`. No test sets it; align this side, not the backend, if one ever needs to.
//!
//! copilot installs are user-global with no `--scope`, so unlike the `claude` double
//! there is no scope to be blind to: one registry per copilot home is the real shape.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

/// Exactly the crate's 1.0.71 floor, in copilot's own version-last shape
/// (`copilot_meets_floor` compares `>=`, so equal passes).
const VERSION_LINE: &str = "GitHub Copilot CLI 1.0.71.";

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
            print!("{}", render_plugins(&State::load().plugins));
            ExitCode::SUCCESS
        }
        ["marketplace", rest @ ..] => marketplace(rest),
        ["install", id, ..] => install(id),
        // A local marketplace's source dir IS the live copy, so there is nothing to
        // refresh; the presence gate is the part worth modeling.
        ["update", id, ..] => require_installed(id, |_| {}),
        ["uninstall", id, ..] => require_installed(id, |state| state.plugins.retain(|p| p["id"] != json!(id))),
        other => usage(other),
    }
}

fn marketplace(argv: &[&str]) -> ExitCode {
    match argv {
        ["list", ..] => {
            print!("{}", render_marketplaces(&State::load().marketplaces));
            ExitCode::SUCCESS
        }
        ["add", source, ..] => mutate(|state| {
            let entry = marketplace_entry(source);
            state.marketplaces.retain(|m| m["name"] != entry["name"]);
            state.marketplaces.push(entry);
        }),
        // copilot exposes no `marketplace update` and no `marketplace remove`; the
        // backend never calls either, so an invocation here is a regression to surface.
        other => usage(other),
    }
}

/// `plugin install <plugin>@<marketplace>`. Refuses when that marketplace is not
/// registered — the one ordering constraint the backend's `ensure_marketplace` step
/// exists to satisfy, so the double must not let a regression there pass.
fn install(id: &str) -> ExitCode {
    let Some((_, wanted)) = id.split_once('@') else {
        eprintln!("fake-copilot: `{id}` is not <plugin>@<marketplace>");
        return ExitCode::FAILURE;
    };
    let mut state = State::load();
    if !state.marketplaces.iter().any(|m| m["name"] == json!(wanted)) {
        eprintln!("fake-copilot: marketplace `{wanted}` is not registered");
        return ExitCode::FAILURE;
    }
    if !state.plugins.iter().any(|p| p["id"] == json!(id)) {
        state.plugins.push(json!({"id": id}));
    }
    state.save();
    ExitCode::SUCCESS
}

/// Apply `edit` only when `id` is installed, else fail with copilot's own
/// already-removed wording. `plugin_uninstall` in the backend treats exactly that text
/// as benign, so the phrase is load-bearing, not decoration.
fn require_installed(id: &str, edit: impl FnOnce(&mut State)) -> ExitCode {
    let mut state = State::load();
    if !state.plugins.iter().any(|p| p["id"] == json!(id)) {
        eprintln!("Plugin {id} is not installed");
        return ExitCode::FAILURE;
    }
    edit(&mut state);
    state.save();
    ExitCode::SUCCESS
}

fn mutate(edit: impl FnOnce(&mut State)) -> ExitCode {
    let mut state = State::load();
    edit(&mut state);
    state.save();
    ExitCode::SUCCESS
}

fn usage(argv: &[&str]) -> ExitCode {
    eprintln!("fake-copilot: unmodeled invocation {argv:?}");
    ExitCode::from(2)
}

/// A registered marketplace entry. A local dir carries `path` (what copilot's
/// `(Local: <path>)` row renders); anything else is taken as `owner/repo` and rendered
/// as a `(GitHub: <repo>)` row.
fn marketplace_entry(source: &str) -> Value {
    let path = Path::new(source);
    if path.is_dir() {
        return json!({"name": local_marketplace_name(path), "path": source});
    }
    json!({"name": source.rsplit('/').next().unwrap_or(source), "repo": source})
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

/// `copilot plugin list` output. No `(v…)` column: see the module doc.
fn render_plugins(plugins: &[Value]) -> String {
    if plugins.is_empty() {
        return "No plugins installed.\n".to_string();
    }
    let mut out = String::from("Installed plugins:\n");
    for id in plugins.iter().filter_map(|p| p["id"].as_str()) {
        out.push_str(&format!("  \u{2022} {id}\n"));
    }
    out
}

/// `copilot plugin marketplace list` output. The built-in section is always printed,
/// because the parser's whole job there is to keep those rows out of what it counts as
/// ours; the `Registered marketplaces:` header prints even with nothing under it.
fn render_marketplaces(marketplaces: &[Value]) -> String {
    let mut out = String::from("Included with GitHub Copilot:\n  \u{25c6} copilot-plugins (GitHub: github/copilot-plugins)\n\n");
    out.push_str("Registered marketplaces:\n");
    for entry in marketplaces {
        let name = entry["name"].as_str().unwrap_or_default();
        let detail = match (entry["path"].as_str(), entry["repo"].as_str()) {
            (Some(path), _) => format!("Local: {path}"),
            (None, Some(repo)) => format!("GitHub: {repo}"),
            (None, None) => continue,
        };
        out.push_str(&format!("  \u{2022} {name} ({detail})\n"));
    }
    out
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
    let dir = std::env::var_os("COPILOT_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".copilot")))
        .unwrap_or_else(|| PathBuf::from(".copilot"));
    dir.join("fake-copilot-state.json")
}
