# Builds the optional HEIF/HEIC decoder for Windows x64 (libheif + libde265, LGPL-3.0) and
# writes it to native/windows-x64/libheif/ together with manifest.json (file names, sizes,
# SHA-256). The app embeds that manifest at build time and only installs DLLs matching it,
# so after rebuilding, commit the folder and rebuild the app.
#
# Needs Visual Studio (Build Tools) with the C++ workload and git. vcpkg is cloned into
# target/native/vcpkg at a fixed commit. Only the libde265 decoder is enabled: the default
# "hevc" feature pulls in the x265 encoder, which is GPL.
#
#   powershell -ExecutionPolicy Bypass -File scripts/build-libheif.ps1

$ErrorActionPreference = "Stop"
$VcpkgCommit = "bd8298ae219367a606c74c6c25dc50cc6d66eca8"
$Triplet = "x64-windows-release"

$root = Split-Path -Parent $PSScriptRoot
$vcpkg = Join-Path $root "target\native\vcpkg"
$out = Join-Path $root "native\windows-x64\libheif"

if (-not (Test-Path (Join-Path $vcpkg ".git"))) {
    git clone https://github.com/microsoft/vcpkg.git $vcpkg
}
git -C $vcpkg fetch --depth 1 origin $VcpkgCommit
git -C $vcpkg checkout --quiet $VcpkgCommit
if (-not (Test-Path (Join-Path $vcpkg "vcpkg.exe"))) {
    & (Join-Path $vcpkg "bootstrap-vcpkg.bat") -disableMetrics
}

& (Join-Path $vcpkg "vcpkg.exe") install "libheif[core,libde265]:$Triplet" --clean-after-build
if ($LASTEXITCODE -ne 0) { throw "vcpkg install failed" }

$installed = Join-Path $vcpkg "installed\$Triplet"
New-Item -ItemType Directory -Force $out | Out-Null
Get-ChildItem $out -File | Remove-Item -Force
$dlls = @("heif.dll", "libde265.dll")
foreach ($d in $dlls) {
    Copy-Item (Join-Path $installed "bin\$d") $out
}
# LGPL: ship the license texts with the libraries.
Copy-Item (Join-Path $installed "share\libheif\copyright") (Join-Path $out "COPYING.libheif.txt")
Copy-Item (Join-Path $installed "share\libde265\copyright") (Join-Path $out "COPYING.libde265.txt")

$version = (Get-Content (Join-Path $vcpkg "ports\libheif\vcpkg.json") | ConvertFrom-Json).version
$files = foreach ($f in Get-ChildItem $out -File | Where-Object Name -ne "manifest.json" | Sort-Object Name) {
    [ordered]@{
        name   = $f.Name
        size   = $f.Length
        sha256 = (Get-FileHash $f.FullName -Algorithm SHA256).Hash.ToLower()
    }
}
$manifest = [ordered]@{
    version = $version
    library = "heif.dll"
    vcpkg   = $VcpkgCommit
    files   = @($files)
}
$json = $manifest | ConvertTo-Json -Depth 4
[System.IO.File]::WriteAllText((Join-Path $out "manifest.json"), $json + "`n", (New-Object System.Text.UTF8Encoding $false))
Write-Host "libheif $version -> $out"
