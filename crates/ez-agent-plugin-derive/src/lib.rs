//! Derive macro for `ez-agent-plugin`. Full expansion (plugin.json cross-check,
//! `include_dir!` emission, the `EZ_PLUGIN_GUARD` const-panic) lands in the macro
//! phase; see `docs/design.md` §10. This placeholder declares the surface so hosts
//! can wire `#[derive(PluginHost)]` + `#[plugin(..)]` before the body exists.

use proc_macro::TokenStream;

#[proc_macro_derive(PluginHost, attributes(plugin))]
pub fn derive_plugin_host(_input: TokenStream) -> TokenStream {
    TokenStream::new()
}
