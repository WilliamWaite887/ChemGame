param([ValidateSet('solo', 'coop')][string]$Mode = 'solo')
$ErrorActionPreference = 'Stop'
$workspace = Split-Path $PSScriptRoot -Parent
$executable = Join-Path $workspace 'target/debug/chemgame.exe'
if (-not (Test-Path -LiteralPath $executable)) { throw 'Run cargo build first.' }
$roles = if ($Mode -eq 'coop') { @('host', 'client') } else { @('solo') }
$processes = @()
$started = Get-Date
$run = $started.ToString('yyyyMMdd-HHmmss')
$previous = @{}
foreach ($key in @('LOCALAPPDATA', 'APPDATA', 'BEVY_ASSET_ROOT', 'RUST_LOG')) {
    $previous[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
try {
    foreach ($role in $roles) {
        $output = Join-Path $workspace "target/security-case-playtest/$role"
        New-Item -ItemType Directory -Force -Path "$output/$run/appdata", "$output/$run/roaming" | Out-Null
        $env:LOCALAPPDATA = "$output/$run/appdata"
        $env:APPDATA = "$output/$run/roaming"
        $env:BEVY_ASSET_ROOT = $workspace
        $env:RUST_LOG = 'info,wgpu=warn,naga=warn'
        $launchArgs = if ($role -eq 'client') { @('--join', '127.0.0.1', '--security-case-playtest') }
            elseif ($role -eq 'host') { @('--host', '--security-case-playtest') }
            else { @('--solo', '--security-case-playtest') }
        $process = Start-Process -FilePath $executable -ArgumentList $launchArgs `
            -WorkingDirectory $workspace -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput "$output/stdout.log" -RedirectStandardError "$output/stderr.log"
        $processes += $process
        Write-Output "$role process: $($process.Id)"
        if ($role -eq 'host') { Start-Sleep -Seconds 3 }
    }
} finally {
    foreach ($key in $previous.Keys) { [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process') }
}
$deadline = [DateTime]::UtcNow.AddSeconds(295)
while (@($processes | Where-Object { -not $_.HasExited }).Count -gt 0) {
    if ([DateTime]::UtcNow -gt $deadline) {
        $processes | Where-Object { -not $_.HasExited } | Stop-Process
        throw 'Security case playtest timed out. Inspect its logs.'
    }
    Start-Sleep -Seconds 1
}
foreach ($role in $roles) {
    $report = Get-Item -LiteralPath (Join-Path $workspace "target/security-case-playtest/$role/result.txt")
    if ($report.LastWriteTime -lt $started) { throw "$role did not write a fresh report." }
    $result = Get-Content -LiteralPath $report.FullName -Raw
    if ($result -notmatch 'Security physical case completed' -or $result -notmatch 'Held order resumed: true') {
        throw "$role did not complete the custody/appeal flow: $result"
    }
    Write-Output "${role}:`n$result"
}
