#!/bin/sh
# Usage: compose.sh [standalone|stack] [Docker Compose arguments...]
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
export ASC_MCP_SOURCE_DIR=$repo_dir
config_dir=${ASC_MCP_CONFIG_DIR:-"$HOME/.config/asc-mcp"}
mode=${1:-standalone}
if [ "$#" -gt 0 ]; then shift; fi
[ -f "$config_dir/runtime.env" ] || "$repo_dir/script/docker/setup.sh"
if [ "$#" -eq 0 ]; then set -- up -d --build; fi
case "$mode" in
 standalone)
  exec docker compose --env-file "$config_dir/runtime.env" -p asc-mcp -f "$repo_dir/compose.yaml" "$@" ;;
 stack)
  localgpt_dir=${LOCALGPT_DIR:-"$(dirname -- "$repo_dir")/local-gpt"}
  if [ -f "$localgpt_dir/compose.override.yaml" ]; then
    exec docker compose --env-file "$config_dir/runtime.env" --project-directory "$localgpt_dir" -p localgpt -f "$localgpt_dir/compose.yaml" -f "$localgpt_dir/compose.override.yaml" -f "$repo_dir/compose.yaml" "$@"
  fi
  exec docker compose --env-file "$config_dir/runtime.env" --project-directory "$localgpt_dir" -p localgpt -f "$localgpt_dir/compose.yaml" -f "$repo_dir/compose.yaml" "$@" ;;
 *) printf 'Expected standalone or stack\n' >&2; exit 2 ;;
esac
