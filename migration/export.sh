#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
fixture=internal/asc/api/zz_migration_capture.go
if [ -e "$fixture" ]; then
  echo "Capture client already exists; refusing to overwrite it" >&2
  exit 1
fi
cleanup() {
  python3 -c 'import pathlib,sys; pathlib.Path(sys.argv[1]).unlink(missing_ok=True)' "$fixture"
}
trap cleanup EXIT HUP INT TERM
cp migration/capture_client.go.txt "$fixture"
go run -tags migration_export ./migration/export
python3 migration/derive_recipes.py
python3 migration/generate_ledger.py
