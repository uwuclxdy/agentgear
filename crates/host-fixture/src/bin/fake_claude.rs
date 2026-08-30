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
//! Every invocation is also appended to `<config-dir>/fake-claude-calls.log`, one
//! line per call, for the tests whose subject is which calls ran rather than what the
//! registry ended up holding. `FAKE_CLAUDE_FAIL_INSTALL` in the environment makes
//! `plugin install` fail, which is the only way a test reaches a lifecycle that
//! uninstalled and never got the plugin back.
//!
//! State lives in `<config-dir>/fake-claude-state.json`, where `<config-dir>` is
//! `CLAUDE_CONFIG_DIR` (else `$HOME/.claude`). An empty `CLAUDE_CONFIG_DIR` resolves
//! cwd-relative rather than falling back to `$HOME`, matching real `claude` 2.1.220's
//! config-dir joins (probed 2026-07-26; see `docs/design.md` § empty `CLAUDE_CONFIG_DIR`).
//!
//! It is also SCOPE-BLIND: one flat registry per config dir, with `--scope` and the
//! process cwd swallowed by the argument rest-patterns. Every test driving it today is
//! user-scope; a project-scope test would pass here for the wrong reason, so widen this
//! before writing one.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

/// Exactly the crate's 2.1.196 floor, which `ensure_min_version` gates with `<`, so
/// equal passes.
const VERSION_LINE: &str = "2.1.196 (Claude Code)";

/// The state file's name, joined onto whatever `resolve_config_dir` returns.
const STATE_FILENAME: &str = "fake-claude-state.json";

/// Set in a child's env to make `plugin install` fail, for the tests whose subject is
/// what a half-completed reinstall leaves behind.
const FAIL_INSTALL_VAR: &str = "FAKE_CLAUDE_FAIL_INSTALL";

/// One line per invocation, beside the state file. The registry alone cannot answer
/// "did the backend reinstall?" — an uninstall + install of the same id leaves exactly
/// the state that was there before, so a test asserting a reinstall (or asserting that
/// a converged pass ran none) has to read the calls.
const CALL_LOG_FILENAME: &str = "fake-claude-calls.log";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    log_call(&argv);
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
            println!("{}", render(&listed_plugins(State::load())));
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

/// The `plugin list --json` view: every installed plugin plus the `errors` CC
/// computes at list time for one whose marketplace no longer loads. The real
/// failure mode this models (probed 2.1.241): a directory-source marketplace whose
/// `.claude-plugin/marketplace.json` vanished registers the plugin entry — files
/// intact — with `["Marketplace <mkt> failed to load: cache-miss"]`, and CC serves
/// 0 hooks and 0 MCP from it.
fn listed_plugins(state: State) -> Vec<Value> {
    let mut plugins = state.plugins.clone();
    for plugin in &mut plugins {
        let Some(marketplace) = plugin["id"].as_str().and_then(|id| id.split_once('@').map(|(_, m)| m.to_string())) else {
            continue;
        };
        let Some(entry) = state.marketplaces.iter().find(|m| m["name"] == json!(marketplace)) else {
            continue;
        };
        let broken = entry["path"].as_str().is_some_and(|path| !Path::new(path).join(".claude-plugin").join("marketplace.json").exists());
        if broken {
            plugin["errors"] = json!([format!("Marketplace {marketplace} failed to load: cache-miss")]);
        }
    }
    plugins
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
        // A no-op for a healthy local marketplace: the source dir IS the live copy.
        // A dir whose manifest vanished fails like the real CLI's EISDIR refresh
        // failure (probed 2.1.241), so a heal that picks `update` over a re-pointing
        // `add` reds instead of converging.
        ["update", name, ..] => {
            let state = State::load();
            let broken = state.marketplaces.iter().any(|m| {
                m["name"] == json!(name)
                    && m["path"].as_str().is_some_and(|path| !Path::new(path).join(".claude-plugin").join("marketplace.json").exists())
            });
            if broken {
                eprintln!(
                    "Failed to update marketplace(s): Failed to refresh marketplace '{name}': EISDIR: illegal operation on a directory, read"
                );
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        ["remove", name, ..] => mutate(|state| state.marketplaces.retain(|m| m["name"] != json!(name))),
        other => usage(other),
    }
}

/// `plugin install <name>@<marketplace>`. Refuses when that marketplace is not
/// registered — the one ordering constraint the backend's `ensure_marketplace` step
/// exists to satisfy, so the double must not let a regression there pass.
fn install(id: &str) -> ExitCode {
    // The one injectable failure: a lifecycle whose install half fails is otherwise
    // unreachable from a test, and it is the half that leaves the registry empty
    // after the backend's own uninstall has already run.
    if std::env::var_os(FAIL_INSTALL_VAR).is_some() {
        eprintln!("fake-claude: refusing to install `{id}` ({FAIL_INSTALL_VAR} is set)");
        return ExitCode::FAILURE;
    }
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
        // The real CLI's user-scope entries carry `"scope": "user"`; the backend's
        // scoped lookup reads it, so the double models it.
        state.plugins.push(json!({"id": id, "enabled": true, "scope": "user"}));
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
    config_dir().join(STATE_FILENAME)
}

fn config_dir() -> PathBuf {
    resolve_config_dir(std::env::var_os("CLAUDE_CONFIG_DIR"), std::env::var_os("HOME"))
}

/// Append this invocation to the call log, best-effort: a double that fails a
/// lifecycle call because its own log could not be written would red the test for the
/// wrong reason.
fn log_call(argv: &[&str]) {
    let path = config_dir().join(CALL_LOG_FILENAME);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(file, "{}", argv.join(" "));
}

/// Pure resolution behind `state_path()`, taking both env lookups as parameters so it's
/// testable without mutating process env. A `Some(empty string)` `config_dir` resolves
/// literally (cwd-relative), never falling back to `home` — see the module doc.
fn resolve_config_dir(config_dir: Option<OsString>, home: Option<OsString>) -> PathBuf {
    match config_dir {
        Some(dir) => PathBuf::from(dir),
        None => home.map(|home| PathBuf::from(home).join(".claude")).unwrap_or_else(|| PathBuf::from(".claude")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_dir_resolves_cwd_relative_no_home_fallback() {
        let dir = resolve_config_dir(Some(OsString::new()), Some(OsString::from("/home/someone")));
        assert_eq!(dir.join(STATE_FILENAME), PathBuf::from(STATE_FILENAME));
    }

    #[test]
    fn explicit_config_dir_wins_over_home() {
        let dir = resolve_config_dir(Some(OsString::from("/explicit/dir")), Some(OsString::from("/home/someone")));
        assert_eq!(dir.join(STATE_FILENAME), PathBuf::from("/explicit/dir").join(STATE_FILENAME));
    }

    #[test]
    fn unset_config_dir_falls_back_to_home_dot_claude() {
        let dir = resolve_config_dir(None, Some(OsString::from("/home/someone")));
        assert_eq!(dir.join(STATE_FILENAME), PathBuf::from("/home/someone/.claude").join(STATE_FILENAME));
    }

    #[test]
    fn unset_config_dir_and_no_home_falls_back_to_dot_claude() {
        let dir = resolve_config_dir(None, None);
        assert_eq!(dir.join(STATE_FILENAME), PathBuf::from(".claude").join(STATE_FILENAME));
    }
}
