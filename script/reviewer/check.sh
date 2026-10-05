#!/bin/sh
set -eu
asc_repo="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
asc_binary="$asc_repo/target/release/asc-mcp"
if [ "${1:-}" = '--login' ]; then
  shift
  if [ "$#" -gt 0 ]; then exec "$asc_binary" reviewer-login --apple-id "$1"; fi
  exec "$asc_binary" reviewer-login
fi
exec "$asc_binary" reviewer-check "$@"
