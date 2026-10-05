#!/bin/sh
# Connect Codex stdio MCP to the already-running shared stack container.
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
exec "$repo_dir/script/docker/compose.sh" stack exec -T asc-mcp /usr/local/bin/asc-mcp serve
