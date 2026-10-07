#!/bin/sh
set -eu
. "$(dirname -- "$0")/reviewer-env.sh"
exec "$ASC_MCP_HOST_BINARY" reviewer-renew
