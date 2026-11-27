# get-streamline.ps1 - fetch the NVIDIA Streamline 2.14.16 runtime DLLs for the DLSS frame
# generation build, check them, and put them in <repo>\streamline.
#
#   cd <your VIctoryUnified-DLSS folder>
#   powershell -ExecutionPolicy Bypass -File .\scripts\get-streamline.ps1
#
# What it does:
#   1. downloads NVIDIA's official release package (about 276 MB) from
#      github.com/NVIDIA-RTX/Streamline (tag v2.14.16, the version the code was written for);
#   2. extracts ONLY these six production DLLs from bin\x64 inside it:
#      sl.interposer, sl.common, sl.dlss_g, sl.reflex, sl.pcl, nvngx_dlssg;
#   3. checks each one's SHA-256 against the values below (taken from the official package
#      on 2026-11-27) and that Windows reports a VALID Authenticode signature naming NVIDIA;
#   4. copies them to <repo>\streamline (the launcher looks there).
# It stops, and copies nothing, if any check fails.  Run from a normal (non-admin) window.
# NOT run on Windows by its author: read the output of the first run.

param(
    [string]$Dest = (Join-Path (Split-Path -Parent $PSScriptRoot) "streamline"),
    [string]$Zip  = (Join-Path $env:TEMP "streamline-sdk-v2.14.16.zip"),
    [string]$Url  = "https://github.com/NVIDIA-RTX/Streamline/releases/download/v2.14.16/streamline-sdk-v2.14.16.zip"
)

$expected = @{
    "sl.interposer.dll" = "B1D7395AEBD7B9722841D64DCF7433680020B6A8E421F851C16D5BA0C2182EDF"
    "sl.common.dll"     = "38EEA3B4209AFC455636EA372B5933D65FCC13BECEC6C84D101DCFC4D8648F75"
    "sl.dlss_g.dll"     = "3ED9538DF724993BEA8A050195A6A78B9C8C8A566D5B05ADE477AAB3C390E90F"
    "sl.reflex.dll"     = "443577E869352436FF7EC8F4D9C90418D3E95368436CE219261E0F4F07A38122"
    "sl.pcl.dll"        = "5698C38E722318681F215C9FE68E45B9BEDD51378ACC699D8EEE26E961E57770"
    "nvngx_dlssg.dll"   = "8D59D49B36410DD6B62FEF2503A5871D347415BD85D943FA0848563A95DD50C7"
}

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$ProgressPreference = "SilentlyContinue"   # the progress bar makes Invoke-WebRequest very slow

if (-not (Test-Path $Zip) -or (Get-Item $Zip).Length -lt 200MB) {
    Write-Host "Downloading $Url (about 276 MB) ..."
    Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Zip
}
Write-Host ("Package: {0} ({1:N0} bytes)" -f $Zip, (Get-Item $Zip).Length)

$stage = Join-Path $env:TEMP "sl-stage-$([guid]::NewGuid().ToString('N').Substring(0,8))"
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem
$arc = [IO.Compression.ZipFile]::OpenRead($Zip)
try {
    foreach ($name in $expected.Keys) {
        $entry = $arc.Entries | Where-Object { $_.FullName -eq "bin/x64/$name" } | Select-Object -First 1
        if (-not $entry) { throw "bin/x64/$name is not in the package" }
        [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $stage $name), $true)
    }
} finally { $arc.Dispose() }

$bad = @()
foreach ($name in $expected.Keys) {
    $f = Join-Path $stage $name
    $hash = (Get-FileHash -Algorithm SHA256 -Path $f).Hash
    $sig  = Get-AuthenticodeSignature -FilePath $f
    $who  = if ($sig.SignerCertificate) { $sig.SignerCertificate.Subject } else { "(none)" }
    $okHash = ($hash -eq $expected[$name])
    $okSig  = ($sig.Status -eq "Valid") -and ($who -match "NVIDIA")
    "{0,-18} hash:{1,-5} signature:{2,-5} {3}" -f $name, ($(if ($okHash) {"ok"} else {"BAD"})), ($(if ($okSig) {"ok"} else {"BAD"})), $who
    if (-not ($okHash -and $okSig)) { $bad += $name }
}
if ($bad.Count -gt 0) {
    Remove-Item -Recurse -Force $stage
    throw "Check failed for: $($bad -join ', '). Nothing was copied."
}

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
Copy-Item -Path (Join-Path $stage "*.dll") -Destination $Dest -Force
Remove-Item -Recurse -Force $stage
Write-Host ""
Write-Host "Done: 6 DLLs in $Dest"
Write-Host "The launcher scripts\play-nvidia-8gb.cmd now finds them and turns frame generation on."
