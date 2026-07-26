//! confedit tests: json_edit round-trips, preserves unknown keys, reports changed
//! vs unchanged, never creates an empty file, and BOM-free with a trailing newline;
//! write_file_idem + remove_file_idem idempotency; and the removal pairs' two rules —
//! only a container OUR removal emptied goes, never one the user had empty, and a
//! root left holding nothing takes the file — in all three editors. Explicit temp
//! paths, no env.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::{json_edit, json_obj_at, json_prune_at, json_prune_obj, json_remove, remove_file_idem, write_file_idem};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ez-confedit-{:016x}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn json_edit_creates_preserves_and_detects_change() {
    let path = scratch("cfg.json");
    // Seed an existing config with an unrelated key that must survive.
    std::fs::write(&path, br#"{"userKey":123,"mcpServers":{"theirs":{"command":"x"}}}"#).unwrap();

    let changed = json_edit(&path, |root| {
        json_obj_at(root, &["mcpServers"]).insert("ours".into(), json!({"command":"host"}));
        Ok(())
    })
    .unwrap();
    assert!(changed, "adding a server must report changed");

    let bytes = std::fs::read(&path).unwrap();
    let back: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back["userKey"], json!(123), "unknown top-level key survived");
    assert_eq!(back["mcpServers"]["theirs"]["command"], json!("x"), "user's server survived");
    assert_eq!(back["mcpServers"]["ours"]["command"], json!("host"));
    assert_ne!(bytes.first(), Some(&0xEF), "no UTF-8 BOM");
    assert_eq!(bytes.last(), Some(&b'\n'), "trailing newline");

    // A second identical edit is a byte-for-byte no-op.
    let before = std::fs::read(&path).unwrap();
    let changed = json_edit(&path, |root| {
        json_obj_at(root, &["mcpServers"]).insert("ours".into(), json!({"command":"host"}));
        Ok(())
    })
    .unwrap();
    assert!(!changed, "re-inserting the same value must report unchanged");
    assert_eq!(std::fs::read(&path).unwrap(), before, "unchanged edit must not rewrite");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn json_edit_missing_file_semantics() {
    let created = scratch("new.json");
    let changed = json_edit(&created, |root| {
        json_obj_at(root, &["mcp"]).insert("s".into(), json!({"command":"c"}));
        Ok(())
    })
    .unwrap();
    assert!(changed);
    assert!(created.exists());

    // A no-op edit on a missing file creates nothing.
    let untouched = scratch("noop.json");
    let changed = json_edit(&untouched, |_root| Ok(())).unwrap();
    assert!(!changed);
    assert!(!untouched.exists(), "a no-op edit must not create an empty file");

    std::fs::remove_dir_all(created.parent().unwrap()).ok();
    std::fs::remove_dir_all(untouched.parent().unwrap()).ok();
}

#[test]
fn json_edit_treats_empty_file_as_object() {
    // A touched / interrupted-write (empty or whitespace-only) config is not a parse
    // error; it starts from `{}` so an install still lands.
    let path = scratch("empty.json");
    std::fs::write(&path, b"   \n\t").unwrap();
    let changed = json_edit(&path, |root| {
        json_obj_at(root, &["mcpServers"]).insert("ours".into(), json!({"command":"host"}));
        Ok(())
    })
    .unwrap();
    assert!(changed);
    let back: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(back["mcpServers"]["ours"]["command"], json!("host"));
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

/// Drop `key` from the container at `path`, the shape every removal path has.
fn prune_drop(root: &mut Value, path: &[&str], key: &str) -> bool {
    json_prune_obj(root, path, |obj| {
        obj.remove(key);
        Ok(())
    })
    .unwrap()
}

#[test]
fn json_prune_drops_only_a_container_our_removal_emptied() {
    // Ours was the last key: the container we created goes with it.
    let mut root = json!({"theme":"dark","mcpServers":{"ours":{"command":"host"}}});
    assert!(prune_drop(&mut root, &["mcpServers"], "ours"));
    assert_eq!(root, json!({"theme":"dark"}), "the container our own key emptied must go");

    // A user key survives ours, so the container stays — with exactly theirs in it.
    let mut root = json!({"mcpServers":{"ours":{"command":"host"},"theirs":{"command":"x"}}});
    assert!(!prune_drop(&mut root, &["mcpServers"], "ours"));
    assert_eq!(root, json!({"mcpServers":{"theirs":{"command":"x"}}}), "a container still holding theirs must stay");

    // The user's own empty container: our removal takes nothing, so it is not ours to
    // prune. This is the whole difference between "we emptied it" and "it is empty".
    let mut root = json!({"theme":"dark","ui":{}});
    assert!(!prune_drop(&mut root, &["ui"], "statusLine"));
    assert_eq!(root, json!({"theme":"dark","ui":{}}), "a container the user had empty must survive untouched");

    // A missing container is navigated, never created.
    let mut root = json!({"theme":"dark"});
    assert!(!prune_drop(&mut root, &["ui"], "statusLine"));
    assert_eq!(root, json!({"theme":"dark"}), "a missing container must not be created by a removal");

    // Nested: dropping the leaf container takes the ancestor it emptied with it, and
    // stops at the level still holding something of the user's.
    let mut root = json!({"a":{"b":{"ours":1}}});
    assert!(prune_drop(&mut root, &["a", "b"], "ours"));
    assert_eq!(root, json!({}), "an ancestor emptied by the level we dropped must go too");
    let mut root = json!({"a":{"keep":1,"b":{"ours":1}}});
    assert!(prune_drop(&mut root, &["a", "b"], "ours"));
    assert_eq!(root, json!({"a":{"keep":1}}), "an ancestor still holding a user key must stay");

    // An emptied ARRAY is the same class (opencode's `instructions[]`).
    let mut root = json!({"instructions":["/ours.md"]});
    let pruned = json_prune_at(&mut root, &["instructions"], |list| {
        list.as_array_mut().unwrap().retain(|e| e != "/ours.md");
        Ok(())
    })
    .unwrap();
    assert!(pruned);
    assert_eq!(root, json!({}), "an array key our entry emptied must go");
}

/// The YAML twin of `json_prune_drops_only_a_container_our_removal_emptied`, one
/// level deep (goose's `extensions`, the only YAML container any backend creates).
#[cfg(feature = "goose")]
#[test]
fn yaml_prune_drops_only_a_mapping_our_removal_emptied() {
    use serde_norway::Value as Yaml;

    use super::yaml_prune_map;

    fn parse(text: &str) -> Yaml {
        serde_norway::from_str(text).unwrap()
    }
    fn drop_key(root: &mut Yaml, container: &str, key: &str) -> bool {
        yaml_prune_map(root, container, |map| {
            map.remove(key);
            Ok(())
        })
        .unwrap()
    }

    // Ours was the last extension: the mapping our `ext_map` created goes with it.
    let mut root = parse("GOOSE_MODEL: gpt-x\nextensions:\n  ours:\n    cmd: host\n");
    assert!(drop_key(&mut root, "extensions", "ours"));
    assert_eq!(root, parse("GOOSE_MODEL: gpt-x\n"), "the mapping our own key emptied must go");

    // A user extension survives ours, so the mapping stays — with exactly theirs in it.
    let mut root = parse("extensions:\n  ours:\n    cmd: host\n  theirs:\n    cmd: x\n");
    assert!(!drop_key(&mut root, "extensions", "ours"));
    assert_eq!(root, parse("extensions:\n  theirs:\n    cmd: x\n"), "a mapping still holding theirs must stay");

    // The user's own empty mapping: our removal takes nothing, so it is not ours to
    // prune. This is the whole difference between "we emptied it" and "it is empty".
    let mut root = parse("GOOSE_MODEL: gpt-x\nextensions: {}\n");
    assert!(!drop_key(&mut root, "extensions", "ours"));
    assert_eq!(root, parse("GOOSE_MODEL: gpt-x\nextensions: {}\n"), "a mapping the user had empty must survive untouched");

    // A missing mapping is navigated, never created.
    let mut root = parse("GOOSE_MODEL: gpt-x\n");
    assert!(!drop_key(&mut root, "extensions", "ours"));
    assert_eq!(root, parse("GOOSE_MODEL: gpt-x\n"), "a missing mapping must not be created by a removal");

    // A key holding a non-mapping is left exactly as the user wrote it.
    let mut root = parse("extensions: mine\n");
    assert!(!drop_key(&mut root, "extensions", "ours"));
    assert_eq!(root, parse("extensions: mine\n"), "a non-mapping value must not be touched");
}

#[test]
fn json_remove_drops_a_file_left_holding_nothing() {
    // Everything in it was ours, so the file is ours to take back.
    let ours = scratch("ours.json");
    std::fs::write(&ours, br#"{"mcpServers":{"ours":{"command":"host"}}}"#).unwrap();
    let changed = json_remove(&ours, |root| {
        prune_drop(root, &["mcpServers"], "ours");
        Ok(())
    })
    .unwrap();
    assert!(changed);
    assert!(!ours.exists(), "a root emptied by our own removal must take the file with it");

    // One user key left: the file is rewritten, never dropped.
    let shared = scratch("shared.json");
    std::fs::write(&shared, br#"{"theme":"dark","mcpServers":{"ours":{"command":"host"}}}"#).unwrap();
    let changed = json_remove(&shared, |root| {
        prune_drop(root, &["mcpServers"], "ours");
        Ok(())
    })
    .unwrap();
    assert!(changed);
    let back: Value = serde_json::from_slice(&std::fs::read(&shared).unwrap()).unwrap();
    assert_eq!(back, json!({"theme":"dark"}), "a root still holding a user key must survive as a file");

    // A teardown with nothing of ours to undo writes nothing — including against a
    // file the user deliberately keeps empty, and against no file at all.
    let empty = scratch("empty.json");
    std::fs::write(&empty, b"{}").unwrap();
    assert!(!json_remove(&empty, |_root| Ok(())).unwrap());
    assert_eq!(std::fs::read(&empty).unwrap(), b"{}", "a no-op teardown must not touch a user's empty config");
    let missing = scratch("missing.json");
    assert!(!json_remove(&missing, |_root| Ok(())).unwrap());
    assert!(!missing.exists(), "a no-op teardown must not create a file");

    // The inverse control: the very same emptying edit through `json_edit` keeps the
    // file. Dropping one is the removal pair's job alone, so no install path can.
    let install_side = scratch("install.json");
    std::fs::write(&install_side, br#"{"mcpServers":{"ours":{"command":"host"}}}"#).unwrap();
    let changed = json_edit(&install_side, |root| {
        prune_drop(root, &["mcpServers"], "ours");
        Ok(())
    })
    .unwrap();
    assert!(changed);
    let back: Value = serde_json::from_slice(&std::fs::read(&install_side).unwrap()).unwrap();
    assert_eq!(back, json!({}), "`json_edit` must never delete a file, however empty the edit leaves it");

    for p in [&ours, &shared, &empty, &missing, &install_side] {
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }
}

#[test]
fn write_and_remove_file_idem() {
    let path = scratch("f.txt");
    assert!(write_file_idem(&path, b"one").unwrap());
    assert!(!write_file_idem(&path, b"one").unwrap(), "same bytes must not rewrite");
    assert!(write_file_idem(&path, b"two").unwrap(), "changed bytes must rewrite");
    assert!(remove_file_idem(&path).unwrap(), "present file removal reports true");
    assert!(!remove_file_idem(&path).unwrap(), "absent file removal reports false");

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

/// The TOML twin of `json_prune_drops_only_a_container_our_removal_emptied`, one
/// level deep over both container spellings a TOML backend creates: codex's
/// `[mcp_servers]` table and kimi's `[[hooks]]` array of tables.
///
/// The whole-document `assert_eq!`s here pin `toml_edit`'s DECOR as well as our own
/// logic — the leading blank line below is the removed table's, not something we
/// chose, and comment placement is its call too. A `toml_edit` bump can therefore red
/// these with no regression behind it; check the diff is whitespace or comment
/// position before hunting for a bug. Kept whole rather than loosened on purpose: the
/// delete gate reads key counts and never decor, so a decor shift can only ever
/// surface here, loudly, instead of somewhere it changes a file's fate silently.
#[cfg(any(feature = "codex", feature = "kimi"))]
#[test]
fn toml_prune_drops_only_a_container_our_removal_emptied() {
    use toml_edit::{DocumentMut, Item};

    use super::toml_prune;

    fn drop_server(doc: &mut DocumentMut, name: &str) -> bool {
        toml_prune(doc, "mcp_servers", |item| {
            if let Some(table) = item.as_table_mut() {
                table.remove(name);
            }
            Ok(())
        })
        .unwrap()
    }

    // Ours was the last server: the implicit table `mcp_table` created goes with it.
    let mut doc: DocumentMut = "model = \"gpt-5.4\"\n\n[mcp_servers.ours]\ncommand = \"host\"\n".parse().unwrap();
    assert!(drop_server(&mut doc, "ours"));
    assert!(doc.get("mcp_servers").is_none(), "the container our own key emptied must go:\n{doc}");
    assert_eq!(doc.to_string(), "model = \"gpt-5.4\"\n", "the user's own key must survive untouched");

    // A user server survives ours, so the table stays — with exactly theirs in it.
    // Decor pin: the leading blank line is what ours carried as its own prefix, left
    // behind by the removal. `toml_edit`'s to keep, not ours.
    let mut doc: DocumentMut = "[mcp_servers.ours]\ncommand = \"host\"\n\n[mcp_servers.theirs]\ncommand = \"x\"\n".parse().unwrap();
    assert!(!drop_server(&mut doc, "ours"));
    assert_eq!(doc.to_string(), "\n[mcp_servers.theirs]\ncommand = \"x\"\n", "a table still holding theirs must stay");

    // An empty table we never installed into: our removal takes nothing, so it is not
    // ours to prune. This is the whole difference between "we emptied it" and "it is
    // empty". It does NOT extend to the real install flow — a table the user had empty
    // and we then filled IS pruned when ours come back out, since nothing on disk
    // records who created it. That is the accepted cost `docs/harness/foundation.md`
    // §0 names, identical to the JSON side's.
    let mut doc: DocumentMut = "# mine\n[mcp_servers]\n".parse().unwrap();
    assert!(!drop_server(&mut doc, "ours"));
    assert_eq!(doc.to_string(), "# mine\n[mcp_servers]\n", "a table we never wrote into must survive untouched");

    // A missing container is navigated, never created.
    let mut doc: DocumentMut = "model = \"gpt-5.4\"\n".parse().unwrap();
    assert!(!drop_server(&mut doc, "ours"));
    assert_eq!(doc.to_string(), "model = \"gpt-5.4\"\n", "a missing container must not be created by a removal");

    // A key holding a non-container is left exactly as the user wrote it.
    let mut doc: DocumentMut = "mcp_servers = \"mine\"\n".parse().unwrap();
    assert!(!drop_server(&mut doc, "ours"));
    assert_eq!(doc.to_string(), "mcp_servers = \"mine\"\n", "a non-table value must not be touched");

    // kimi's array-of-tables is the same class: emptied by our own retain, so it goes.
    let mut doc: DocumentMut = "[[hooks]]\nevent = \"Stop\"\ncommand = \"ours\"\n".parse().unwrap();
    let pruned = toml_prune(&mut doc, "hooks", |item| {
        if let Some(arr) = item.as_array_of_tables_mut() {
            arr.retain(|t| t.get("command").and_then(Item::as_str) != Some("ours"));
        }
        Ok(())
    })
    .unwrap();
    assert!(pruned);
    assert!(doc.as_table().is_empty(), "an array-of-tables our entry emptied must go:\n{doc}");
}

/// `toml_remove`'s file arm, both directions, plus the four shapes that each rule out
/// one plausible-but-wrong way to write it:
///
/// - codex's `[mcp_servers]` and kimi's `[[hooks]]` are implicit, so an emptied one
///   renders to zero bytes while still keying the root — the reported symptom. Rules
///   out "count root keys without pruning first".
/// - a `[their_section]` header the user keeps empty holds no leaf value anywhere, yet
///   the file is theirs. Rules out a structural "no value survives anywhere" scan.
/// - a TRAILING comment leaves a non-empty render over an empty root. Rules out
///   "delete when the render came out blank".
/// - a CRLF or BOM comment-only config does not round-trip through `toml_edit`'s own
///   renderer. Rules out "delete when the text changed", which would take a file we
///   never wrote a byte to.
#[cfg(any(feature = "codex", feature = "kimi"))]
#[test]
fn toml_remove_drops_a_file_left_holding_nothing() {
    use toml_edit::{DocumentMut, Item};

    use super::{toml_edit as toml_edit_fn, toml_prune, toml_remove};

    fn drop_ours(doc: &mut DocumentMut) -> Result<bool, crate::error::Error> {
        toml_prune(doc, "mcp_servers", |item| {
            if let Some(table) = item.as_table_mut() {
                table.remove("ours");
            }
            Ok(())
        })
    }

    // A comment plus exactly one entry of ours and nothing else: everything in the
    // file was ours, so the file is ours to take back. The comment goes with it —
    // our removal having emptied every key means it was already orphaned.
    let ours = scratch("ours.toml");
    std::fs::write(&ours, "# my codex config\n[mcp_servers.ours]\ncommand = \"host\"\n").unwrap();
    assert!(toml_remove(&ours, drop_ours).unwrap());
    assert!(!ours.exists(), "a root emptied by our own removal must take the file with it");

    // The implicit-container trap, stated directly: the emptied table renders to zero
    // bytes, so a file that survived here would be the reported symptom.
    let mut doc: DocumentMut = "[mcp_servers.ours]\ncommand = \"host\"\n".parse().unwrap();
    doc.as_table_mut().get_mut("mcp_servers").and_then(Item::as_table_mut).unwrap().remove("ours");
    assert!(doc.to_string().is_empty(), "premise: an emptied implicit table renders to nothing");
    assert!(!doc.as_table().is_empty(), "premise: it still keys the root, so an unpruned root reads as non-empty");

    // One user key left: the file is rewritten, never dropped — comment and all,
    // since the TOML editor is the comment-preserving one.
    let shared = scratch("shared.toml");
    let seed = "# the user's own codex config\nmodel = \"gpt-5.4\"\n\n[mcp_servers.ours]\ncommand = \"host\"\n";
    std::fs::write(&shared, seed).unwrap();
    assert!(toml_remove(&shared, drop_ours).unwrap());
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "# the user's own codex config\nmodel = \"gpt-5.4\"\n",
        "a root still holding a user key must survive as a file, comment intact"
    );

    // A foreign server beside ours: the table survives too, so nothing empties, and
    // the comment the user wrote above their own server survives with it. Decor pin:
    // that survival is `toml_edit` keeping the comment attached to THEIR header —
    // ordering ours after theirs is what makes it observable, since a comment directly
    // above ours would go out with ours.
    let foreign = scratch("foreign.toml");
    let seed = "# theirs\n[mcp_servers.theirs]\ncommand = \"x\"\n\n[mcp_servers.ours]\ncommand = \"host\"\n";
    std::fs::write(&foreign, seed).unwrap();
    assert!(toml_remove(&foreign, drop_ours).unwrap());
    assert_eq!(
        std::fs::read_to_string(&foreign).unwrap(),
        "# theirs\n[mcp_servers.theirs]\ncommand = \"x\"\n",
        "a table still holding a foreign server must survive as a file, comment intact"
    );

    // A section header the user keeps empty carries no value anywhere, yet the file is
    // theirs. This is why the root test counts keys instead of scanning for a leaf.
    let valueless = scratch("valueless.toml");
    std::fs::write(&valueless, "[their_section]\n\n[mcp_servers.ours]\ncommand = \"host\"\n").unwrap();
    assert!(toml_remove(&valueless, drop_ours).unwrap());
    assert_eq!(
        std::fs::read_to_string(&valueless).unwrap(),
        "[their_section]\n",
        "a file whose only survivor is an empty user section must not be taken"
    );

    // A TRAILING comment: the delete has to fire even though the render is NOT empty.
    // Every case above seeds a LEADING comment, which `toml_edit` carries as the
    // removed table's own prefix decor, so their render is already empty by the time
    // the file arm runs — a "delete when the render came out blank" rule would pass
    // every one of them and silently skip this. The comment goes; our own removal
    // having emptied every key means it was already orphaned.
    let trailing = scratch("trailing.toml");
    std::fs::write(&trailing, "[mcp_servers.ours]\ncommand = \"host\"\n\n# TODO revisit\n").unwrap();
    assert!(toml_remove(&trailing, drop_ours).unwrap());
    assert!(!trailing.exists(), "the delete must fire on an emptied root whose leftover decor still renders");

    // A teardown with nothing of ours to undo writes nothing — including against the
    // comment-only file whose root is already empty, the 0-byte residue an older
    // build left behind, and no file at all.
    let comment_only = scratch("comment.toml");
    std::fs::write(&comment_only, "# only a comment\n").unwrap();
    assert!(!toml_remove(&comment_only, drop_ours).unwrap());
    assert_eq!(
        std::fs::read_to_string(&comment_only).unwrap(),
        "# only a comment\n",
        "a comment-only config must survive a no-op teardown"
    );

    // The same file as the user's editor actually saves it. `toml_edit` normalizes
    // CRLF decor to LF and strips a BOM, so the document does not round-trip through
    // its own renderer — a file arm keyed on "the text changed" would read both of
    // these as ours and delete a config we never wrote a byte to. Keyed on the prune,
    // neither is touched at all.
    for (label, seed) in [("crlf", "# my config\r\n".as_bytes()), ("bom", "\u{feff}# my config\n".as_bytes())] {
        let path = scratch("windows.toml");
        std::fs::write(&path, seed).unwrap();
        assert!(!toml_remove(&path, drop_ours).unwrap(), "a {label} comment-only config must report no change");
        assert_eq!(std::fs::read(&path).unwrap(), seed, "a {label} comment-only config must survive a teardown byte-for-byte");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    let residue = scratch("residue.toml");
    std::fs::write(&residue, "").unwrap();
    assert!(!toml_remove(&residue, drop_ours).unwrap());
    assert!(residue.exists(), "a file the user is already keeping empty is not ours to take");
    let missing = scratch("missing.toml");
    assert!(!toml_remove(&missing, drop_ours).unwrap());
    assert!(!missing.exists(), "a no-op teardown must not create a file");

    // The inverse control: the very same emptying edit through `toml_edit` keeps the
    // file. Dropping one is the removal pair's job alone, so no install path can.
    let install_side = scratch("install.toml");
    std::fs::write(&install_side, "[mcp_servers.ours]\ncommand = \"host\"\n").unwrap();
    assert!(toml_edit_fn(&install_side, |doc| drop_ours(doc).map(|_| ())).unwrap());
    assert_eq!(
        std::fs::read_to_string(&install_side).unwrap(),
        "",
        "`toml_edit` must never delete a file, however empty the edit leaves it"
    );

    for p in [&ours, &shared, &foreign, &valueless, &trailing, &comment_only, &residue, &missing, &install_side] {
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }
}

/// `yaml_remove`'s file arm, both directions. The YAML editor is not comment
/// preserving, so a surviving file keeps keys and loses comments (pre-existing
/// `yaml_edit` behavior); what this pins is which files survive at all.
#[cfg(feature = "goose")]
#[test]
fn yaml_remove_drops_a_file_left_holding_nothing() {
    use serde_norway::Value as Yaml;

    use super::{yaml_edit, yaml_prune_map, yaml_remove};

    fn drop_ours(root: &mut Yaml) -> Result<(), crate::error::Error> {
        yaml_prune_map(root, "extensions", |exts| {
            exts.remove("ours");
            Ok(())
        })
        .map(|_| ())
    }

    // A comment plus exactly one extension of ours and nothing else: the mapping goes,
    // then the root it emptied takes the file. The comment goes with it — our removal
    // having taken every key means it was already orphaned.
    let ours = scratch("ours.yaml");
    std::fs::write(&ours, "# my goose config\nextensions:\n  ours:\n    cmd: host\n").unwrap();
    assert!(yaml_remove(&ours, drop_ours).unwrap());
    assert!(!ours.exists(), "a root emptied by our own removal must take the file with it");

    // One user key left: the file is rewritten, never dropped.
    let shared = scratch("shared.yaml");
    std::fs::write(&shared, "GOOSE_MODEL: gpt-x\nextensions:\n  ours:\n    cmd: host\n").unwrap();
    assert!(yaml_remove(&shared, drop_ours).unwrap());
    assert_eq!(std::fs::read_to_string(&shared).unwrap(), "GOOSE_MODEL: gpt-x\n", "a root still holding a user key must survive as a file");

    // A foreign extension beside ours: the mapping survives too, so nothing empties.
    let foreign = scratch("foreign.yaml");
    std::fs::write(&foreign, "extensions:\n  ours:\n    cmd: host\n  theirs:\n    cmd: x\n").unwrap();
    assert!(yaml_remove(&foreign, drop_ours).unwrap());
    let back: Yaml = serde_norway::from_slice(&std::fs::read(&foreign).unwrap()).unwrap();
    assert_eq!(back, serde_norway::from_str::<Yaml>("extensions:\n  theirs:\n    cmd: x\n").unwrap(), "the foreign extension must survive");

    // A teardown with nothing of ours to undo writes nothing — including against the
    // comment-only file, the empty-map residue an older build left behind, a mapping
    // the user is keeping empty, and no file at all.
    let comment_only = scratch("comment.yaml");
    std::fs::write(&comment_only, "# only a comment\n").unwrap();
    assert!(!yaml_remove(&comment_only, drop_ours).unwrap());
    assert_eq!(
        std::fs::read_to_string(&comment_only).unwrap(),
        "# only a comment\n",
        "a comment-only config must survive a no-op teardown"
    );
    let residue = scratch("residue.yaml");
    std::fs::write(&residue, "{}\n").unwrap();
    assert!(!yaml_remove(&residue, drop_ours).unwrap());
    assert_eq!(std::fs::read_to_string(&residue).unwrap(), "{}\n", "a file the user is already keeping empty is not ours to take");
    let user_empty = scratch("user-empty.yaml");
    std::fs::write(&user_empty, "# mine\nextensions: {}\n").unwrap();
    assert!(!yaml_remove(&user_empty, drop_ours).unwrap());
    assert_eq!(
        std::fs::read_to_string(&user_empty).unwrap(),
        "# mine\nextensions: {}\n",
        "a mapping the user had empty must survive untouched"
    );
    let missing = scratch("missing.yaml");
    assert!(!yaml_remove(&missing, drop_ours).unwrap());
    assert!(!missing.exists(), "a no-op teardown must not create a file");

    // The inverse control: the very same emptying edit through `yaml_edit` keeps the
    // file. Dropping one is the removal pair's job alone, so no install path can.
    let install_side = scratch("install.yaml");
    std::fs::write(&install_side, "extensions:\n  ours:\n    cmd: host\n").unwrap();
    assert!(yaml_edit(&install_side, drop_ours).unwrap());
    assert!(install_side.exists(), "`yaml_edit` must never delete a file, however empty the edit leaves it");

    for p in [&ours, &shared, &foreign, &comment_only, &residue, &user_empty, &missing, &install_side] {
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }
}
