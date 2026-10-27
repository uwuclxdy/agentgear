//! A test double for the `copilot` CLI, staged onto a scratch `PATH` as `copilot` by
//! the hermetic copilot-cli-backend tests.
//!
//! The real CLI is the transaction boundary the copilot-cli backend orchestrates, so
//! every backend behavior that is NOT the registry — doctor's local checks, the
//! tree-hash staleness gate — is otherwise reachable only from the docker leg, which
//! needs a real (auth'd) `copilot` and cannot run under `cargo test`. This binary
//! models just enough of the registry (`plugin list`/`install`/`update`/`uninstall`,
//! `plugin marketplace list`/`add`/`remove`, `config --rm`, `--version`) for those
//! tests to run in the normal `cargo test` job.
//!
//! It is a DOUBLE, never a spec: the docker leg against the real binary stays the
//! authority on copilot's behavior.
//! Its TEXT output is shaped to parse under the real `parse_plugin_list` /
//! `parse_marketplace_list` (`crates/agentgear/src/cli.rs`), whose fixtures in
//! `crates/agentgear/tests/unit/copilot_cli.rs` are the ground truth for these shapes.
//!
//! It deliberately models the ordering constraint that matters (`plugin install` fails
//! when the plugin's marketplace is not registered). It omits the `(v…)` version column
//! from its rows by default, which the crate's tolerant parser reads as "unknown, never
//! churn" rather than letting the double invent a version the backend would then compare
//! against the baked one; `FAKE_COPILOT_PLUGIN_VERSION` opts a row into a version so a
//! test can drive the Update/Frozen reconcile arms.
//!
//! The version it reports (and the `plugin uninstall` behavior that version implies)
//! switches on `FAKE_COPILOT_VERSION`, defaulting to `1.0.92`: at >= 1.0.81 uninstall
//! DISABLES a local-marketplace plugin (the row stays, marked `(disabled)`, and
//! `enabledPlugins[id]` flips false) instead of removing, and at >= 1.0.92 the
//! `config --rm` subcommand exists. `plugin disable`/`plugin enable` model the
//! deliberate disable the backend reads back as `Disabled` and re-enables only on an
//! explicit install. This lets one test pin the teardown the backend must handle on a
//! live-load copilot without a second binary.
//!
//! Every invocation is also appended to `<copilot-home>/fake-copilot-calls.log`, one
//! line per call, for the tests whose subject is which calls ran.
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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

/// The version the double reports via `--version`; a test overrides it to model the
/// disable-only uninstall (at 1.0.81 and later) and, from 1.0.92, the `config`
/// subcommand — without a second binary.
const VERSION_ENV: &str = "FAKE_COPILOT_VERSION";

/// An optional plugin version the double reports in `plugin list` rows; absent by
/// default (no `(v…)` column) so the tolerant parser reads "unknown, never churn".
const PLUGIN_VERSION_ENV: &str = "FAKE_COPILOT_PLUGIN_VERSION";

/// Default = the crate's 1.0.92 floor, in copilot's own version-last shape
/// (`copilot_meets_floor` compares `>=`, so equal passes).
const DEFAULT_VERSION: &str = "1.0.92";

/// `plugin uninstall` switched from remove to disable-only at 1.0.81 (the
/// path-sourced live-load model).
const DISABLE_ONLY_FLOOR: (u64, u64, u64) = (1, 0, 81);

/// The `config` subcommand (list/read/set/remove) ships at 1.0.92.
const CONFIG_FLOOR: (u64, u64, u64) = (1, 0, 92);

/// One line per invocation, beside the state file, for the tests whose subject is which
/// calls ran rather than what the registry ended up holding.
const CALL_LOG_FILENAME: &str = "fake-copilot-calls.log";

/// When set (any non-empty value), `plugin enable` models real copilot's exit-0
/// "no plugin named ..." warning: nothing flips, so the backend's post-enable
/// re-read must fail loud.
const REFUSE_ENABLE_ENV: &str = "FAKE_COPILOT_REFUSE_ENABLE";

/// When set (any non-empty value), `plugin install` fails, so a test can leave the
/// half-install a failed first setup does: marketplace added, plugin listed
/// `(disabled)`.
const REFUSE_INSTALL_ENV: &str = "FAKE_COPILOT_REFUSE_INSTALL";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    log_call(&argv);
    match argv.as_slice() {
        ["--version"] | ["-v"] => {
            println!("GitHub Copilot CLI {}.", version_raw());
            ExitCode::SUCCESS
        }
        ["plugin", rest @ ..] => plugin(rest),
        ["config", rest @ ..] => config(rest),
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
        ["uninstall", id, ..] => uninstall(id),
        ["disable", id, ..] => disable(id),
        ["enable", id, ..] => enable(id),
        other => usage(other),
    }
}

fn marketplace(argv: &[&str]) -> ExitCode {
    match argv {
        ["list", ..] => {
            print!("{}", render_marketplaces(&State::load().marketplaces));
            ExitCode::SUCCESS
        }
        ["add", source, ..] => add_marketplace(source),
        // `marketplace remove <name>` drops the marketplace and every installed plugin
        // from it; the disable-only uninstall's stale `enabledPlugins` entry survives it,
        // exactly as the real >= 1.0.81 CLI behaves (`config --rm` clears it after).
        ["remove", name, ..] => marketplace_remove(name),
        other => usage(other),
    }
}

/// `copilot plugin marketplace add <source>`. Refuses an existing name with rc 1
/// (`Marketplace "…" already registered`), matching real copilot: a marketplace
/// must be removed before it can be re-pointed.
fn add_marketplace(source: &str) -> ExitCode {
    let entry = marketplace_entry(source);
    let mut state = State::load();
    let name = entry["name"].as_str().unwrap_or_default().to_string();
    if state.marketplaces.iter().any(|m| m["name"] == json!(name)) {
        eprintln!("Marketplace \"{name}\" already registered");
        return ExitCode::FAILURE;
    }
    state.marketplaces.push(entry);
    // Real 1.0.104 lists every plugin of a local marketplace as a `(disabled)`
    // row right after `marketplace add`, before any install (measured); `plugin
    // install` is what flips a row to enabled.
    for plugin in marketplace_plugin_rows(source) {
        state.plugins.retain(|p| p["id"] != plugin["id"]);
        state.plugins.push(plugin);
    }
    state.save();
    ExitCode::SUCCESS
}

fn config(argv: &[&str]) -> ExitCode {
    match argv {
        ["--rm", key, ..] => config_rm(key),
        ["enabledPlugins", ..] => config_get_enabled_plugins(),
        other => usage(other),
    }
}

/// `config enabledPlugins`: read the group as JSON, like the real CLI's group
/// read. No file is written (the backend gates `--rm` on this read's answer). An
/// absent key exits 1 with no output (measured 1.0.92, 1.0.104); a present-but-empty
/// `{}` exits 0 with `{}` (measured 1.0.104).
fn config_get_enabled_plugins() -> ExitCode {
    let state = State::load();
    if !state.enabled_plugins_present {
        return ExitCode::FAILURE;
    }
    let body = serde_json::to_string_pretty(&state.enabled_plugins).unwrap_or_else(|_| "{}".to_string());
    print!("{body}");
    ExitCode::SUCCESS
}

/// `plugin install <plugin>@<marketplace>`. Refuses when that marketplace is not
/// registered — the one ordering constraint the backend's `ensure_marketplace` step
/// exists to satisfy, so the double must not let a regression there pass.
/// `FAKE_COPILOT_REFUSE_INSTALL` models a failed install after the add.
fn install(id: &str) -> ExitCode {
    if !std::env::var(REFUSE_INSTALL_ENV).unwrap_or_default().is_empty() {
        eprintln!("Plugin install failed");
        return ExitCode::FAILURE;
    }
    let Some((_, wanted)) = id.split_once('@') else {
        eprintln!("fake-copilot: `{id}` is not <plugin>@<marketplace>");
        return ExitCode::FAILURE;
    };
    let mut state = State::load();
    if !state.marketplaces.iter().any(|m| m["name"] == json!(wanted)) {
        eprintln!("fake-copilot: marketplace `{wanted}` is not registered");
        return ExitCode::FAILURE;
    }
    match state.plugins.iter_mut().find(|p| p["id"] == json!(id)) {
        Some(existing) => {
            existing["disabled"] = json!(false);
            if let Some(version) = plugin_version() {
                existing["version"] = json!(version);
            }
        }
        None => {
            let mut entry = json!({"id": id, "disabled": false});
            if let Some(version) = plugin_version() {
                entry["version"] = json!(version);
            }
            state.plugins.push(entry);
        }
    }
    state.enabled_plugins[id] = json!(true);
    state.save();
    ExitCode::SUCCESS
}

fn uninstall(id: &str) -> ExitCode {
    if disable_only() && plugin_marketplace_is_local(id) {
        require_installed(id, |state| {
            for plugin in &mut state.plugins {
                if plugin["id"] == json!(id) {
                    plugin["disabled"] = json!(true);
                }
            }
            state.enabled_plugins[id] = json!(false);
        })
    } else {
        require_installed(id, |state| {
            state.plugins.retain(|p| p["id"] != json!(id));
            if let Some(enabled) = state.enabled_plugins.as_object_mut() {
                enabled.remove(id);
            }
        })
    }
}

/// Whether the plugin's marketplace is a local directory (path-sourced), the
/// kind whose uninstall disables instead of removes at >= 1.0.81.
fn plugin_marketplace_is_local(id: &str) -> bool {
    let Some((_, mkt)) = id.split_once('@') else {
        return false;
    };
    State::load().marketplaces.iter().any(|m| m["name"] == json!(mkt) && m["path"].is_string())
}

/// `copilot plugin disable <id>`: a deliberate per-plugin disable, the state the
/// backend must read back as `Disabled` and never re-enable on self_heal.
fn disable(id: &str) -> ExitCode {
    require_installed(id, |state| {
        for plugin in &mut state.plugins {
            if plugin["id"] == json!(id) {
                plugin["disabled"] = json!(true);
            }
        }
        state.enabled_plugins[id] = json!(false);
    })
}

/// `copilot plugin enable <id>`: the explicit-setup counterpart, flipping the row
/// back to enabled. `FAKE_COPILOT_REFUSE_ENABLE` models real copilot's exit-0
/// "no plugin named ..." warning: nothing flips, the row stays disabled.
fn enable(id: &str) -> ExitCode {
    if !std::env::var(REFUSE_ENABLE_ENV).unwrap_or_default().is_empty() {
        eprintln!("Warning: no plugin named \"{id}\" is currently configured, so no active plugin was enabled.");
        return ExitCode::SUCCESS;
    }
    require_installed(id, |state| {
        for plugin in &mut state.plugins {
            if plugin["id"] == json!(id) {
                plugin["disabled"] = json!(false);
            }
        }
        state.enabled_plugins[id] = json!(true);
    })
}

fn marketplace_remove(name: &str) -> ExitCode {
    mutate(|state| {
        state.marketplaces.retain(|m| m["name"] != json!(name));
        state.plugins.retain(|p| p["id"].as_str().and_then(|s| s.split_once('@')).map(|(_, mkt)| mkt != name).unwrap_or(true));
    })
}

fn config_rm(key: &str) -> ExitCode {
    if !has_config() {
        return usage(&["config", "--rm", key]);
    }
    let Some(id) = key.strip_prefix("enabledPlugins.") else {
        eprintln!("fake-copilot: unknown config key `{key}`");
        return ExitCode::FAILURE;
    };
    if id.contains('.') {
        eprintln!("Error: Cannot descend past record leaf at \"enabledPlugins.{}\".", id.split('.').next().unwrap_or(id));
        return ExitCode::FAILURE;
    }
    mutate(|state| {
        if let Some(enabled) = state.enabled_plugins.as_object_mut() {
            enabled.remove(id);
        }
    });
    write_settings_lock();
    ExitCode::SUCCESS
}

/// The real CLI's `config --rm` leaves a `settings.json.lock` in the config home
/// even when nothing was there to clear. The backend gates `--rm` on the group
/// read above, so a teardown that owns nothing must never reach this write.
fn write_settings_lock() {
    let path = copilot_home().join("settings.json.lock");
    let _ = fs::write(&path, "");
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

/// The version this double reports, honoring the `FAKE_COPILOT_VERSION` override.
fn version_raw() -> String {
    std::env::var(VERSION_ENV).unwrap_or_else(|_| DEFAULT_VERSION.to_string())
}

/// The optional plugin version this double reports in `plugin list` rows.
fn plugin_version() -> Option<String> {
    std::env::var(PLUGIN_VERSION_ENV).ok()
}

fn parsed_version() -> (u64, u64, u64) {
    let raw = version_raw();
    match parse_version(&raw) {
        Some(v) => v,
        None => {
            eprintln!("fake-copilot: cannot parse FAKE_COPILOT_VERSION={raw:?}");
            std::process::exit(2);
        }
    }
}

fn disable_only() -> bool {
    parsed_version() >= DISABLE_ONLY_FLOOR
}

fn has_config() -> bool {
    parsed_version() >= CONFIG_FLOOR
}

/// Parse a leading `MAJOR.MINOR.PATCH`; trailing non-digits on the patch are
/// tolerated (`1.0.104.`), matching the crate's tolerant parser.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch: String = parts.next()?.chars().take_while(char::is_ascii_digit).collect();
    let patch = patch.parse().ok()?;
    Some((major, minor, patch))
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

/// The plugins a local marketplace declares, as `(disabled)` rows — the shape real
/// 1.0.104 lists right after `marketplace add`, before any `plugin install` (measured).
fn marketplace_plugin_rows(source: &str) -> Vec<Value> {
    let dir = Path::new(source);
    if !dir.is_dir() {
        return Vec::new();
    }
    let name = local_marketplace_name(dir);
    let manifest =
        fs::read(dir.join(".claude-plugin").join("marketplace.json")).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let Some(plugins) = manifest.and_then(|doc| doc.get("plugins").cloned()).and_then(|p| p.as_array().cloned()) else {
        return Vec::new();
    };
    plugins
        .into_iter()
        .filter_map(|p| p.get("name").and_then(Value::as_str).map(str::to_string))
        .map(|plugin| json!({"id": format!("{plugin}@{name}"), "disabled": true}))
        .collect()
}

/// `copilot plugin list` output. No `(v…)` column: see the module doc. A disabled
/// row carries the `(disabled)` suffix the real >= 1.0.81 CLI appends after a
/// disable-only uninstall.
fn render_plugins(plugins: &[Value]) -> String {
    if plugins.is_empty() {
        return "No plugins installed.\n".to_string();
    }
    let mut out = String::from("Installed plugins:\n");
    for plugin in plugins {
        let Some(id) = plugin["id"].as_str() else { continue };
        let version = plugin["version"].as_str();
        let disabled = plugin["disabled"].as_bool().unwrap_or(false);
        match (version, disabled) {
            (Some(v), true) => out.push_str(&format!("  \u{2022} {id} (v{v}) (disabled)\n")),
            (Some(v), false) => out.push_str(&format!("  \u{2022} {id} (v{v})\n")),
            (None, true) => out.push_str(&format!("  \u{2022} {id} (disabled)\n")),
            (None, false) => out.push_str(&format!("  \u{2022} {id}\n")),
        }
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
    enabled_plugins: Value,
    /// Whether the loaded state file actually held an `enabledPlugins` key. Real
    /// copilot reads an absent key as rc 1 with no output and a present-but-empty
    /// `{}` as rc 0 with `{}` (measured 1.0.104), so the double models both.
    enabled_plugins_present: bool,
}

impl State {
    fn load() -> Self {
        let root: Value = fs::read(state_path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
        let array = |key: &str| root.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        let enabled_present = root.get("enabledPlugins").is_some();
        let enabled = root.get("enabledPlugins").cloned().unwrap_or_else(|| json!({}));
        Self {
            plugins: array("plugins"),
            marketplaces: array("marketplaces"),
            enabled_plugins: enabled,
            enabled_plugins_present: enabled_present,
        }
    }

    fn save(&self) {
        let path = state_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut root = json!({"plugins": self.plugins, "marketplaces": self.marketplaces});
        // `enabledPlugins` is written only once it holds an entry (or the loaded
        // file already carried the key): real `marketplace add` writes no such key
        // (measured 1.0.104), so the double must not invent one.
        if self.enabled_plugins_present || !self.enabled_plugins.as_object().is_none_or(|m| m.is_empty()) {
            root["enabledPlugins"] = self.enabled_plugins.clone();
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&root) {
            let _ = fs::write(&path, bytes);
        }
    }
}

fn state_path() -> PathBuf {
    copilot_home().join("fake-copilot-state.json")
}

fn copilot_home() -> PathBuf {
    std::env::var_os("COPILOT_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".copilot")))
        .unwrap_or_else(|| PathBuf::from(".copilot"))
}

/// Append this invocation to the call log, best-effort: a double that fails a lifecycle
/// call because its own log could not be written would red the test for the wrong
/// reason. The registry alone cannot answer "did the backend reinstall?" — an uninstall
/// + install of the same id leaves exactly the state that was there before.
fn log_call(argv: &[&str]) {
    let path = copilot_home().join(CALL_LOG_FILENAME);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(file, "{}", argv.join(" "));
}
