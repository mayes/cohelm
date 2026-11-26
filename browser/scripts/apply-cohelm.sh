#!/usr/bin/env bash
# Adds Cohelm's patches and GN flags to a checked-out platform build repo
# (ungoogled-chromium-macos or ungoogled-chromium-windows), so its own build
# applies them after ungoogled-chromium's.
#
#   apply-cohelm.sh <platform-repo-dir> <flags-file-name>
#
# Run by build-macos.sh and build-windows.ps1; safe to run twice.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
repo="$1"
flags_file="$repo/$2"

test -f "$repo/patches/series" || { echo "no patches/series in $repo" >&2; exit 1; }
test -f "$flags_file" || { echo "no $flags_file" >&2; exit 1; }

marker="# --- Cohelm ---"

if ! grep -qxF "$marker" "$repo/patches/series"; then
  echo "$marker" >> "$repo/patches/series"
  # The series may hold no patches yet, so grep finding nothing is fine.
  { grep -vE '^\s*(#|$)' "$here/patches/series" || true; } | while read -r patch; do
    mkdir -p "$repo/patches/$(dirname "$patch")"
    cp "$here/patches/$patch" "$repo/patches/$patch"
    echo "$patch" >> "$repo/patches/series"
  done
fi

if ! grep -qxF "$marker" "$flags_file"; then
  { echo "$marker"; grep -vE '^\s*(#|$)' "$here/flags.gn"; } >> "$flags_file"
fi
