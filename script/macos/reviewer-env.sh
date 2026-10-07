#!/bin/sh
# Host-only paths for the existing Docker sidecar. No secret-bearing env is loaded.
asc_config_dir=${ASC_MCP_CONFIG_DIR:-"$HOME/.config/asc-mcp"}
if [ -d "$asc_config_dir" ]; then
  asc_config_dir=$(CDPATH= cd -P -- "$asc_config_dir" && pwd -P)
else
  case "$asc_config_dir" in /*) ;; *) asc_config_dir="$(pwd -P)/$asc_config_dir";; esac
fi
asc_state_name='ASC MCP'
asc_state_base="$asc_config_dir/state/Library/Application Support"
if [ ! -e "$asc_state_base/ASC MCP" ] && [ ! -L "$asc_state_base/ASC MCP" ] &&
   { [ -e "$asc_state_base/InterceptorReview" ] || [ -L "$asc_state_base/InterceptorReview" ]; }; then
  asc_state_name='InterceptorReview'
fi
ASC_REVIEW_COOKIE_PATH="$asc_config_dir/credentials/session.cookie"
ASC_REVIEW_TRUST_ROOT="$asc_config_dir/credentials-login"
ASC_REVIEW_STATE_ROOT="$asc_state_base/$asc_state_name"
if [ -f "$asc_config_dir/runtime.env" ]; then
  asc_configured_state=$(sed -n 's/^[[:space:]]*ASC_REVIEW_STATE_ROOT=//p' "$asc_config_dir/runtime.env" | tail -n 1)
  if [ -n "$asc_configured_state" ]; then
    case "$asc_configured_state" in
      /state/*)
        case "/${asc_configured_state#/state/}/" in */../*|*/./*) printf 'Invalid configured ASC state path\n' >&2; exit 2;; esac
        ASC_REVIEW_STATE_ROOT="$asc_config_dir/state/${asc_configured_state#/state/}" ;;
      *) printf 'Configured ASC state root must be beneath the container /state mount\n' >&2; exit 2 ;;
    esac
  fi
fi
# Preflight only: the Rust helper independently uses no-follow directory descriptors,
# so a container swapping a path after this check cannot redirect host I/O.
asc_state_probe=$ASC_REVIEW_STATE_ROOT
while [ "$asc_state_probe" != "$asc_config_dir" ]; do
  if [ -L "$asc_state_probe" ]; then
    printf 'ASC shared state path contains a symlink\n' >&2; exit 2
  fi
  asc_state_probe=$(dirname -- "$asc_state_probe")
  case "$asc_state_probe/" in
    "$asc_config_dir/"|"$asc_config_dir/state/"*) ;;
    *) printf 'ASC state path escapes its configured mount\n' >&2; exit 2;;
  esac
done
ASC_MCP_HOST_BINARY=${ASC_MCP_HOST_BINARY:-"$HOME/.local/share/asc-mcp/bin/asc-mcp"}
export ASC_REVIEW_COOKIE_PATH ASC_REVIEW_TRUST_ROOT ASC_REVIEW_STATE_ROOT ASC_MCP_HOST_BINARY
