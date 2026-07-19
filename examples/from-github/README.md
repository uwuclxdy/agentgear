# from-github

A zero-embed agentgear host: the binary ships no baked plugin blob and installs its
plugin from a GitHub marketplace instead. [`hello-mcp`](../hello-mcp) is the smallest
host to start from; read this one for the distribution choice.

`src/lib.rs` holds the derive (the `embed = false` + `default_source = "github"`
pairing) and `Cargo.toml` its feature half. Read them together.

## What it demonstrates

- The zero-embed feature pairing: `default-features = false` on the lib (no `embed`
  feature, so no `tar`/`brotli` and no decompress path) with `embed = false` on the
  derive (an empty baked blob instead of `include_bytes!`). The crate compiling at
  all is the proof both halves line up.
- `default_source = "github"`: `setup` keys on `Source::GitHub { repo, ref_ }` rather
  than a baked tree, and `update`/`self-heal`/`doctor` resolve against the same ref.

## The `plugin/` tree still matters

The in-repo `plugin/` tree is not embedded, but it is not dead weight either: the
derive reads its `plugin.json` at compile time to cross-check the `name`, and the
one-line `build.rs` asserts `version` == `CARGO_PKG_VERSION`. Treat it as the source
you would publish at the GitHub repo root. The binary installs whatever the repo
serves at the pinned ref, not this local copy.

## Run it

```sh
cargo run -p from-github -- <setup|update|uninstall|self-heal|doctor>
```

## Expected output

`setup` with no `claude` on PATH skips the (undetected) backend and converges to
nothing:

```
$ from-github setup
NoOp
```

`setup --embedded` forces the wrong source for a zero-embed host on purpose. There is
no baked blob to materialize, so it fails (exit 1):

```
$ from-github setup --embedded
error: invalid plugin tree: this binary was built without the `embed` feature; use `Source::Path` or `Source::GitHub`, or enable `embed`
```

The exact message varies: a build with the lib's `embed` feature on reports `embedded
blob is empty` instead. Either way it is the same failure. A zero-embed host cannot
install from `Source::Embedded`, which is why `setup` defaults to `DEFAULT_SOURCE`.

## When to choose GitHub over embedded

Embedded bakes the plugin tree into the binary, so a plugin change needs a new binary
release. A GitHub source decouples the two: publish a new tagged plugin version and
`claude plugin update` pulls it without a binary rebuild. Pick GitHub when the plugin
iterates faster than the binary.

## Ref-pinning semantics

The derive defaults `ref_` to `v{CARGO_PKG_VERSION}` (the tag `claude plugin tag`
produces), so a given binary tracks the plugin tag that matches its own version rather
than a moving branch. That ref reaches the CLI: the marketplace is registered as
`owner/repo@ref`.

A marketplace already present on a *different* ref gets re-added instead of updated,
because `claude plugin marketplace update` refreshes a pin without moving it. Re-adding
is what re-points the pin when a binary upgrade changes the tracked tag.

`tests/wiring.rs`'s `github_install_pins_the_version_tag` covers that flow against the
real `claude` CLI and a pushed tag. It is `#[ignore]`d and gated on
`AGENTGEAR_E2E_GITHUB=1` on top of that, so a default test run never couples to a
mutable remote. The file's other tests run unconditionally, against a fake `claude`
shim where they need one.
