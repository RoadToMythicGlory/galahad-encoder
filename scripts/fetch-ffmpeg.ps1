# Downloads a static FFmpeg build and places ffmpeg.exe where Tauri bundles it.
#
# Run from the galahad-encoder directory:
#   pwsh ./scripts/fetch-ffmpeg.ps1
#
# The binary is git-ignored (src-tauri/binaries) and embedded into the installer
# via the `resources` entry in tauri.conf.json, so players never install FFmpeg.

$ErrorActionPreference = "Stop"

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $here
$dest = Join-Path $root "src-tauri/binaries"
$zip = Join-Path $env:TEMP "ffmpeg-galahad.zip"
$extract = Join-Path $env:TEMP "ffmpeg-galahad"

# gyan.dev publishes redistributable Windows builds with SRT + NVENC/QSV/AMF.
$url = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"

Write-Host "Downloading FFmpeg from $url ..."
Invoke-WebRequest -Uri $url -OutFile $zip

Write-Host "Extracting ..."
if (Test-Path $extract) { Remove-Item -Recurse -Force $extract }
Expand-Archive -Path $zip -DestinationPath $extract

$ffmpeg = Get-ChildItem -Path $extract -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
if (-not $ffmpeg) { throw "ffmpeg.exe not found in archive" }

New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item -Force $ffmpeg.FullName (Join-Path $dest "ffmpeg.exe")

Write-Host "FFmpeg ready at $dest/ffmpeg.exe"
Write-Host "License notices: keep the build's LICENSE/README alongside distribution."
