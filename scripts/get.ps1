# One-line installer for Job Harness (Windows PowerShell).
# Downloads the latest Windows release archive, verifies SHA256SUMS, and installs `job-harness.exe` + `jh.exe`.
$ErrorActionPreference = "Stop"

$Repo = "machineyearningengineer/job-harness"
$Prefix = if ($env:JOB_HARNESS_PREFIX) { $env:JOB_HARNESS_PREFIX } else { Join-Path $HOME ".local" }
$BinDir = Join-Path $Prefix "bin"
$Version = $env:JOB_HARNESS_VERSION

if (-not $Version) {
    $Releases = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases" -Headers @{ "User-Agent" = "job-harness-installer" }
    $Version = $Releases[0].tag_name
}
if (-not $Version) {
    throw "Could not determine latest release tag. Install with: cargo install --git https://github.com/$Repo job-harness"
}

$Archive = "job-harness-$Version-x86_64-pc-windows-msvc.zip"
$BaseUrl = "https://github.com/$Repo/releases/download/$Version"
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("job-harness-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $Tmp | Out-Null

try {
    $ZipPath = Join-Path $Tmp $Archive
    $SumsPath = Join-Path $Tmp "SHA256SUMS"
    Write-Host "Downloading $Archive ($Version)..."
    Invoke-WebRequest -Uri "$BaseUrl/$Archive" -OutFile $ZipPath
    Invoke-WebRequest -Uri "$BaseUrl/SHA256SUMS" -OutFile $SumsPath

    $Match = Get-Content $SumsPath | Where-Object { $_ -match "\s+\*?$([regex]::Escape($Archive))$" } | Select-Object -First 1
    if (-not $Match) { throw "Checksum entry for $Archive not found in SHA256SUMS." }
    $Expected = ($Match -split "\s+")[0].ToLowerInvariant()
    $Actual = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Expected -ne $Actual) { throw "SHA-256 mismatch for $Archive" }

    Expand-Archive -Path $ZipPath -DestinationPath $Tmp -Force
    $Exe = Get-ChildItem -Path $Tmp -Filter "job-harness.exe" -Recurse | Select-Object -First 1
    if (-not $Exe) { throw "job-harness.exe not found inside $Archive" }

    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    Copy-Item -Force $Exe.FullName (Join-Path $BinDir "job-harness.exe")
    Copy-Item -Force $Exe.FullName (Join-Path $BinDir "jh.exe")

    Write-Host ""
    Write-Host "Installed $(Join-Path $BinDir 'job-harness.exe') ($Version) and jh.exe."
    Write-Host "Ensure $BinDir is on your PATH, then run:"
    Write-Host '  jh setup --workspace "$HOME\Career\workspace"'
    Write-Host '  jh --root "$HOME\Career\workspace" connect mcp'
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
