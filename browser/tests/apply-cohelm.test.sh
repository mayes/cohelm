#!/usr/bin/env bash
# Checks apply-cohelm.sh against a fake platform repo: Cohelm's flags land
# after the platform's, and running it twice changes nothing.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/repo/patches"
printf 'upstream/one.patch\n' > "$work/repo/patches/series"
printf 'is_official_build=true\n' > "$work/repo/flags.macos.gn"

"$here/scripts/apply-cohelm.sh" "$work/repo" flags.macos.gn
"$here/scripts/apply-cohelm.sh" "$work/repo" flags.macos.gn

flags="$(cat "$work/repo/flags.macos.gn")"
[ "$(head -n1 <<<"$flags")" = "is_official_build=true" ] || { echo "platform flags moved"; exit 1; }
grep -qx "enable_reporting=false" <<<"$flags" || { echo "Cohelm flags missing"; exit 1; }
[ "$(grep -c '^# --- Cohelm ---$' "$work/repo/flags.macos.gn")" = 1 ] || { echo "flags added twice"; exit 1; }
[ "$(grep -c '^# --- Cohelm ---$' "$work/repo/patches/series")" = 1 ] || { echo "series added twice"; exit 1; }
[ "$(head -n1 "$work/repo/patches/series")" = "upstream/one.patch" ] || { echo "upstream patches moved"; exit 1; }
# With a patch in Cohelm's series, it is copied in and listed after upstream's.
cp -R "$here" "$work/cohelm"
mkdir -p "$work/cohelm/patches/cohelm"
printf -- '--- a/x\n+++ b/x\n' > "$work/cohelm/patches/cohelm/example.patch"
printf 'cohelm/example.patch\n' >> "$work/cohelm/patches/series"
mkdir -p "$work/repo2/patches"
printf 'upstream/one.patch\n' > "$work/repo2/patches/series"
printf '\n' > "$work/repo2/flags.windows.gn"
"$work/cohelm/scripts/apply-cohelm.sh" "$work/repo2" flags.windows.gn
[ "$(tail -n1 "$work/repo2/patches/series")" = "cohelm/example.patch" ] || { echo "patch not listed"; exit 1; }
test -f "$work/repo2/patches/cohelm/example.patch" || { echo "patch not copied"; exit 1; }

echo "apply-cohelm: ok"
