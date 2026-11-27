# Downloads the Microsoft D3D12 Agility SDK (1.614.1, SDKVersion 614) into D3D12\
# at the repository root. Must match SDK_VERSION in apps/victory/src/agility.rs.
$ErrorActionPreference = "Stop"

$Version = "1.614.1"
$RepoRoot = Split-Path -Parent $PSScriptRoot
$DestDir = Join-Path $RepoRoot "D3D12"
$TempZip = Join-Path $env:TEMP "Microsoft.Direct3D.D3D12.$Version.zip"
$TempExtract = Join-Path $env:TEMP "Microsoft.Direct3D.D3D12.$Version"

$Url = "https://www.nuget.org/api/v2/package/Microsoft.Direct3D.D3D12/$Version"
Write-Host "Downloading Microsoft.Direct3D.D3D12 $Version from NuGet..."
Invoke-WebRequest -Uri $Url -OutFile $TempZip

if (Test-Path $TempExtract) {
    Remove-Item -Recurse -Force $TempExtract
}
Expand-Archive -Path $TempZip -DestinationPath $TempExtract -Force

New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
$BinDir = Join-Path $TempExtract "build\native\bin\x64"
Copy-Item -Path (Join-Path $BinDir "D3D12Core.dll") -Destination (Join-Path $DestDir "D3D12Core.dll") -Force
if (Test-Path (Join-Path $BinDir "d3d12SDKLayers.dll")) {
    Copy-Item -Path (Join-Path $BinDir "d3d12SDKLayers.dll") -Destination (Join-Path $DestDir "d3d12SDKLayers.dll") -Force
}

Remove-Item -Force $TempZip
Remove-Item -Recurse -Force $TempExtract
Write-Host "Installed D3D12 Agility SDK $Version into $DestDir"
