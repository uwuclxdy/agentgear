//! The closed agent-id registry, duplicated from the lib because a proc-macro
//! crate cannot import consts from the lib that depends on it. Three copies
//! exist (this list, the lib's `backend_for` arms, the lib's `__feature_check`
//! consts); `known_agents_match_the_lib` below pins them together so a new
//! backend cannot land in one and silently miss the others.

pub(crate) const KNOWN_AGENTS: &[&str] = &[
    "claude",
    "codex",
    "opencode",
    "gemini",
    "cursor",
    "cline",
    "devin",
    "qwen-code",
    "copilot-cli",
    "vscode-copilot",
    "jetbrains-copilot",
    "kimi",
    "kiro",
    "zed",
    "omp",
    "openclaw",
    "kilo",
    "antigravity",
    "antigravity-cli",
    "pi",
    "goose",
    "amp",
    "crush",
    "droid",
    "augment",
];

/// The lib's `__feature_check` const ident for an agent id
/// (`copilot-cli` -> `COPILOT_CLI`).
pub(crate) fn feature_const_ident(id: &str) -> String {
    id.replace('-', "_").to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib_src(rel: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../agentgear/src").join(rel);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {} ({e}); tests only run in the workspace", path.display()))
    }

    /// The first double-quoted token on the line, if the line starts with one.
    fn leading_quoted(line: &str) -> Option<&str> {
        line.trim_start().strip_prefix('"')?.split('"').next()
    }

    /// The three copies of the id list (derive `KNOWN_AGENTS`, lib `backend_for`
    /// arms, lib `__feature_check` consts) must never diverge: a backend landing
    /// in the registry but not here would be rejected as a typo, and one missing
    /// a feature const would fail every host's expansion.
    #[test]
    fn known_agents_match_the_lib() {
        let mut known: Vec<&str> = KNOWN_AGENTS.to_vec();
        known.sort_unstable();

        let registry = lib_src("agents/mod.rs");
        let mut arms: Vec<&str> = registry.lines().filter(|line| line.contains("=> Some(Box::new(")).filter_map(leading_quoted).collect();
        arms.sort_unstable();
        assert!(!arms.is_empty(), "no `backend_for` arms found; the extraction pattern is stale");
        assert_eq!(arms, known, "derive KNOWN_AGENTS vs lib backend_for arms");

        let feature_check = lib_src("__feature_check.rs");
        let mut consts: Vec<(String, String)> = feature_check
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('"') && line.contains("=>"))
            .filter_map(|line| {
                let feature = leading_quoted(line)?;
                let ident = line.split("=>").nth(1)?.trim().trim_end_matches(',');
                Some((feature.to_string(), ident.to_string()))
            })
            .collect();
        consts.sort_unstable();
        let mut expected: Vec<(String, String)> = KNOWN_AGENTS.iter().map(|id| (id.to_string(), feature_const_ident(id))).collect();
        expected.sort_unstable();
        assert_eq!(consts, expected, "derive KNOWN_AGENTS vs lib __feature_check consts");
    }
}
