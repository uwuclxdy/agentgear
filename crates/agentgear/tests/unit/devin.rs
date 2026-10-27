//! devin backend unit tests: the plugin-prefixed, flattened `<plugin>-<stem>` dir
//! naming both surfaces and `remove`/`doctor` key on.

use std::collections::BTreeMap;

use super::{namespaced, render_doc};
use crate::components::MarkdownDoc;

fn doc(rel: &str) -> MarkdownDoc {
    MarkdownDoc { name: "nested".into(), rel: rel.into(), frontmatter: BTreeMap::new(), body: String::new(), raw: Vec::new() }
}

/// Each skill/subagent owns one depth-1 dir, so a nested doc must flatten into one
/// identifier segment rather than nest under its bare basename.
#[test]
fn namespaced_flattens_a_nested_doc() {
    assert_eq!(namespaced("ez-fixture-plugin", &doc("commands/sub/nested.md")), "ez-fixture-plugin-sub-nested");
    assert_eq!(namespaced("ez-fixture-plugin", &doc("agents/sub/nested.md")), "ez-fixture-plugin-sub-nested");
}

#[test]
fn render_doc_names_the_flattened_stem() {
    let rendered = render_doc("ez-fixture-plugin", &doc("commands/sub/nested.md"));
    assert!(rendered.contains("name: ez-fixture-plugin-sub-nested"), "rendered name must be the flattened stem:\n{rendered}");
}
