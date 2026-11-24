# Giwa 릴리스 바이너리 크기 검사 (ADR-0176). CI 의 release-size 작업과 손으로 돌릴 때 쓴다.
# 사용: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/size.ps1 [-LimitMB 47]
# 릴리스 빌드가 오래 걸려(개발 PC 12분) 커밋마다 도는 check.ps1 에는 넣지 않았다.
# 1MB = 10^6 바이트. 한도를 바꿀 때는 ADR 을 남기고 이 기본값과 ci.yml 을 같이 고친다.
# 이 파일은 UTF-8 BOM + CRLF 로 저장한다 (PowerShell 5.1 이 한글을 깨뜨리지 않도록).
param([double]$LimitMB = 47)

$ErrorActionPreference = "Continue"
Set-Location (Split-Path -Parent $PSScriptRoot)
$env:PATH = $env:PATH -replace '"', ''

# 실행 파일 경로는 빌드 메시지에서 꺼낸다. PowerShell 5.1 은 cargo 출력의 한글 경로를 깨뜨려
# `cargo metadata` 의 JSON 전체를 읽지 못한다. 실행 파일 경로(대상 폴더)는 ASCII 다(ADR-0015).
$lines = cargo build --release -p giwa-cli --message-format=json-render-diagnostics
if ($LASTEXITCODE -ne 0) {
    Write-Host "[size] 릴리스 빌드 실패" -ForegroundColor Red
    exit $LASTEXITCODE
}
$exe = $null
foreach ($line in $lines) {
    # JSON 안의 역슬래시는 두 개로 적혀 있다
    if ($line -match '"executable":"([^"]*[/\\]giwa(\.exe)?)"') { $exe = $Matches[1] -replace '\\\\', '\' }
}
if (-not $exe -or -not (Test-Path $exe)) {
    Write-Host "[size] 실행 파일을 찾지 못했습니다: $exe" -ForegroundColor Red
    exit 2
}
$name = Split-Path -Leaf $exe
$bytes = (Get-Item $exe).Length
$mb = [math]::Round($bytes / 1e6, 2)
Write-Host "[size] $name $mb MB ($bytes 바이트), 한도 $LimitMB MB"
if ($mb -gt $LimitMB) {
    Write-Host "[size] 한도를 넘었습니다. 무엇이 늘었는지 보고, 줄이거나 ADR 로 한도를 다시 정하세요." -ForegroundColor Red
    exit 1
}
