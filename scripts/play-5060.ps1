# VIctoryUnified-DLSS launcher for a hybrid-graphics laptop with an RTX 5060 8 GB.
# It makes Windows use the high-performance GPU for the exe (without this the integrated GPU is used and the
# game crawls), then starts play-foxhound.ps1 with Series S (8 GB of video memory) and DLSS frame generation.
#   .\scripts\play-5060.ps1 -Exe 'C:\path\to\game_scarlett_master.exe'
# Anything else is passed to play-foxhound.ps1, which comes later and wins: -Dlss performance, -FrameGenX 2,
# -Preload (first run: builds every shader first), -More @{...}. VICTORY_NO_GPU_PREF=1 skips the GPU preference.
# Run Get-NVIDIA-DLSS.cmd once first. Build first if there is no target\release\victory.exe (the launcher
# builds it with cargo if it is missing).
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(ValueFromRemainingArguments)]$Rest
)
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if (-not $env:VICTORY_NO_GPU_PREF) {
    try {
        $k = 'HKCU:\Software\Microsoft\DirectX\UserGpuPreferences'
        if (-not (Test-Path $k)) { New-Item -Path $k -Force | Out-Null }
        foreach ($b in 'diag', 'release') {
            Set-ItemProperty -Path $k -Name (Join-Path $root "target\$b\victory.exe") -Value 'GpuPreference=2;'
        }
        Write-Host 'Windows GPU preference: high performance for victory.exe'
    } catch { Write-Host "Could not set the GPU preference: $_" }
}
Write-Host 'VIctoryUnified-DLSS by TheUnrealZaka | github.com/TheUnrealZaka/VIctoryUnified-DLSS'
& (Join-Path $PSScriptRoot 'play-foxhound.ps1') -Exe $Exe -SeriesS -FrameGen @Rest
