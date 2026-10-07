#!/bin/sh
# Install a stable host binary and an agent; enrollment stays interactive.
set -eu
if [ "$(uname -s)" != Darwin ]; then printf 'Automatic reviewer renewal requires a macOS host\n' >&2; exit 2; fi
if [ "$#" != 2 ] || [ "$1" != --binary ] || [ ! -x "$2" ]; then
  printf 'Usage: install-renewal-agent.sh --binary /absolute/path/to/native/asc-mcp\n' >&2; exit 2
fi
asc_binary=$2
asc_scripts=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$asc_scripts/reviewer-env.sh"
umask 077
asc_install_dir="$HOME/.local/share/asc-mcp/bin"
asc_label='work.asc-mcp.reviewer-renewal'
asc_plist="$HOME/Library/LaunchAgents/$asc_label.plist"
mkdir -p "$asc_install_dir" "$HOME/Library/LaunchAgents" "$asc_config_dir/logs" "$ASC_REVIEW_TRUST_ROOT" "$ASC_REVIEW_STATE_ROOT"
# A private stable pathname lets interactive enrollment authorize the same binary as launchd.
cp "$asc_binary" "$asc_install_dir/.asc-mcp.new"
chmod 700 "$asc_install_dir/.asc-mcp.new"
mv "$asc_install_dir/.asc-mcp.new" "$asc_install_dir/asc-mcp"
cp "$asc_scripts/reviewer-env.sh" "$asc_scripts/reviewer-renew.sh" "$asc_scripts/reviewer-login.sh" "$asc_install_dir/"
chmod 700 "$asc_install_dir"/*.sh
command -v python3 >/dev/null 2>&1 || { printf 'Python 3 is required once to generate the LaunchAgent plist\n' >&2; exit 2; }
python3 - "$asc_plist" "$asc_install_dir" "$asc_config_dir" "$ASC_REVIEW_STATE_ROOT" <<'PY'
import os,plistlib,sys
from pathlib import Path
plist,installed,config,state=sys.argv[1:]
p=Path(state)/'renewal.request.json'
if p.is_symlink() or p.exists() and not p.is_file(): raise SystemExit('Invalid renewal request marker')
if not p.exists():
 fd=os.open(p,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
 with os.fdopen(fd,'w') as f:f.write('{"version":1}\n')
value={'Label':'work.asc-mcp.reviewer-renewal','ProgramArguments':[installed+'/reviewer-renew.sh'],'EnvironmentVariables':{'ASC_MCP_CONFIG_DIR':config},'RunAtLoad':True,'StartInterval':900,'ThrottleInterval':10,'WatchPaths':[str(p)],'StandardOutPath':config+'/logs/renewal.log','StandardErrorPath':config+'/logs/renewal-error.log'}
with open(plist,'wb') as f:plistlib.dump(value,f)
os.chmod(plist,0o600)
PY
launchctl bootout "gui/$(id -u)/$asc_label" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$asc_plist"
printf 'Installed native reviewer renewal agent. Enroll once with script/docker/reviewer-login.sh stack --fresh; later logins reuse the account and Keychain.\n'
