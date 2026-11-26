#!/usr/bin/env bash
# Builds Cohelm.app and a disk image from a browser build and the Helper.
#
#   package-macos.sh <browser.app> <helper-binary> <version> <out-dir>
#
# <browser.app> is the ungoogled-chromium build (Chromium.app), or Chrome for
# Testing as a stand-in for the packaging check. The Helper is the app's main
# program and starts the browser inside it (ADR 0006):
#
#   Cohelm.app/Contents/MacOS/Cohelm            the Helper
#   Cohelm.app/Contents/Resources/
#     Cohelm Browser.app                        the browser, unchanged
#     extension/                                Cohelm's built-in extension
#     devtools-mcp/                             Chrome DevTools MCP
#     node/bin/node                             Node, to run it
#
# Downloads Node and Chrome DevTools MCP at the versions pinned in
# runtime.env and checks them against the pinned checksums.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
# shellcheck disable=SC1091 # pinned versions, beside this script
source "$here/runtime.env"

browser_app="${1:?browser .app}"
helper="${2:?Helper binary}"
version="${3:?version}"
out="${4:?output folder}"

test -d "$browser_app/Contents/MacOS" || { echo "not an app: $browser_app" >&2; exit 1; }
test -x "$helper" || { echo "no Helper at $helper" >&2; exit 1; }

case "$(lipo -archs "$helper")" in
  arm64) arch=arm64 node_arch=arm64 node_sha="$NODE_SHA256_DARWIN_ARM64" ;;
  x86_64) arch=x64 node_arch=x64 node_sha="$NODE_SHA256_DARWIN_X64" ;;
  *) echo "build the Helper for one architecture" >&2; exit 1 ;;
esac

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Node.
node_name="node-v$NODE_VERSION-darwin-$node_arch"
curl -fsSL -o "$work/node.tar.gz" "https://nodejs.org/dist/v$NODE_VERSION/$node_name.tar.gz"
echo "$node_sha  $work/node.tar.gz" | shasum -a 256 -c -
tar -xzf "$work/node.tar.gz" -C "$work"

# Chrome DevTools MCP. The npm package has no dependencies of its own.
curl -fsSL -o "$work/devtools-mcp.tgz" \
  "https://registry.npmjs.org/chrome-devtools-mcp/-/chrome-devtools-mcp-$DEVTOOLS_MCP_VERSION.tgz"
got="sha512-$(openssl dgst -sha512 -binary "$work/devtools-mcp.tgz" | openssl base64 -A)"
test "$got" = "$DEVTOOLS_MCP_INTEGRITY" || { echo "Chrome DevTools MCP checksum mismatch" >&2; exit 1; }
mkdir "$work/devtools-mcp"
tar -xzf "$work/devtools-mcp.tgz" -C "$work/devtools-mcp" --strip-components 1

mkdir -p "$out"
app="$out/Cohelm.app"
rm -rf "$app"
contents="$app/Contents"
resources="$contents/Resources"
mkdir -p "$contents/MacOS" "$resources/node/bin"

cp "$helper" "$contents/MacOS/Cohelm"
min_macos="$(plutil -extract LSMinimumSystemVersion raw "$browser_app/Contents/Info.plist" 2>/dev/null || echo 12.0)"
sed -e "s/@VERSION@/$version/g" "$here/Info.plist" > "$contents/Info.plist"
plutil -replace LSMinimumSystemVersion -string "$min_macos" "$contents/Info.plist"

# The browser keeps its own bundle and signature.
ditto "$browser_app" "$resources/Cohelm Browser.app"
ditto "$root/extension" "$resources/extension"
rm -f "$resources/extension/README.md"
ditto "$work/devtools-mcp" "$resources/devtools-mcp"
cp "$work/$node_name/bin/node" "$resources/node/bin/node"
cp "$work/$node_name/LICENSE" "$resources/node/LICENSE"
cp "$root/LICENSE" "$root/NOTICE" "$resources/"

# Unsigned beta (ADR 0003): sign the outer app ad hoc, so its parts hang
# together, without touching the browser's own signature.
codesign --force --sign - "$app"
codesign --verify --strict "$app"

dmg="$out/Cohelm-$version-macos-$arch.dmg"
rm -f "$dmg"
hdiutil create -quiet -volname Cohelm -srcfolder "$app" -format UDZO "$dmg"
echo "$app"
echo "$dmg"
