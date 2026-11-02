# Maru 품질 게이트. pre-commit 과 CI 가 같은 스크립트를 부른다.
# 사용: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check.ps1 [-Quick]
#   -Quick : 테스트를 건너뛴다 (fmt, clippy, 의존 방향만)
param([switch]$Quick)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    if ($env:CARGO_HOME -and (Test-Path "$env:CARGO_HOMEincargo.exe")) {
        $env:PATH = "$env:CARGO_HOMEin;$env:PATH"
    } else {
        Write-Host "[check] cargo 를 찾을 수 없습니다. Rust 를 설치하거나 CARGO_HOME 을 설정하세요." -ForegroundColor Red
        exit 2
    }
}

$failed = @()
$skipped = @()

function Step([string]$name, [scriptblock]$body) {
    Write-Host "[check] $name" -ForegroundColor Cyan
    & $body
    if ($LASTEXITCODE -ne 0) { $script:failed += $name }
}

Step "fmt" { cargo fmt --all --check }
Step "clippy" { cargo clippy --workspace --all-targets --quiet -- -D warnings }

# 의존 방향 (docs/PHASE1_PLAN.md 4.1). 왼쪽 크레이트의 정상 의존 트리에 오른쪽 크레이트가 있으면 실패.
$forbidden = [ordered]@{
    "maru-core"    = @("sqlx", "axum", "minijinja", "tokio", "maru-storage", "maru-web", "maru-themes")
    "maru-storage" = @("axum", "minijinja", "maru-web", "maru-themes")
    "maru-themes"  = @("sqlx", "axum", "maru-storage", "maru-web")
    "maru-web"     = @("sqlx", "maru-storage")
}
Write-Host "[check] deps" -ForegroundColor Cyan
foreach ($crate in $forbidden.Keys) {
    $tree = cargo tree -p $crate -e normal --prefix none --format "{p}" 2>&1
    if ($LASTEXITCODE -ne 0) { $failed += "deps($crate)"; Write-Host $tree; continue }
    $names = $tree | ForEach-Object { ($_ -split " ")[0] } | Sort-Object -Unique
    foreach ($bad in $forbidden[$crate]) {
        if ($names -contains $bad) {
            Write-Host "[check] 금지된 의존: $crate -> $bad" -ForegroundColor Red
            $failed += "deps($crate->$bad)"
        }
    }
}

if ($Quick) {
    $skipped += "test"
} else {
    Step "test" { cargo test --workspace --quiet }
}

# 쿼리 메타데이터: 마이그레이션이 생긴 뒤부터 검사한다.
if (Test-Path "crates/storage/migrations") {
    if (Get-Command sqlx -ErrorAction SilentlyContinue) {
        Step "sqlx-prepare" { cargo sqlx prepare --workspace --check }
    } else {
        $skipped += "sqlx-prepare(sqlx-cli 없음)"
    }
}

if ($skipped.Count -gt 0) { Write-Host "[check] 못 쟀다: $($skipped -join ", ")" -ForegroundColor Yellow }
if ($failed.Count -gt 0) {
    Write-Host "[check] 실패: $($failed -join ", ")" -ForegroundColor Red
    exit 1
}
Write-Host "[check] 통과" -ForegroundColor Green
