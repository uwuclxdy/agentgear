//! `#[derive(PluginHost)]` + `#[plugin(..)]`. At expansion the macro reads the
//! shipped `plugin.json` (existence, JSON validity, and a name cross-check against
//! the `name` attr), bakes the tree in via `include_dir!`, implements the
//! `PluginHost` trait from the attrs, and emits a const-panic guard that fires if
//! the host forgot its `build.rs` (design §7, §10).

use std::path::Path;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{DeriveInput, Expr, ExprLit, Lit, MetaNameValue, Token, parse_macro_input};

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
    span: proc_macro2::Span,
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;
    let attrs = parse_attrs(&input)?;

    verify_plugin_json(&attrs)?;

    let name = &attrs.name;
    let marketplace = &attrs.marketplace;
    let version = &attrs.version;
    let tree = &attrs.tree;
    let agents = &attrs.agents;
    let default_source = default_source_tokens(&attrs)?;

    Ok(quote! {
        impl ::ez_agent_plugin::PluginHost for #ident {
            const NAME: &'static str = #name;
            const MARKETPLACE: &'static str = #marketplace;
            const VERSION: &'static str = #version;
            const DEFAULT_SOURCE: ::ez_agent_plugin::Source = #default_source;
            const AGENTS: &'static [&'static str] = &[#(#agents),*];

            fn embedded_tree() -> &'static ::ez_agent_plugin::__private::include_dir_crate::Dir<'static> {
                use ::ez_agent_plugin::__private::include_dir_crate as include_dir;
                static TREE: ::ez_agent_plugin::__private::include_dir_crate::Dir<'static> =
                    ::ez_agent_plugin::__private::include_dir_macro!(#tree);
                &TREE
            }
        }

        // Fires only if the host forgot its build.rs, which would also lose the
        // version guard and the include_dir rebuild tracking. A compile error with
        // the fix beats a silent no-op.
        const _: () = {
            if ::core::option_env!("EZ_PLUGIN_GUARD").is_none() {
                ::core::panic!(
                    "ez-agent-plugin: host is missing its build.rs calling ez_agent_plugin::build::assert_plugin_version() (see the crate docs)"
                );
            }
        };
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
            "agents" => agents = Some(str_array(&pair.value)?),
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

    Ok(Attrs { name, marketplace, version, tree, default_source, github_repo, agents, span })
}

fn default_source_tokens(attrs: &Attrs) -> syn::Result<TokenStream2> {
    let version = &attrs.version;
    match attrs.default_source.as_str() {
        "embedded" => Ok(quote! { ::ez_agent_plugin::Source::Embedded }),
        "github" => {
            let repo = attrs
                .github_repo
                .as_ref()
                .ok_or_else(|| syn::Error::new(attrs.span, "`default_source = \"github\"` requires `github_repo = \"owner/repo\"`"))?;
            Ok(quote! {
                ::ez_agent_plugin::Source::GitHub { repo: #repo, ref_: ::core::concat!("v", #version) }
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

fn str_array(expr: &Expr) -> syn::Result<Vec<String>> {
    match expr {
        Expr::Array(arr) => arr.elems.iter().map(lit_str).collect(),
        other => Err(syn::Error::new_spanned(other, "expected an array of string literals, e.g. [\"claude\"]")),
    }
}
