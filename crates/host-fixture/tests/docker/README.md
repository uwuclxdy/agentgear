# per-harness docker legs

each `<harness>/Dockerfile` runs the fixture plugin's install against the **real**
harness CLI in a throwaway container, proving our written config is one the tool
actually ingests. the hermetic rust tests (`crates/host-fixture/tests/<harness>.rs`)
own correctness; this leg owns "the real CLI accepts it".

## run

```sh
crates/host-fixture/tests/docker/run.sh <harness>
```

`<harness>` is any dir here holding a `Dockerfile` (19 today).

`run.sh` builds `<harness>/Dockerfile` (context = repo root) and runs it; a nonzero
container exit fails the leg. It needs Docker with the buildx plugin (the Dockerfiles
use `RUN` heredocs, a BuildKit feature) and errors early if buildx is missing.

## what each Dockerfile must do (the contract)

owned by that harness's workflow, disjoint from every other file:

1. **stage 1 (`rust`)**: build `host_fixture` (the fixture host binary).
2. **stage 2**: base image with the real `<harness>` CLI installed (pin a version),
   copy `host_fixture` in, put it on `PATH`.
3. run `host_fixture setup --agent <harness>` — installs only that backend, so no
   `claude` is needed in the image.
4. **assert present**: native `mcp list` where the CLI is auth-free, else parse the
   written config file. the `ez-fixture` server must be listed.
5. run `host_fixture uninstall`, then **assert gone**: the server is removed and any
   pre-seeded unrelated user entry survived. seed that entry before `setup`: removal
   prunes a container our own keys emptied and deletes a config file left holding
   nothing, so an unseeded leg has no file left to parse.
6. exit nonzero on any failed assertion.

## notes

- keep the assertion inside the container so the leg is hermetic (no host state).
- point the harness at a throwaway `$HOME` / config dir inside the image; never the
  builder's real config.
- CI wires one matrix leg per harness calling `run.sh <harness>` (foundation adds
  the job; each Dockerfile lands with its backend).
- a real tool-call round-trip is out of reach today: every backed CLI routes MCP tool
  invocation through the model in an agent session (auth-gated + non-deterministic),
  and none ships a `mcp call` verb. The native `mcp list` above is the deepest
  auth-free proof of ingestion. The `ez-fixture` server advertises a `ping` tool
  (`host_fixture mcp`) for the day a CLI ships `mcp call`.
- sweeping every leg locally: loop `run.sh` over the dirs holding a `Dockerfile`,
  sequentially. **never build two legs concurrently.** They contend on the shared
  buildkit cache, and the per-leg image tags (`agentgear-harness-<id>`) do not isolate
  the layer store.
- a leg that dies deterministically in `exporting layers` with `failed to open writer:
  ref moby/... locked: unavailable` hit moby's containerd-store duplicate-layer export
  bug on the host, not a defect in the leg. `docker builder prune -af` clears it. Seen
  on openclaw across two consecutive runs (2026-07-16); the CLI installed clean every
  time once pruned.
