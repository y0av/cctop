# cctop installer for Windows — downloads the latest prebuilt binary.
#   irm https://raw.githubusercontent.com/y0av/cctop/master/install.ps1 | iex
$ErrorActionPreference = 'Stop'

$repo = 'y0av/cctop'
$installDir = if ($env:CCTOP_INSTALL_DIR) { $env:CCTOP_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'cctop' }

# Detect architecture -> release asset target triple.
$arch = $env:PROCESSOR_ARCHITECTURE
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    default {
        Write-Error "unsupported arch '$arch' — install with: cargo install --git https://github.com/$repo"
    }
}
$asset = "cctop-$target.zip"

# Resolve the latest release tag.
$rel = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'cctop-installer' }
$tag = $rel.tag_name
if (-not $tag) {
    Write-Error "could not find a release — install with: cargo install --git https://github.com/$repo"
}

$url = "https://github.com/$repo/releases/download/$tag/$asset"
Write-Host "downloading cctop $tag ($target)..."

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $zip = Join-Path $tmp $asset
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Copy-Item -Path (Join-Path $tmp 'cctop.exe') -Destination (Join-Path $installDir 'cctop.exe') -Force
    Write-Host "installed cctop -> $installDir\cctop.exe"
} finally {
    Remove-Item -Recurse -Force $tmp
}

# Add to the user PATH if it isn't already there.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $installDir) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$installDir", 'User')
    Write-Host "added $installDir to your PATH — open a new terminal, then run: cctop"
} else {
    Write-Host "run: cctop"
}
