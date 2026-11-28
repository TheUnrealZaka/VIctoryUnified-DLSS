# Summarises a play log: the fps the game runs at before frame generation multiplies it.
# Use it to judge a change by numbers: play the same stretch for about 5 minutes, then run
#   .\scripts\fps-summary.ps1                 newest log in %LOCALAPPDATA%\victory\logs
#   .\scripts\fps-summary.ps1 <log file>      a log you name
# One sample is 5 seconds of play. "under 15 fps" is the share of samples below 15 fps.
param([string]$Log)
if (-not $Log) {
    $Log = (Get-ChildItem "$env:LOCALAPPDATA\victory\logs\victory-nvidia-*.log" | Sort-Object LastWriteTime | Select-Object -Last 1).FullName
}
Write-Host "Log: $Log"
$v = [System.Collections.Generic.List[double]]::new()
foreach ($l in [System.IO.File]::ReadLines($Log)) {
    $i = $l.IndexOf(' fps, ')
    if ($i -lt 0) { continue }
    $m = [regex]::Match($l.Substring(0, $i), '([0-9.]+)$')
    if ($m.Success) { $v.Add([double]::Parse($m.Groups[1].Value, [cultureinfo]::InvariantCulture)) }
}
if ($v.Count -eq 0) { Write-Host 'No fps lines found.'; return }
$s = $v | Sort-Object
function P($p) { $s[[math]::Min($s.Count - 1, [int]($s.Count * $p))] }
$lowShare = [math]::Round(100 * ($v | Where-Object { $_ -lt 15 }).Count / $v.Count)
"{0} samples (5 s each)  median {1}  mean {2}  p10 {3}  p90 {4}  under 15 fps: {5}% of the time" -f `
    $v.Count, (P 0.5), [math]::Round(($v | Measure-Object -Average).Average, 1), (P 0.1), (P 0.9), $lowShare
