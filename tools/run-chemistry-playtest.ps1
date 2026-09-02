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
        $output = Join-Path $workspace "target/chemistry-playtest/$role"
        New-Item -ItemType Directory -Force -Path "$output/$run/appdata", "$output/$run/roaming" | Out-Null
        $env:LOCALAPPDATA = "$output/$run/appdata"
        $env:APPDATA = "$output/$run/roaming"
        $env:BEVY_ASSET_ROOT = $workspace
        $env:RUST_LOG = 'info,wgpu=warn,naga=warn'
        $launchArgs = if ($role -eq 'client') { @('--join', '127.0.0.1', '--chemistry-playtest') }
            elseif ($role -eq 'host') { @('--host', '--chemistry-playtest') }
            else { @('--solo', '--chemistry-playtest') }
        $process = Start-Process -FilePath $executable -ArgumentList $launchArgs `
            -WorkingDirectory $workspace -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput "$output/stdout.log" -RedirectStandardError "$output/stderr.log"
        $processes += $process
        Write-Output "$role process: $($process.Id)"
        if ($role -eq 'host') {
            $readyDeadline = [DateTime]::UtcNow.AddSeconds(45)
            do {
                Start-Sleep -Seconds 1
                $ready = Get-Item -LiteralPath "$output/sandbox-ready.txt" -ErrorAction SilentlyContinue
                $seeded = $null -ne $ready -and $ready.LastWriteTime -ge $started
            } until ($seeded -or $process.HasExited -or [DateTime]::UtcNow -gt $readyDeadline)
            if (-not $seeded) { throw 'Host sandbox fixture did not resolve before late join.' }
        }
    }
} catch {
    $processes | Where-Object { -not $_.HasExited } | Stop-Process
    throw
} finally {
    foreach ($key in $previous.Keys) {
        [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process')
    }
}

$deadline = [DateTime]::UtcNow.AddSeconds(180)
while (@($processes | Where-Object { -not $_.HasExited }).Count -gt 0) {
    if ([DateTime]::UtcNow -gt $deadline) {
        $processes | Where-Object { -not $_.HasExited } | Stop-Process
        throw 'The rendered scenario did not finish. Inspect its logs.'
    }
    Start-Sleep -Seconds 1
}
foreach ($role in $roles) {
    Write-Output "${role}:"
    $report = Get-Item -LiteralPath (Join-Path $workspace "target/chemistry-playtest/$role/result.txt")
    if ($report.LastWriteTime -lt $started) { throw "$role did not write a fresh report." }
    $result = Get-Content -LiteralPath $report.FullName -Raw
    if ($result -notmatch 'PASS\r?\nProducts: 3\r?\nReport shared: true') {
        throw "$role did not complete the chemistry scenario: $result"
    }
    if ($result -notmatch 'Sandbox chemistry: merged Ash and intact collapsed body verified') { throw "$role did not verify sandbox chemistry." }
    Write-Output $result
}
