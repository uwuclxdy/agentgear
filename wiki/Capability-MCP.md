# MCP servers

An MCP server is declared in `mcpServers` (in `plugin.json`, or a `.mcp.json`). agentgear parses each
into the components IR as one of three kinds — `Stdio`, `Http { url }`, `Sse { url }` — then each
backend renders it into that tool's own config shape. stdio is the verified path on every backend;
remote (http/sse) fidelity varies and is called out per tool below.

Of the 25 backends, **24 host MCP** — every one except `pi`, which has no native MCP surface.
`claude` and `copilot-cli` read the servers straight from the plugin tree (no translation); the
other 22 translate.

## stdio shapes

The rendered server body differs by tool. The families:

- **`{command, args, env}`** — the JSON `mcpServers` majority (gemini, augment, devin, kimi, droid,
  qwen-code, cline, omp, kiro, antigravity family, …).
- **`type:"stdio"` prefixed** — cursor, jetbrains-copilot, vscode-copilot.
- **`type:"local"` with an array command** (`command:[cmd, ...args]`) — opencode, kilo.
- **`[mcp_servers.<name>]` inline table** — codex (TOML).
- **`extensions.<name>` block** — goose (YAML, `{name, type:stdio, cmd, args, envs, enabled, timeout}`).

`copilot-cli` renders no MCP shape at all: its plugin engine reads `.claude-plugin/plugin.json`
directly.

## Remote (http/sse) fidelity

Remote servers render in each tool's own dialect. Some read the rendered `{type, url, headers}` shape
as-is, some key transport off a different field, some would reject the shape and void the file — those
transports are **skipped** rather than written wrong. "Faithful" means it lands in the tool's native
form and dials.

| harness | remote verdict |
|---|---|
| claude | native, no translation (Claude Code reads the tree itself) |
| copilot-cli | native, no translation (copilot's own plugin engine reads the tree itself) |
| amp | faithful. `type` is inert; the tool infers transport from `url` |
| antigravity | faithful: sse rendered as native `{serverUrl}`; http skipped (no landing) |
| antigravity-cli | faithful: sse rendered as native `{serverUrl}`; http skipped (no landing) |
| augment | faithful. byte-matches the tool's own writer |
| cline | faithful: rendered with cline's literal `streamableHttp`/`sse` transport values |
| codex | faithful. streamable-HTTP only; sse maps onto the same `url` |
| crush | faithful. both transports dial-proven |
| cursor | faithful. `type` genuinely picks the transport |
| devin | faithful: rendered with devin's native `{url, transport}` form |
| droid | faithful. `type` is the live discriminator |
| gemini | faithful. native `{url, type, headers?}` |
| goose | http faithful; sse skipped (goose runtime-refuses sse, so a dead extension is never written) |
| jetbrains-copilot | faithful: `{type, url}` (headers will nest under `requestInit.headers` once plugins carry them) |
| kilo | faithful. `type` mandatory, dial-proven |
| kimi | faithful: rendered with kimi's native `{url, transport}` form |
| kiro | unproven (login-walled). native `{url, headers}`, examples carry no `type` |
| omp | faithful. matches native exactly |
| openclaw | faithful, canonical: renders `{url, headers, transport}` directly, matching the tool's own post-`doctor --fix` shape. no churn |
| opencode | faithful. both transports collapse into `remote` |
| pi | skipped. core pi has no native MCP surface |
| qwen-code | faithful: rendered with qwen's key-presence form (http `{httpUrl}`, sse `{url}`) |
| vscode-copilot | http faithful (byte-preserved); sse skipped (VS Code rewrites sse to http, so writing it would churn) |
| zed | http faithful (native `{url, headers}`); sse skipped (zed has a single remote transport) |

## Where each server lands

The file and the key path each backend writes, with the stdio body and the remote shape.

| harness | file | key path | stdio shape | remote shape |
|---|---|---|---|---|
| claude | plugin tree `.claude-plugin/plugin.json` (or `.mcp.json`) | `mcpServers` | `{command,args,env}`, `${CLAUDE_PLUGIN_ROOT}` expands | native, no translation |
| amp | `settings.json` (or `.jsonc`) | `amp.mcpServers` (literal flat-dotted key) | `{command,args,env}` | `{type,url,headers}`, `type` inert |
| antigravity | `~/.gemini/config/mcp_config.json` | `mcpServers` | `{command,args,env}` | `{serverUrl}` sse-only, http skipped |
| antigravity-cli | `~/.gemini/config/mcp_config.json` (user) · `<root>/.agents/mcp_config.json` (project) | `mcpServers.<name>` | `{command,args,env}` | `{serverUrl}` sse-only, http skipped |
| augment | `~/.augment/settings.json` | `mcpServers.<name>` | `{command,args,env}`, no `type` | `{type,url,headers}` |
| cline | `~/.cline/data/settings/cline_mcp_settings.json` | `mcpServers` (global only) | `{command,args,env}` | `{type:"streamableHttp"\|"sse",url,headers}` |
| codex | `config.toml` | `[mcp_servers.<name>]` | `{command,args,env}` inline table | `{url}`, streamable-HTTP only |
| copilot-cli | native — `copilot plugin install` copies the tree verbatim | n/a | native, no translation | native, no translation |
| crush | `crush.json` | root `mcp.<name>` | `{type:"stdio",command,args,env}` | `{type,url,headers}` |
| cursor | `mcp.json` | `mcpServers` | `{type:"stdio",command,args,env}` | `{type,url,headers}`, `type` picks transport |
| devin | `config.json` | `mcpServers` | `{command,args,env}` | `{url,transport}` |
| droid | `mcp.json` | `mcpServers.<name>` | `{command,args,env}` | `{type,url,headers}`, `type` is the discriminator |
| gemini | `settings.json` | `mcpServers.<name>` | `{command,args,env}` | `{type,url,headers}` |
| goose | `config.yaml` | `extensions.<name>` | `{name,type:stdio,cmd,args,envs,enabled,timeout}` | http `{name,type:streamable_http,uri,…}`; sse skipped |
| jetbrains-copilot | `mcp.json` | `servers` | `{type:"stdio",command,args,env}` | `{type,url}` (no flat `headers` key yet) |
| kilo | `kilo.json` | root `mcp.<name>` | `{type:"local",command:[cmd,...args],environment,enabled}` | `{type:"remote",url,headers,enabled}`, `type` mandatory |
| kimi | `~/.kimi-code/mcp.json` | `mcpServers` | `{command,args,env}` | `{url,transport}` |
| kiro | `settings/mcp.json` | `mcpServers` | `{command,args,env}` | `{type,url,headers}` (unproven, login-walled) |
| omp | `mcp.json` | `mcpServers` | `{command,args,env}` | `{type,url,headers}` |
| openclaw | `openclaw.json` | `mcp.servers.<name>` | `{command,args,env}` | `{url,headers,transport}`, canonical |
| opencode | `opencode.json` | root `mcp.<name>` | `{type:"local",command:[cmd,...args],enabled,environment}` | `{type:"remote",url,enabled}` |
| pi | none | — | — | skipped |
| qwen-code | `settings.json` | `mcpServers.<name>` | `{command,args,env}` | http `{httpUrl}` · sse `{url}` |
| vscode-copilot | `<project>/.vscode/mcp.json` | `servers` | `{type:"stdio",command,args,env}` | http byte-preserved; sse skipped |
| zed | `settings.json` | `context_servers.<name>` | `{command,args,env}` | http `{url,headers}`; sse skipped |

## Gotchas

- **Portability skip.** A server whose `command` or `args` carry `${CLAUDE_PLUGIN_ROOT}` is silently
  dropped on every config-merge backend — the token expands only inside Claude Code. Doctor's MCP
  check then reports "no portable mcp servers to register" instead of naming a server. Write a bare
  executable name. See [Plugin tree](Plugin-Tree#claude_plugin_root-portability).
- **`${AGENTGEAR_CLIENT}` expands instead of skipping.** Each backend substitutes it for its own
  client id in the server's `command`/`args`. See
  [Plugin tree](Plugin-Tree#agentgear_client-per-harness-client-id).
- **`pi`** ships no MCP surface at all; its `~/.pi/agent/mcp.json` belongs to a third-party extension,
  so agentgear writes nothing there.
- **Skipped transports are silent by design.** antigravity http, and goose/zed/vscode-copilot sse,
  have no faithful landing, so those servers are dropped rather than written to the wrong transport.
  A remote server that never appears in the tool traces back to one of these.
- **A same-key entry elsewhere can outrank ours.** e.g. crush honors a same-name server in
  `$XDG_DATA_HOME/crush/crush.json` at higher precedence, which agentgear never writes.

## See also

- [Capabilities](Capabilities) — the index and the portability filter.
- [Agent backends](Agent-Backends) — the shared MCP renderers and how to add a backend.
- [Harness comparison](Harness-Comparison) — the harness-first grid.
