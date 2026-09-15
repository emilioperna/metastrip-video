# Fetches the FFmpeg binaries that this app bundles as Tauri sidecars.
# They are not in git; run this once after cloning.
#
# Pinned to an immutable BtbN autobuild tag and checked by SHA-256, so every
# machine and every release build ships byte-identical binaries. `ffmpeg.exe` and
# `ffprobe.exe` are extracted from the same archive, so the two can never drift
# to different FFmpeg builds.
#
# BtbN keeps daily autobuilds for about two weeks and one build per month after
# that. Pin an end-of-month tag: a mid-month one is pruned upstream within weeks
# and every clean clone and CI run then fails to fetch it.

$ErrorActionPreference = "Stop"

$Version = "n8.1.2-50-g1a748fe2cd"
$Tag     = "autobuild-2026-08-31-13-27"
$Asset   = "ffmpeg-$Version-win64-lgpl-8.1.zip"
$Url     = "https://github.com/BtbN/FFmpeg-Builds/releases/download/$Tag/$Asset"
$ZipSha  = "F6274BBD9C247F9E90C1BBED066B03ED4A3907CECE2FB91BE6DD352393936365"

$Root    = Split-Path -Parent $PSScriptRoot
$BinDir  = Join-Path $Root "src-tauri\binaries"
$License = Join-Path $Root "src-tauri\FFMPEG-LICENSE.txt"

# Every sidecar bundled by `externalBin` in src-tauri/tauri.conf.json, with the
# SHA-256 of that executable as published inside the pinned archive. Tauri
# resolves an `externalBin` entry by appending the target triple, which is what
# the installed names carry.
$Sidecars = @(
  @{
    Name   = "ffmpeg.exe"
    Target = "ffmpeg-x86_64-pc-windows-msvc.exe"
    Sha    = "9C60DA6C0B083110D59084EA39F60AE149AA3E031C3B4BB4F573FAFA1C1E7CEA"
  },
  @{
    Name   = "ffprobe.exe"
    Target = "ffprobe-x86_64-pc-windows-msvc.exe"
    Sha    = "67176FA62F89F94C3BCD379FD05677A25651569A2EB8880EC2194E62C82BE412"
  }
)

# Hashing and unzipping go straight to .NET rather than through Get-FileHash and
# Expand-Archive. Those live in modules that have to be autoloaded, and on the
# GitHub runner npm launches this under powershell.exe from a pwsh 7 parent, which
# leaves PSModulePath pointing at PowerShell 7's module directories; Windows
# PowerShell then cannot find its own modules and Get-FileHash is simply missing.
# The .NET types are part of the framework and need no module at all.
function Sha256($path) {
  $stream = [System.IO.File]::OpenRead($path)
  try {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
      return [System.BitConverter]::ToString($sha.ComputeHash($stream)).Replace("-", "")
    }
    finally { $sha.Dispose() }
  }
  finally { $stream.Dispose() }
}

function Expand-Zip($zipPath, $destination) {
  Add-Type -AssemblyName System.IO.Compression.FileSystem
  [System.IO.Compression.ZipFile]::ExtractToDirectory($zipPath, $destination)
}

function Test-Sidecar($sidecar) {
  $path = Join-Path $BinDir $sidecar.Target
  return (Test-Path $path) -and (Sha256 $path) -eq $sidecar.Sha
}

if (@($Sidecars | Where-Object { -not (Test-Sidecar $_) }).Count -eq 0) {
  Write-Host "ffmpeg $Version already present and verified ($($Sidecars.Count) sidecars)."
  exit 0
}

New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("ffmpeg-sidecar-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $work | Out-Null

try {
  $zip = Join-Path $work $Asset
  Write-Host "Downloading $Asset (~146 MB)..."
  $old = $ProgressPreference; $ProgressPreference = "SilentlyContinue"
  Invoke-WebRequest -Uri $Url -OutFile $zip -UseBasicParsing
  $ProgressPreference = $old

  $got = Sha256 $zip
  if ($got -ne $ZipSha) {
    throw "SHA-256 mismatch for ${Asset}: expected $ZipSha, got $got"
  }
  Write-Host "Archive checksum OK."

  Expand-Zip $zip $work

  # Each executable is verified against its own pinned hash before it is copied,
  # so a correct archive containing an unexpected binary still fails here.
  foreach ($sidecar in $Sidecars) {
    $src = Get-ChildItem -Path $work -Recurse -Filter $sidecar.Name | Select-Object -First 1
    if (-not $src) { throw "$($sidecar.Name) not found inside $Asset" }

    $got = Sha256 $src.FullName
    if ($got -ne $sidecar.Sha) {
      throw "SHA-256 mismatch for $($sidecar.Name): expected $($sidecar.Sha), got $got"
    }

    Copy-Item $src.FullName (Join-Path $BinDir $sidecar.Target) -Force
    Write-Host "Installed $($sidecar.Name) $Version -> $($sidecar.Target)"
  }

  $lic = Get-ChildItem -Path $work -Recurse -Filter "LICENSE.txt" | Select-Object -First 1
  if ($lic) { Copy-Item $lic.FullName $License -Force }
}
finally {
  Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
