#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
mode=${1:-standalone}
if [ "$#" -gt 0 ]; then shift; fi
case "$mode" in
  -h|--help)
    printf 'Usage: developer-login.sh [standalone|stack] --apple-id ACCOUNT --team-id TEAM\nRequires an interactive terminal and existing Docker setup.\n'
    exit 0 ;;
  standalone|stack) ;;
  *) printf 'Expected standalone or stack\n' >&2; exit 2 ;;
esac

if [ ! -t 0 ] || [ ! -t 2 ]; then
  printf 'Developer login requires an interactive terminal for password and verification.\n' >&2
  exit 2
fi

config_dir=${ASC_MCP_CONFIG_DIR:-"$HOME/.config/asc-mcp"}
if [ ! -f "$config_dir/runtime.env" ] || [ ! -d "$config_dir/state" ]; then
  printf 'Run script/docker/setup.sh before Developer login.\n' >&2
  exit 2
fi
config_dir=$(CDPATH= cd -- "$config_dir" && pwd)
state_root='/state/Library/Application Support/ASC MCP/Developer Portal'
configured_state_root=$(sed -n 's/^[[:space:]]*ASC_DEVELOPER_STATE_ROOT=//p' "$config_dir/runtime.env" | tail -n 1)
if [ -n "$configured_state_root" ]; then state_root=$configured_state_root; fi
cookie_path="$state_root/developer-session.cookie"
configured_cookie_path=$(sed -n 's/^[[:space:]]*ASC_DEVELOPER_COOKIE_PATH=//p' "$config_dir/runtime.env" | tail -n 1)
if [ -n "$configured_cookie_path" ]; then cookie_path=$configured_cookie_path; fi

for container_path in "$state_root" "$cookie_path"; do
  case "$container_path" in
    /state/*) ;;
    *) printf 'Developer paths must be inside the shared /state directory.\n' >&2; exit 2 ;;
  esac
  case "$container_path/" in
    */../*|*/./*|*//*) printf 'Developer paths must not contain traversal or empty components.\n' >&2; exit 2 ;;
  esac
  if printf '%s' "$container_path" | LC_ALL=C grep -q '[[:cntrl:]]'; then
    printf 'Developer paths must not contain control characters.\n' >&2
    exit 2
  fi
done

export ASC_MCP_CONFIG_DIR=$config_dir
exec "$repo_dir/script/docker/compose.sh" "$mode" run --rm --no-deps \
  --env HOME=/state \
  --env "ASC_DEVELOPER_STATE_ROOT=$state_root" \
  --env "ASC_DEVELOPER_COOKIE_PATH=$cookie_path" \
  asc-mcp developer-login "$@"
