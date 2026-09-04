param(
    [switch]$PackageOnly,
    [switch]$SkipTests
)


$WindowsDepotId = "5103231"
$SteamAppId = "5103230"
$SteamBranch = "candidate"

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)][string]$Program,
        [Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments
    )

    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Program exited with code $LASTEXITCODE"
    }
}

# Windows PowerShell 5.1 runs on .NET Framework, which predates
# System.IO.Path.GetRelativePath. The release runner only needs paths beneath
# one already-validated staging root, so use a compatible prefix calculation.
function Get-StageRelativePath {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$FullPath
    )

    $RootPrefix = [System.IO.Path]::GetFullPath($Root).TrimEnd('\') + '\'
    $ResolvedPath = [System.IO.Path]::GetFullPath($FullPath)
    if (-not $ResolvedPath.StartsWith($RootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "File is outside the Steam staging root: $ResolvedPath"
    }
    return $ResolvedPath.Substring($RootPrefix.Length).Replace('\', '/')
}

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$SteamRoot = Join-Path $RepoRoot "target\steam"
$StageRoot = Join-Path $SteamRoot "windows-content"
$VdfRoot = Join-Path $SteamRoot "steamcmd"
$PreviousLocation = Get-Location

try {
    Set-Location $RepoRoot

    if ([string]::IsNullOrWhiteSpace($env:CHEMGAME_ASSET_KEY) -or
        $env:CHEMGAME_ASSET_KEY -notmatch '^[0-9A-Fa-f]{64}$') {
        throw "Set CHEMGAME_ASSET_KEY to a stable 64-character hexadecimal key before releasing."
    }
    if (Test-Path -LiteralPath $StageRoot) {
        $ResolvedStage = (Resolve-Path -LiteralPath $StageRoot).Path
        $AllowedRoot = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot "target\steam"))
        if (-not $ResolvedStage.StartsWith($AllowedRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
            $ResolvedStage -eq $AllowedRoot) {
            throw "Refusing to clear unexpected staging path: $ResolvedStage"
        }
        Remove-Item -LiteralPath $ResolvedStage -Recurse -Force
    }
    New-Item -ItemType Directory -Path $StageRoot -Force | Out-Null

    if (-not $SkipTests) {
        Invoke-Checked cargo test --workspace
    }
    Invoke-Checked cargo build --release --bin chemgame --features packed-assets
    Invoke-Checked -Program cargo -Arguments @(
        "run",
        "--release",
        "--bin",
        "pack-assets",
        "--",
        "--output",
        $StageRoot
    )

    $ReleaseRoot = Join-Path $RepoRoot "target\release"
    foreach ($RequiredFile in @("chemgame.exe", "steam_api64.dll")) {
        $Source = Join-Path $ReleaseRoot $RequiredFile
        if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
            throw "Required release file was not built: $Source"
        }
        Copy-Item -LiteralPath $Source -Destination (Join-Path $StageRoot $RequiredFile)
    }

    $AllowedRootFiles = @(
        "chemgame.exe",
        "steam_api64.dll",
        "CREDITS.md",
        "content_00.cgp",
        "content_01.cgp",
        "content_02.cgp"
    )
    $Unexpected = Get-ChildItem -LiteralPath $StageRoot -File -Recurse | Where-Object {
        $Relative = Get-StageRelativePath -Root $StageRoot -FullPath $_.FullName
        if ($Relative.StartsWith("assets/sounds/", [System.StringComparison]::Ordinal)) {
            return $_.Extension -ne ".ogg"
        }
        return $Relative -notin $AllowedRootFiles
    }
    if ($Unexpected) {
        throw "Unexpected files entered the Steam stage: $($Unexpected.FullName -join ', ')"
    }
    $LooseSourceFiles = Get-ChildItem -LiteralPath $StageRoot -File -Recurse | Where-Object {
        $_.Extension -in @(".py", ".pyc", ".blend", ".blend1", ".ron", ".map", ".glb", ".png", ".json", ".svg")
    }
    if ($LooseSourceFiles) {
        throw "Protected or authoring files were staged loose: $($LooseSourceFiles.FullName -join ', ')"
    }

    $SoundCount = (Get-ChildItem -LiteralPath (Join-Path $StageRoot "assets\sounds") -Filter "*.ogg" -File -Recurse).Count
    if ($SoundCount -ne 68) {
        throw "Expected 68 credited public sounds, but staged $SoundCount. Update CREDITS.md and the release audit together."
    }
    Write-Host "Windows Steam content is ready at $StageRoot ($SoundCount public sounds; CREDITS.md is readable)."

    if ($PackageOnly) {
        Write-Host "PackageOnly was selected; Steam upload was skipped."
        return
    }

    if ($WindowsDepotId -notmatch '^\d+$') {
        throw "Edit tools/release-steam.ps1 and replace PASTE_WINDOWS_DEPOT_ID_HERE with the numeric Windows depot ID."
    }

    $SteamCmd = $env:STEAMCMD_PATH
    if ([string]::IsNullOrWhiteSpace($SteamCmd)) {
        $SdkLocation = $env:STEAM_SDK_LOCATION
        if ([string]::IsNullOrWhiteSpace($SdkLocation)) {
            $CargoConfig = Join-Path $RepoRoot ".cargo\config.toml"
            if (Test-Path -LiteralPath $CargoConfig -PathType Leaf) {
                $CargoConfigText = Get-Content -LiteralPath $CargoConfig -Raw
                if ($CargoConfigText -match 'STEAM_SDK_LOCATION\s*=\s*\{\s*value\s*=\s*"([^"]+)"') {
                    $SdkLocation = $Matches[1]
                }
            }
        }
        if (-not [string]::IsNullOrWhiteSpace($SdkLocation)) {
            $SdkSteamCmd = Join-Path $SdkLocation "tools\ContentBuilder\builder\steamcmd.exe"
            if (Test-Path -LiteralPath $SdkSteamCmd -PathType Leaf) {
                $SteamCmd = $SdkSteamCmd
            }
        }
        if ([string]::IsNullOrWhiteSpace($SteamCmd)) {
            $SteamCmdCommand = Get-Command steamcmd.exe -ErrorAction SilentlyContinue
            if ($null -ne $SteamCmdCommand) {
                $SteamCmd = $SteamCmdCommand.Source
            }
        }
        if ([string]::IsNullOrWhiteSpace($SteamCmd)) {
            throw "Set STEAMCMD_PATH to steamcmd.exe, install it in the Steamworks SDK, or put it on PATH."
        }
    }
    if (-not (Test-Path -LiteralPath $SteamCmd -PathType Leaf)) {
        throw "steamcmd.exe was not found at STEAMCMD_PATH: $SteamCmd"
    }

    $SteamUser = $env:STEAM_USERNAME
    if ([string]::IsNullOrWhiteSpace($SteamUser)) {
        $SteamUser = Read-Host "Steam build account username"
    }

    New-Item -ItemType Directory -Path $VdfRoot -Force | Out-Null
    $BuildOutput = Join-Path $SteamRoot "build-output"
    New-Item -ItemType Directory -Path $BuildOutput -Force | Out-Null
    $ContentRootForVdf = $StageRoot.Replace('\', '/')
    $BuildOutputForVdf = $BuildOutput.Replace('\', '/')
    $DepotVdf = Join-Path $VdfRoot "depot_windows.vdf"
    $AppVdf = Join-Path $VdfRoot "app_build.vdf"

    $DepotVdfText = @"
"DepotBuild"
{
    "DepotID" "$WindowsDepotId"
    "ContentRoot" "$ContentRootForVdf"
    "FileMapping"
    {
        "LocalPath" "*"
        "DepotPath" "."
        "Recursive" "1"
    }
}
"@
    [System.IO.File]::WriteAllText(
        $DepotVdf,
        $DepotVdfText,
        [System.Text.UTF8Encoding]::new($false)
    )

    $DepotVdfForApp = $DepotVdf.Replace('\', '/')
    $AppVdfText = @"
"AppBuild"
{
    "AppID" "$SteamAppId"
    "Desc" "ChemGame Windows $((Get-Date).ToString('yyyy-MM-dd HH:mm:ss'))"
    "BuildOutput" "$BuildOutputForVdf"
    "ContentRoot" "$ContentRootForVdf"
    "SetLive" "$SteamBranch"
    "Preview" "0"
    "Depots"
    {
        "$WindowsDepotId" "$DepotVdfForApp"
    }
}
"@
    [System.IO.File]::WriteAllText(
        $AppVdf,
        $AppVdfText,
        [System.Text.UTF8Encoding]::new($false)
    )

    Write-Host "Uploading App $SteamAppId / Windows depot $WindowsDepotId to the '$SteamBranch' branch."
    Write-Host "SteamCMD may request the build account password or Steam Guard code; neither is stored by this script."
    Invoke-Checked $SteamCmd +login $SteamUser +run_app_build $AppVdf +quit
    Write-Host "Steam upload completed and was assigned to '$SteamBranch'. Public/default promotion remains manual."
}
finally {
    Set-Location $PreviousLocation
}
