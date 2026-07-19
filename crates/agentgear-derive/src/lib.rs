//! `#[derive(PluginHost)]` + `#[plugin(..)]`. At expansion the macro reads the
//! shipped `plugin.json` (existence, JSON validity, and a name cross-check against
//! the `name` attr), bakes the compressed tree in via `include_bytes!` of the blob
//! the build.rs produced (`AGENTGEAR_BLOB`), implements the `PluginHost` trait from
//! the attrs, and emits a const-panic guard that fires if the host forgot its
//! `build.rs` (design §7, §10). `embed = false` bakes nothing (an empty blob).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::Path;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{DeriveInput, Expr, ExprLit, Lit, MetaNameValue, Token, parse_macro_input};

mod known_agents;
use known_agents::{KNOWN_AGENTS, feature_const_ident};

#[proc_macro_derive(PluginHost, attributes(plugin))]
pub fn derive_plugin_host(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input).unwrap_or_else(|e| e.to_compile_error()).into()
}

struct Attrs {
    name: String,
    marketplace: String,
    version: TokenStream2,
    tree: String,
    default_source: String,
    github_repo: Option<String>,
    agents: Vec<String>,
    embed: bool,
    /// A path to a `fn() -> Option<String>` the emitted impl calls from
    /// `PluginHost::instructions` (the only override seam, since the derive owns the
    /// sole impl block). Spliced verbatim like `version`; `None` inherits the trait
    /// default (`None`, no instructions surface).
    instructions_fn: Option<TokenStream2>,
    span: proc_macro2::Span,
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;
    let attrs = parse_attrs(&input)?;

    verify_plugin_json(&attrs)?;

    let name = &attrs.name;
    let marketplace = &attrs.marketplace;
    let version = &attrs.version;
    let agents = &attrs.agents;
    let default_source = default_source_tokens(&attrs)?;

    // Override `PluginHost::instructions` only when the host set `instructions_fn`;
    // otherwise the emitted impl omits the method and inherits the trait default
    // (`None`). The attr is a path to a `fn() -> Option<String>`, called verbatim.
    let instructions_method = match &attrs.instructions_fn {
        Some(path) => quote! {
            fn instructions() -> ::core::option::Option<::std::string::String> {
                #path()
            }
        },
        None => quote! {},
    };

    // `embed = true` (default): bake the build.rs blob via `include_bytes!` of the
    // `AGENTGEAR_BLOB` path. `embed = false`: an empty slice (`Source::Embedded`
    // then errors at materialize). Pairs with the lib's `embed` feature: turning
    // that off but leaving this on makes `env!("AGENTGEAR_BLOB")` a compile error.
    let embedded_blob_body = if attrs.embed {
        quote! { ::core::include_bytes!(::core::env!("AGENTGEAR_BLOB")) }
    } else {
        quote! { &[] }
    };

    // One const-eval check per listed agent: `agents = ["codex"]` with the
    // `codex` cargo feature off fails HERE (host compile) with the fix, instead
    // of on the end user's machine at first `setup`. Same const-panic trick as
    // the AGENTGEAR_GUARD block below; the bools live in the lib because a
    // panicking unit const would fire eagerly in agentgear's own build.
    let feature_checks = agents.iter().map(|id| {
        let const_ident = format_ident!("{}", feature_const_ident(id));
        let msg = format!(
            "agentgear: agent `{id}` is listed in #[plugin(agents = [...])] but its `{id}` cargo feature is off; add `features = [\"{id}\"]` to your agentgear dependency"
        );
        quote! {
            const _: () = {
                if !::agentgear::__feature_check::#const_ident {
                    ::core::panic!(#msg);
                }
            };
        }
    });

    Ok(quote! {
        impl ::agentgear::PluginHost for #ident {
            const NAME: &'static str = #name;
            const MARKETPLACE: &'static str = #marketplace;
            const VERSION: &'static str = #version;
            const DEFAULT_SOURCE: ::agentgear::Source = #default_source;
            const AGENTS: &'static [&'static str] = &[#(#agents),*];

            fn embedded_blob() -> &'static [u8] {
                #embedded_blob_body
            }

            #instructions_method
        }

        // Fires only if the host forgot its build.rs, which would also lose the
        // version guard and the include_dir rebuild tracking. A compile error with
        // the fix beats a silent no-op.
        const _: () = {
            if ::core::option_env!("AGENTGEAR_GUARD").is_none() {
                ::core::panic!(
                    "agentgear: host is missing its build.rs calling agentgear::build::assert_plugin_version() (see the crate docs)"
                );
            }
        };

        #(#feature_checks)*
    })
}

fn parse_attrs(input: &DeriveInput) -> syn::Result<Attrs> {
    let span = input.ident.span();
    let mut name: Option<String> = None;
    let mut marketplace: Option<String> = None;
    let mut version: Option<TokenStream2> = None;
    let mut tree: Option<String> = None;
    let mut default_source: Option<String> = None;
    let mut github_repo: Option<String> = None;
    let mut agents: Option<Vec<String>> = None;
    let mut embed: Option<bool> = None;
    let mut instructions_fn: Option<TokenStream2> = None;

    let attr = input
        .attrs
        .iter()
        .find(|a| a.path().is_ident("plugin"))
        .ok_or_else(|| syn::Error::new(span, "missing `#[plugin(name = \"...\", ...)]` attribute"))?;

    let pairs = attr.parse_args_with(Punctuated::<MetaNameValue, Token![,]>::parse_terminated)?;
    for pair in pairs {
        let key = pair.path.get_ident().map(ToString::to_string).unwrap_or_default();
        match key.as_str() {
            "name" => name = Some(lit_str(&pair.value)?),
            "marketplace" => marketplace = Some(lit_str(&pair.value)?),
            "version" => {
                // Splice the expression through verbatim (a literal or an
                // `env!(..)` call); never evaluate it at proc-macro time.
                let value = &pair.value;
                version = Some(quote! { #value });
            }
            "tree" => tree = Some(lit_str(&pair.value)?),
            "default_source" => default_source = Some(lit_str(&pair.value)?),
            "github_repo" => github_repo = Some(lit_str(&pair.value)?),
            "agents" => {
                let list = str_array(&pair.value)?;
                // The registry is closed (an external backend never joins the
                // derive fan-out), so a typo fails here with the known set — the
                // feature-off diagnostic below must never fire for a misspelling.
                if let Some(bad) = list.iter().find(|id| !KNOWN_AGENTS.contains(&id.as_str())) {
                    return Err(syn::Error::new_spanned(
                        &pair.value,
                        format!("unknown agent id `{bad}`; known ids: {}", KNOWN_AGENTS.join(", ")),
                    ));
                }
                agents = Some(list);
            }
            "embed" => embed = Some(lit_bool(&pair.value)?),
            "instructions_fn" => {
                // A fn path (e.g. `guidance::session_block_opt`), spliced through
                // verbatim and called from the emitted `instructions()`.
                let value = &pair.value;
                instructions_fn = Some(quote! { #value });
            }
            other => return Err(syn::Error::new_spanned(&pair.path, format!("unknown `plugin` key `{other}`"))),
        }
    }

    let name = name.ok_or_else(|| syn::Error::new(span, "`#[plugin(..)]` requires `name = \"...\"`"))?;
    let marketplace = marketplace.unwrap_or_else(|| name.clone());
    let version = version.unwrap_or_else(|| quote! { ::core::env!("CARGO_PKG_VERSION") });
    let tree = tree.unwrap_or_else(|| "$CARGO_MANIFEST_DIR/plugin".to_string());
    let default_source = default_source.unwrap_or_else(|| "embedded".to_string());
    let agents = agents.unwrap_or_else(|| vec!["claude".to_string()]);
    if agents.is_empty() {
        return Err(syn::Error::new(span, "`agents` must list at least one backend; an empty list is a silent no-op host"));
    }
    let embed = embed.unwrap_or(true);

    Ok(Attrs { name, marketplace, version, tree, default_source, github_repo, agents, embed, instructions_fn, span })
}

fn default_source_tokens(attrs: &Attrs) -> syn::Result<TokenStream2> {
    let version = &attrs.version;
    match attrs.default_source.as_str() {
        "embedded" => Ok(quote! { ::agentgear::Source::Embedded }),
        "github" => {
            let repo = attrs
                .github_repo
                .as_ref()
                .ok_or_else(|| syn::Error::new(attrs.span, "`default_source = \"github\"` requires `github_repo = \"owner/repo\"`"))?;
            Ok(quote! {
                ::agentgear::Source::GitHub { repo: #repo, ref_: ::core::concat!("v", #version) }
            })
        }
        other => Err(syn::Error::new(attrs.span, format!("`default_source` must be \"embedded\" or \"github\", got {other:?}"))),
    }
}

/// Read `<tree>/.claude-plugin/plugin.json` at expansion: it must exist, be valid
/// JSON, and its `name` must match the `name` attr. build.rs owns the version
/// equality check, so it is not duplicated here.
fn verify_plugin_json(attrs: &Attrs) -> syn::Result<()> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let resolved = attrs.tree.replace("$CARGO_MANIFEST_DIR", &manifest_dir);
    let plugin_json = Path::new(&resolved).join(".claude-plugin").join("plugin.json");

    let bytes = std::fs::read(&plugin_json).map_err(|e| {
        syn::Error::new(
            attrs.span,
            format!("cannot read {} ({e}); ship the plugin tree there or set `tree = \"...\"`", plugin_json.display()),
        )
    })?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| syn::Error::new(attrs.span, format!("{} is not valid JSON: {e}", plugin_json.display())))?;
    let json_name = json
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| syn::Error::new(attrs.span, format!("{} has no string `name`", plugin_json.display())))?;
    if json_name != attrs.name {
        return Err(syn::Error::new(attrs.span, format!("`name = {:?}` does not match plugin.json name {json_name:?}", attrs.name)));
    }
    Ok(())
}

fn lit_str(expr: &Expr) -> syn::Result<String> {
    match expr {
        Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) => Ok(s.value()),
        other => Err(syn::Error::new_spanned(other, "expected a string literal")),
    }
}

fn lit_bool(expr: &Expr) -> syn::Result<bool> {
    match expr {
        Expr::Lit(ExprLit { lit: Lit::Bool(b), .. }) => Ok(b.value),
        other => Err(syn::Error::new_spanned(other, "expected a bool literal (`true` or `false`)")),
    }
}

fn str_array(expr: &Expr) -> syn::Result<Vec<String>> {
    match expr {
        Expr::Array(arr) => arr.elems.iter().map(lit_str).collect(),
        other => Err(syn::Error::new_spanned(other, "expected an array of string literals, e.g. [\"claude\"]")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_agent_id_is_rejected_at_expansion() {
        let input: DeriveInput = syn::parse_quote! {
            #[plugin(name = "x", agents = ["claude", "codx"])]
            struct Host;
        };
        let Err(err) = parse_attrs(&input) else {
            panic!("`codx` must be rejected");
        };
        let msg = err.to_string();
        assert!(msg.contains("unknown agent id `codx`"), "got: {msg}");
        assert!(msg.contains("known ids: claude,"), "must list the known ids: {msg}");
    }

    #[test]
    fn known_agent_ids_parse() {
        let input: DeriveInput = syn::parse_quote! {
            #[plugin(name = "x", agents = ["claude", "copilot-cli", "qwen-code"])]
            struct Host;
        };
        let Ok(attrs) = parse_attrs(&input) else {
            panic!("known ids must parse");
        };
        assert_eq!(attrs.agents, ["claude", "copilot-cli", "qwen-code"]);
    }
}
