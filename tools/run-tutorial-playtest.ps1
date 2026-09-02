param([string]$Lesson='', [ValidateSet('dx12','vulkan')][string]$Backend='dx12', [switch]$Lifecycle)
$ErrorActionPreference = 'Stop'
$workspace = Split-Path $PSScriptRoot -Parent
$executable = Join-Path $workspace 'target/debug/chemgame.exe'
if (-not (Test-Path -LiteralPath $executable)) { throw 'Run cargo build first.' }
$started = Get-Date
$output = Join-Path $workspace 'target/tutorial-playtest'
$run = $started.ToString('yyyyMMdd-HHmmss')
New-Item -ItemType Directory -Force -Path "$output/$run/appdata", "$output/$run/roaming" | Out-Null
$previous = @{}
foreach ($key in @('LOCALAPPDATA', 'APPDATA', 'BEVY_ASSET_ROOT', 'RUST_LOG', 'WGPU_BACKEND', 'RUST_BACKTRACE')) {
    $previous[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
try {
    $env:LOCALAPPDATA = "$output/$run/appdata"
    $env:APPDATA = "$output/$run/roaming"
    $env:BEVY_ASSET_ROOT = $workspace
    $env:RUST_LOG = 'info,wgpu=warn,naga=warn'
    $env:WGPU_BACKEND = $Backend
    $env:RUST_BACKTRACE = '1'
    $launchArgs = @(if ($Lifecycle) { '--tutorial-lifecycle-playtest' } else { '--tutorial-playtest' })
    if ($Lesson) { $launchArgs += @('--practice', $Lesson) }
    $process = Start-Process -FilePath $executable -ArgumentList $launchArgs `
        -WorkingDirectory $workspace -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput "$output/stdout.log" -RedirectStandardError "$output/stderr.log"
    Set-Content -LiteralPath "$output/process-id.txt" -Value $process.Id
} finally {
    foreach ($key in $previous.Keys) {
        [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process')
    }
}
$deadline = [DateTime]::UtcNow.AddSeconds(240)
while (-not $process.HasExited) {
    if ([DateTime]::UtcNow -gt $deadline) {
        $process | Stop-Process
        throw 'The training scenario did not finish. Inspect target/tutorial-playtest logs.'
    }
    Start-Sleep -Seconds 1
}
$reportName = if ($Lifecycle) { 'lifecycle-result.txt' } else { 'result.txt' }
$report = Get-Item -LiteralPath "$output/$reportName" -ErrorAction SilentlyContinue
if ($null -eq $report -or $report.LastWriteTime -lt $started) {
    throw "The game did not write a fresh report (exit $($process.ExitCode)). Inspect target/tutorial-playtest logs."
}
$result = Get-Content -LiteralPath $report.FullName -Raw
Write-Output $result
if ($result -notmatch '^PASS') { throw 'Rendered training verification failed.' }

$label = if ($Lifecycle) { 'lifecycle' } elseif ($Lesson) { $Lesson } else { 'core' }
Copy-Item -LiteralPath $report.FullName -Destination (Join-Path $output "verified-$label.txt")
