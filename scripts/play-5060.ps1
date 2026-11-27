# VIctoryUnified-DLSS launcher for a laptop with an RTX 5060 8 GB (hybrid graphics): the same as
# play-nvidia.ps1, with the card picked by name and Windows told to use the high-performance GPU for the
# exe, and the log written to %LOCALAPPDATA%\VIctory\logs (-Log).
#   scripts\play-5060.ps1 -Exe 'C:\path\to\game_scarlett_master.exe'
# Extra play-nvidia.ps1 switches go through: -NoDlss, -Quality, -Build release, -More @{...}
# Run Get-NVIDIA-DLSS.cmd once first (DLSS and frame generation files).
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$Adapter = '5060',
    [Parameter(ValueFromRemainingArguments)]$Rest
)
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

# Hybrid laptops: Windows' default card is often the integrated GPU. Ask for the high-performance one for
# every exe this launcher can start (keys for files that do not exist are harmless).
if (-not $env:VICTORY_NO_GPU_PREF) {
    try {
        $k = 'HKCU:\Software\Microsoft\DirectX\UserGpuPreferences'
        if (-not (Test-Path $k)) { New-Item -Path $k -Force | Out-Null }
        foreach ($b in 'diag', 'release') {
            foreach ($n in 'victory.exe', 'victory_sm.exe') {
                Set-ItemProperty -Path $k -Name (Join-Path $root "target\$b\$n") -Value 'GpuPreference=2;'
            }
        }
    } catch { Write-Host "Could not set the GPU preference: $_" }
}

Write-Host 'VIctoryUnified-DLSS by TheUnrealZaka | github.com/TheUnrealZaka/VIctoryUnified-DLSS'
& (Join-Path $PSScriptRoot 'play-nvidia.ps1') -Exe $Exe -Adapter $Adapter -Log @Rest
