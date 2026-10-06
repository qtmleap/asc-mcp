#!/bin/sh
# Generates only a private local HTTP bearer token; Apple credentials stay unset.
set -eu
config_dir=${ASC_MCP_CONFIG_DIR:-"$HOME/.config/asc-mcp"}
umask 077
mkdir -p "$config_dir/credentials" "$config_dir/credentials-login" "$config_dir/state"
if [ ! -f "$config_dir/runtime.env" ]; then
  token=$(openssl rand -hex 32)
  printf 'ASC_MCP_AUTH_TOKENS=%s\nASC_MCP_UID=%s\nASC_MCP_GID=%s\n' "$token" "$(id -u)" "$(id -g)" > "$config_dir/runtime.env"
fi
printf 'Private configuration directory: %s\n' "$config_dir"
