# Maru 품질 게이트. pre-commit 과 CI 가 같은 스크립트를 부른다.
# 사용: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check.ps1 [-Quick]
#   -Quick : 테스트를 건너뛴다 (fmt, clippy, 의존 방향만)
# 이 파일은 UTF-8 BOM + CRLF 로 저장한다 (PowerShell 5.1 이 한글을 깨뜨리지 않도록).
param([switch]$Quick)

# cargo 는 진행 메시지("Blocking waiting for file lock" 등)를 stderr 로 쓴다.
# PowerShell 5.1 에서 Stop 이면 그것만으로 실패하므로 Continue 로 두고, 성공·실패는 종료 코드로만 판정한다.
$ErrorActionPreference = "Continue"
Set-Location (Split-Path -Parent $PSScriptRoot)

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    if ($env:CARGO_HOME -and (Test-Path (Join-Path $env:CARGO_HOME "bin/cargo.exe"))) {
        $env:PATH = (Join-Path $env:CARGO_HOME "bin") + ";" + $env:PATH
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
    "maru-web"     = @("sqlx", "maru-storage", "maru-mod-point", "maru-mod-reaction", "maru-mod-scrap")
    "maru-modules" = @("sqlx", "maru-storage", "maru-web", "maru-themes")
}
Write-Host "[check] deps" -ForegroundColor Cyan
foreach ($crate in $forbidden.Keys) {
    # stderr 는 cmd 가 버린다 (PowerShell 이 오류 레코드로 감싸 출력에 섞지 않도록)
    $tree = cmd /c "cargo tree -p $crate -e normal --prefix none --format {p} 2>nul"
    if ($LASTEXITCODE -ne 0) { $failed += "deps($crate)"; continue }
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
        # 마이그레이션만 적용한 빈 DB 를 매번 새로 만들어 비교한다 (개발 DB 상태에 좌우되지 않게).
        # cargo 출력의 한글 경로가 콘솔 코드 페이지로 깨지지 않게 UTF-8 로 읽는다.
        $prevEncoding = [Console]::OutputEncoding
        [Console]::OutputEncoding = [Text.Encoding]::UTF8
        $targetDir = $null
        try {
            $targetDir = (cmd /c "cargo metadata --format-version 1 --no-deps 2>nul" | Out-String | ConvertFrom-Json).target_directory
        } catch {}
        [Console]::OutputEncoding = $prevEncoding
        if (-not $targetDir) {
            Write-Host "[check] cargo metadata 에서 target 경로를 읽지 못했습니다" -ForegroundColor Red
            $failed += "sqlx-prepare(target 경로)"
        } else {
            $checkDb = Join-Path $targetDir "sqlx-check.db"
            Remove-Item "$checkDb*" -Force -ErrorAction SilentlyContinue
            $env:DATABASE_URL = "sqlite:" + ($checkDb -replace "\\", "/")
            Step "sqlx-db" { cmd /c "sqlx database create && sqlx migrate run --source crates/storage/migrations" }
            # 모듈 표도 있어야 모듈의 쿼리 매크로를 검증한다 (ADR-0066).
            foreach ($moduleMigrations in (Get-ChildItem "mods/*/migrations" -Directory -ErrorAction SilentlyContinue)) {
                $source = $moduleMigrations.FullName
                Step "sqlx-db($($moduleMigrations.Parent.Name))" { cmd /c "sqlx migrate run --ignore-missing --source `"$source`"" }
            }
            Step "sqlx-prepare" { cargo sqlx prepare --workspace --check }
            Remove-Item Env:DATABASE_URL
        }
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
