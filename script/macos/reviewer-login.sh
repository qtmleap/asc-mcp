#!/bin/sh
set -eu
. "$(dirname -- "$0")/reviewer-env.sh"
if [ ! -x "$ASC_MCP_HOST_BINARY" ]; then
  printf 'Install the native host helper first with script/macos/install-renewal-agent.sh --binary /path/to/asc-mcp\n' >&2
  exit 2
fi
exec "$ASC_MCP_HOST_BINARY" reviewer-login "$@"
