# Fetches the FFmpeg binary that Aurevm Video Cleaner bundles as a Tauri sidecar.
# The binary is not in git; run this once after cloning.
#
# Pinned to an immutable BtbN autobuild tag and checked by SHA-256, so every
# machine and every release build ships byte-identical FFmpeg.

$ErrorActionPreference = "Stop"

$Version   = "n8.1.2-44-g7c533d0f86"
$Tag       = "autobuild-2026-08-24-13-10"
$Asset     = "ffmpeg-$Version-win64-lgpl-8.1.zip"
$Url       = "https://github.com/BtbN/FFmpeg-Builds/releases/download/$Tag/$Asset"
$ZipSha    = "5CA909AC2A46635BA4F21E5C04861825132FCF8B8C263B20793D79933E2DA5D1"
$FfmpegSha = "5346A1DAAC36A23B4797E33E5C15E0D477E88CBD24B947F288C8607DF89CB850"

$Root    = Split-Path -Parent $PSScriptRoot
$BinDir  = Join-Path $Root "src-tauri\binaries"
$Target  = Join-Path $BinDir "ffmpeg-x86_64-pc-windows-msvc.exe"
$License = Join-Path $Root "src-tauri\FFMPEG-LICENSE.txt"

function Sha256($path) { (Get-FileHash -Algorithm SHA256 -Path $path).Hash }

if ((Test-Path $Target) -and (Sha256 $Target) -eq $FfmpegSha) {
  Write-Host "ffmpeg $Version already present and verified."
  exit 0
}

New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("aurevm-ffmpeg-" + [guid]::NewGuid())
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

  Expand-Archive -Path $zip -DestinationPath $work -Force
  $src = Get-ChildItem -Path $work -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
  if (-not $src) { throw "ffmpeg.exe not found inside $Asset" }

  $got = Sha256 $src.FullName
  if ($got -ne $FfmpegSha) {
    throw "SHA-256 mismatch for ffmpeg.exe: expected $FfmpegSha, got $got"
  }

  Copy-Item $src.FullName $Target -Force

  $lic = Get-ChildItem -Path $work -Recurse -Filter "LICENSE.txt" | Select-Object -First 1
  if ($lic) { Copy-Item $lic.FullName $License -Force }

  Write-Host "Installed ffmpeg $Version -> $Target"
}
finally {
  Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
