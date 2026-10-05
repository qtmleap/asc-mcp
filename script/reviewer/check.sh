#!/bin/sh
set -eu
# Use the same Ruby and gem paths as Homebrew's fastlane launcher.
asc_fastlane_prefix="$(brew --prefix fastlane)"
asc_ruby_prefix="$(brew --prefix ruby)"
asc_fastlane_gem_home="${FASTLANE_GEM_HOME:-${HOME}/.local/share/fastlane/4.0.0}"
asc_script_directory="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
export GEM_HOME="$asc_fastlane_gem_home"
export GEM_PATH="$asc_fastlane_gem_home:$asc_fastlane_prefix/libexec"
export FASTLANE_SKIP_UPDATE_CHECK=1
export SPACESHIP_TIMEOUT=12
exec "$asc_ruby_prefix/bin/ruby" "$asc_script_directory/check.rb" "$@"
