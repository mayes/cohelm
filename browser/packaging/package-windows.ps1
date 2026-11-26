# Builds the Cohelm installer for Windows from a browser build and the Helper.
#
#   package-windows.ps1 -Browser <folder with chrome.exe> -Helper <cohelm-helper.exe> -Version <x.y.z> -Out <folder>
#
# -Browser is the ungoogled-chromium build, or Chrome for Testing as a
# stand-in for the packaging check. Cohelm installs for the current user, with
# the Helper as Cohelm.exe and the browser beside it (ADR 0006):
#
#   Cohelm.exe       the Helper
#   browser\         the browser, unchanged
#   extension\       Cohelm's built-in extension
#   devtools-mcp\    Chrome DevTools MCP
#   node\node.exe    Node, to run it
#
# Downloads Node and Chrome DevTools MCP at the versions pinned in
# runtime.env, checks them against the pinned checksums, and builds the
# installer with Inno Setup (iscc on PATH, or in its usual folder).
param(
    [Parameter(Mandatory)] [string] $Browser,
    [Parameter(Mandatory)] [string] $Helper,
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $Out
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$here = $PSScriptRoot
$root = (Resolve-Path (Join-Path $here '..\..')).Path
$pins = @{}
foreach ($line in Get-Content (Join-Path $here 'runtime.env')) {
    if ($line -match '^\s*([A-Z0-9_]+)=(.*)$') { $pins[$Matches[1]] = $Matches[2].Trim() }
}

if (-not (Test-Path (Join-Path $Browser 'chrome.exe'))) { throw "no chrome.exe in $Browser" }
if (-not (Test-Path $Helper)) { throw "no Helper at $Helper" }

$work = Join-Path ([System.IO.Path]::GetTempPath()) ("cohelm-package-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
try {
    # Node.
    $nodeName = "node-v$($pins.NODE_VERSION)-win-x64"
    $nodeZip = Join-Path $work 'node.zip'
    Invoke-WebRequest -Uri "https://nodejs.org/dist/v$($pins.NODE_VERSION)/$nodeName.zip" -OutFile $nodeZip
    if ((Get-FileHash -Algorithm SHA256 $nodeZip).Hash.ToLower() -ne $pins.NODE_SHA256_WIN_X64) {
        throw 'Node checksum mismatch'
    }
    Expand-Archive -Path $nodeZip -DestinationPath $work

    # Chrome DevTools MCP. The npm package has no dependencies of its own.
    $tgz = Join-Path $work 'devtools-mcp.tgz'
    Invoke-WebRequest -Uri "https://registry.npmjs.org/chrome-devtools-mcp/-/chrome-devtools-mcp-$($pins.DEVTOOLS_MCP_VERSION).tgz" -OutFile $tgz
    $sha512 = [System.Security.Cryptography.SHA512]::Create()
    $stream = [System.IO.File]::OpenRead($tgz)
    try { $got = 'sha512-' + [Convert]::ToBase64String($sha512.ComputeHash($stream)) } finally { $stream.Dispose() }
    if ($got -ne $pins.DEVTOOLS_MCP_INTEGRITY) { throw 'Chrome DevTools MCP checksum mismatch' }
    $devtools = Join-Path $work 'devtools-mcp'
    New-Item -ItemType Directory -Path $devtools | Out-Null
    # Windows' own tar reads npm tarballs.
    & "$env:SystemRoot\System32\tar.exe" -xzf $tgz -C $devtools --strip-components 1
    if ($LASTEXITCODE -ne 0) { throw 'could not unpack Chrome DevTools MCP' }

    $stage = Join-Path $work 'stage'
    New-Item -ItemType Directory -Path $stage, (Join-Path $stage 'node') | Out-Null
    Copy-Item $Helper (Join-Path $stage 'Cohelm.exe')
    Copy-Item -Recurse $Browser (Join-Path $stage 'browser')
    Copy-Item -Recurse (Join-Path $root 'extension') (Join-Path $stage 'extension')
    Remove-Item (Join-Path $stage 'extension\README.md')
    Copy-Item -Recurse $devtools (Join-Path $stage 'devtools-mcp')
    Copy-Item (Join-Path $work "$nodeName\node.exe"), (Join-Path $work "$nodeName\LICENSE") (Join-Path $stage 'node')
    Copy-Item (Join-Path $root 'LICENSE'), (Join-Path $root 'NOTICE') $stage

    $iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
    if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
    if (-not (Test-Path $iscc)) { throw 'Inno Setup 6 is needed to build the installer' }
    New-Item -ItemType Directory -Force -Path $Out | Out-Null
    & $iscc /Q "/DAppVersion=$Version" "/DStage=$stage" "/O$((Resolve-Path $Out).Path)" (Join-Path $here 'cohelm.iss')
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup failed' }
    Join-Path $Out "Cohelm-$Version-windows-x64-setup.exe"
}
finally {
    Remove-Item -Recurse -Force $work
}
