//! opencode backend unit tests: the plugin-prefixed, flattened filename its
//! `commands/`/`agents/` scan resolves.

use std::collections::BTreeMap;
use std::path::Path;

use super::doc_path;
use crate::components::MarkdownDoc;

fn doc(rel: &str) -> MarkdownDoc {
    MarkdownDoc { name: "nested".into(), rel: rel.into(), frontmatter: BTreeMap::new(), body: String::new(), raw: Vec::new() }
}

/// A nested doc lands as one flattened, plugin-prefixed file rather than under its
/// bare basename.
#[test]
fn doc_path_flattens_a_nested_doc() {
    assert_eq!(
        doc_path(Path::new("base"), "commands", "ez-fixture-plugin", &doc("commands/sub/nested.md")),
        Path::new("base/commands/ez-fixture-plugin-sub-nested.md")
    );
    assert_eq!(
        doc_path(Path::new("base"), "agents", "ez-fixture-plugin", &doc("agents/sub/nested.md")),
        Path::new("base/agents/ez-fixture-plugin-sub-nested.md")
    );
}
