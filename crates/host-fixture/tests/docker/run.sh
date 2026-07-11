#!/usr/bin/env bash
# Per-harness docker leg dispatcher. `run.sh <harness>` builds the image at
# tests/docker/<harness>/Dockerfile and runs it; a nonzero container exit fails
# the leg. Each <harness>/ dir owns its Dockerfile (written by that harness's
# workflow) — this dispatcher stays disjoint from every backend.
#
# The per-harness Dockerfile contract (see README.md) is multi-stage:
#   stage 1 (rust): build `host_fixture`.
#   stage 2: install the real <harness> CLI, copy the binary in, run
#            `host_fixture setup --agent <harness>`, assert our mcp server via the
#            harness's native `mcp list` (auth-free) or a config-file parse, then
#            `host_fixture uninstall` and assert it is gone.
set -euo pipefail

harness="${1:?usage: run.sh <codex|opencode|gemini|cursor|cline|devin>}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dockerfile="$here/$harness/Dockerfile"

if [ ! -f "$dockerfile" ]; then
    echo "no Dockerfile for '$harness' yet: $dockerfile" >&2
    echo "the $harness workflow writes it; see $here/README.md for the contract." >&2
    exit 1
fi

# The build context is the repo root so the Dockerfile can `COPY` the workspace.
repo_root="$(cd "$here/../../../.." && pwd)"
tag="agentgear-harness-$harness"

# The Dockerfiles use `RUN <<'SH'` heredocs, a BuildKit-only feature. The legacy
# builder parses them without error but never runs the script-writing body, so it
# produces an image whose entrypoint script is missing and fails cryptically at run
# time. Require buildx and build through it so that fallback can't happen silently.
if ! docker buildx version >/dev/null 2>&1; then
    echo "the harness docker legs need Docker BuildKit (the Dockerfiles use RUN heredocs)." >&2
    echo "install the buildx plugin: https://docs.docker.com/go/buildx/" >&2
    exit 1
fi

docker buildx build --load -f "$dockerfile" -t "$tag" "$repo_root"
docker run --rm "$tag"
