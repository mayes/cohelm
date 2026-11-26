#!/usr/bin/env bash
# Clones the platform build repo at the tag pinned in ungoogled-chromium.env,
# with its ungoogled-chromium submodule, and checks the submodule is the
# pinned release.
#
#   checkout-platform.sh macos|windows <dest-dir>
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck disable=SC1091  # the pinned release, beside this folder
source "$here/ungoogled-chromium.env"

platform="$1"
dest="$2"
case "$platform" in
  macos) tag="$UC_MACOS_TAG" ;;
  windows) tag="$UC_WINDOWS_TAG" ;;
  *) echo "platform must be macos or windows" >&2; exit 1 ;;
esac

url="https://github.com/ungoogled-software/ungoogled-chromium-$platform.git"
git ls-remote --exit-code --tags "$url" "refs/tags/$tag" >/dev/null \
  || { echo "tag $tag not found in $url" >&2; exit 1; }

git clone --depth 1 --branch "$tag" --recurse-submodules --shallow-submodules "$url" "$dest"

got="$(cat "$dest/ungoogled-chromium/chromium_version.txt")-$(cat "$dest/ungoogled-chromium/revision.txt")"
if [ "$got" != "$UC_VERSION" ]; then
  echo "platform repo $tag carries ungoogled-chromium $got, expected $UC_VERSION" >&2
  exit 1
fi
