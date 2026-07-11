# per-harness docker legs

each `<harness>/Dockerfile` runs the fixture plugin's install against the **real**
harness CLI in a throwaway container, proving our written config is one the tool
actually ingests. the hermetic rust tests (`crates/host-fixture/tests/<harness>.rs`)
own correctness; this leg owns "the real CLI accepts it".

## run

```sh
crates/host-fixture/tests/docker/run.sh <codex|opencode|gemini|cursor|cline|devin>
```

`run.sh` builds `<harness>/Dockerfile` (context = repo root) and runs it; a nonzero
container exit fails the leg.

## what each Dockerfile must do (the contract)

owned by that harness's workflow, disjoint from every other file:

1. **stage 1 (`rust`)**: build `host_fixture` (the fixture host binary).
2. **stage 2**: base image with the real `<harness>` CLI installed (pin a version),
   copy `host_fixture` in, put it on `PATH`.
3. run `host_fixture setup --agent <harness>` — installs only that backend, so no
   `claude` is needed in the image.
4. **assert present**: native `mcp list` where auth-free (codex/opencode/gemini/
   devin) or parse the written config file (cursor/cline). the `ez-fixture` server
   must be listed.
5. run `host_fixture uninstall`, then **assert gone**: the server is removed and any
   pre-seeded unrelated user entry survived.
6. exit nonzero on any failed assertion.

## notes

- keep the assertion inside the container so the leg is hermetic (no host state).
- point the harness at a throwaway `$HOME` / config dir inside the image; never the
  builder's real config.
- CI wires one matrix leg per harness calling `run.sh <harness>` (foundation adds
  the job; each Dockerfile lands with its backend).
