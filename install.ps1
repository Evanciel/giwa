# Maru installer for Windows. Needs no administrator rights.
#   irm https://raw.githubusercontent.com/<repo>/main/install.ps1 | iex
# Messages are ASCII on purpose: Windows PowerShell 5.1 misreads UTF-8 scripts without a BOM.
#
# MARU_BASE_URL     where to download from (default: the latest GitHub release of $Repo)
# MARU_INSTALL_DIR  where to put maru.exe (default: %LOCALAPPDATA%\Programs\maru)
# MARU_TARGET       release target to use instead of detecting one

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Set this when the repository goes public.
$Repo = 'OWNER/maru'

function Stop-Install($Message, $Next) {
    Write-Error "maru install failed: $Message`nnext: $Next" -ErrorAction Continue
    throw "maru install failed: $Message"
}

$base = $env:MARU_BASE_URL
if (-not $base) {
    if ($Repo -like 'OWNER/*') {
        Stop-Install 'no public release is configured for this copy of the script' 'set MARU_BASE_URL, or build from source: cargo install --locked --path crates/cli'
    }
    $base = "https://github.com/$Repo/releases/latest/download"
}
$base = $base.TrimEnd('/')

$target = $env:MARU_TARGET
if (-not $target) {
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        'AMD64' { $target = 'x86_64-pc-windows-msvc' }
        'ARM64' { $target = 'aarch64-pc-windows-msvc' }
        default { Stop-Install "unsupported architecture: $arch" 'build from source: cargo install --locked --path crates/cli' }
    }
}

$dir = $env:MARU_INSTALL_DIR
if (-not $dir) { $dir = Join-Path $env:LOCALAPPDATA 'Programs\maru' }

$archive = "maru-$target.zip"
$work = Join-Path ([IO.Path]::GetTempPath()) ("maru-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $zip = Join-Path $work $archive
    $sums = Join-Path $work 'SHA256SUMS'
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive" -OutFile $zip
    Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sums

    $line = Get-Content $sums | Where-Object { ($_ -split '\s+')[1] -eq $archive } | Select-Object -First 1
    if (-not $line) { Stop-Install "SHA256SUMS has no entry for $archive" 'check MARU_BASE_URL' }
    $want = ($line -split '\s+')[0].ToLowerInvariant()
    $got = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLowerInvariant()
    if ($want -ne $got) { Stop-Install "checksum mismatch for $archive" 'do not use this download; try again or report it' }

    $out = Join-Path $work 'out'
    Expand-Archive -Path $zip -DestinationPath $out
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $bin = Join-Path $dir 'maru.exe'
    try {
        Copy-Item -Force -Path (Join-Path $out 'maru.exe') -Destination $bin
    } catch {
        Stop-Install "cannot replace $bin (is maru running?)" 'maru service uninstall, then run this again'
    }
    $version = & $bin --version
    Write-Output "installed $version"
    Write-Output 'next: maru init --preset blog --name "My site" --yes --json'
    Write-Output $bin
} finally {
    Remove-Item -Recurse -Force -Path $work -ErrorAction SilentlyContinue
}
