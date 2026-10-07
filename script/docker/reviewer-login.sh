#!/bin/sh
# Interactive one-off login; the running service keeps its read-only credentials mount and never
# sees the login-only trust directory.
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
mode=${1:-stack}
if [ "$#" -gt 0 ]; then shift; fi
case "$mode" in
 -h|--help)
  printf 'Usage: reviewer-login.sh [stack|standalone] [reviewer-login options]\nRun from an interactive terminal. Password and 2FA are entered there.\n'
  exit 0 ;;
 stack|standalone) ;;
 *) printf 'Expected stack or standalone\n' >&2; exit 2 ;;
esac
if [ ! -t 0 ] || [ ! -t 2 ]; then
  printf 'Reviewer login requires an interactive terminal for password and 2FA.\n' >&2
  exit 2
fi
if [ "$(uname -s)" = Darwin ]; then
  exec "$repo_dir/script/macos/reviewer-login.sh" "$@"
fi
config_dir=${ASC_MCP_CONFIG_DIR:-"$HOME/.config/asc-mcp"}
[ -d "$config_dir/credentials" ] || "$repo_dir/script/docker/setup.sh"
config_dir=$(CDPATH= cd -- "$config_dir" && pwd)
# Login-only trusted-device cookies live beside, never inside, the service credentials mount.
umask 077
mkdir -p "$config_dir/credentials-login" "$config_dir/state"
export ASC_MCP_CONFIG_DIR=$config_dir
state_name='ASC MCP'
state_base="$config_dir/state/Library/Application Support"
if [ ! -e "$state_base/ASC MCP" ] && [ ! -L "$state_base/ASC MCP" ] &&
   { [ -e "$state_base/InterceptorReview" ] || [ -L "$state_base/InterceptorReview" ]; }; then
  state_name='InterceptorReview'
fi
state_root="/state/Library/Application Support/$state_name"
# Read only this raw Compose env key; never source or evaluate runtime.env.
configured_state_root=$(sed -n 's/^[[:space:]]*ASC_REVIEW_STATE_ROOT=//p' "$config_dir/runtime.env" | tail -n 1)
if [ -n "$configured_state_root" ]; then
  state_root=$configured_state_root
fi
exec "$repo_dir/script/docker/compose.sh" "$mode" run --rm --no-deps \
  --env HOME=/login-home \
  --env "ASC_REVIEW_STATE_ROOT=$state_root" \
  --env ASC_REVIEW_TRUST_ROOT=/login-trust \
  --volume "$config_dir/credentials:/login-home/Library/Application Support/ASC MCP:rw" \
  --volume "$config_dir/credentials-login:/login-trust:rw" \
  asc-mcp reviewer-login "$@"
