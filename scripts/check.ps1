# Giwa 품질 게이트. pre-commit 과 CI 가 같은 스크립트를 부른다.
# 사용: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check.ps1 [-Quick]
#   -Quick : 테스트를 건너뛴다 (fmt, clippy, 의존 방향만)
# 이 파일은 UTF-8 BOM + CRLF 로 저장한다 (PowerShell 5.1 이 한글을 깨뜨리지 않도록).
param([switch]$Quick)

# cargo 는 진행 메시지("Blocking waiting for file lock" 등)를 stderr 로 쓴다.
# PowerShell 5.1 에서 Stop 이면 그것만으로 실패하므로 Continue 로 두고, 성공·실패는 종료 코드로만 판정한다.
$ErrorActionPreference = "Continue"
Set-Location (Split-Path -Parent $PSScriptRoot)

# PATH 의 따옴표를 없앤다. 시스템 PATH 에 "...\Git\bin;...\Git\cmd" 처럼 묶인 항목이 있으면 git-bash 가
# 넘겨줄 때 앞 따옴표만 떨어져 짝이 깨지고, cmd 와 cargo 가 그 뒤 항목(cargo-sqlx 등)을 찾지 못한다 (2026-11-07).
$env:PATH = $env:PATH -replace '"', ''

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
    "giwa-core"    = @("sqlx", "axum", "minijinja", "tokio", "giwa-storage", "giwa-web", "giwa-themes")
    "giwa-storage" = @("axum", "minijinja", "giwa-web", "giwa-themes")
    "giwa-themes"  = @("sqlx", "axum", "giwa-storage", "giwa-web")
    "giwa-web"     = @("sqlx", "giwa-storage", "giwa-mod-point", "giwa-mod-reaction", "giwa-mod-scrap", "giwa-mod-notify", "giwa-mod-message", "giwa-mod-poll", "giwa-mod-report", "giwa-mod-webhook")
    "giwa-modules" = @("sqlx", "giwa-storage", "giwa-web", "giwa-themes")
    "giwa-agent"   = @("sqlx", "giwa-storage", "giwa-web")
}
Write-Host "[check] deps" -ForegroundColor Cyan
foreach ($crate in $forbidden.Keys) {
    # stderr 는 버린다 (2>&1 처럼 섞으면 PowerShell 이 오류 레코드로 감싼다).
    $tree = cargo tree -p $crate -e normal --prefix none --format "{p}" 2>$null
    if ($LASTEXITCODE -ne 0) { $failed += "deps($crate)"; continue }
    $names = $tree | ForEach-Object { ($_ -split " ")[0] } | Sort-Object -Unique
    foreach ($bad in $forbidden[$crate]) {
        if ($names -contains $bad) {
            Write-Host "[check] 금지된 의존: $crate -> $bad" -ForegroundColor Red
            $failed += "deps($crate->$bad)"
        }
    }
}

# Postgres 로 옮길 문을 열어 둔다 (ADR-0175). 저장 계층의 SQLite 전용 문법이 늘면 실패한다.
# 같은 일을 하는 공통 문법을 쓴다: INSERT OR IGNORE -> ON CONFLICT DO NOTHING, rowid -> 명시한 id 열.
# 줄였으면 아래 기준 수도 함께 낮춘다 (다시 늘지 않게). 정말 필요하면 이유를 ADR 에 적고 올린다.
$sqliteOnly = [ordered]@{
    'INSERT OR (IGNORE|REPLACE)' = 7
    '(?i)\browid\b'              = 15
}
Write-Host "[check] sqlite-only" -ForegroundColor Cyan
$storageFiles = @(Get-ChildItem crates/storage/src -Recurse -Filter *.rs) + @(Get-ChildItem mods -Recurse -Filter *.rs | Where-Object { $_.FullName -match '[\\/]src[\\/]' })
foreach ($pattern in $sqliteOnly.Keys) {
    $count = ($storageFiles | Select-String -Pattern $pattern -CaseSensitive | Measure-Object).Count
    $limit = $sqliteOnly[$pattern]
    if ($count -gt $limit) {
        Write-Host "[check] SQLite 전용 문법이 늘었습니다: $pattern $count 곳 (기준 $limit). 공통 문법을 쓰세요 (ADR-0175)." -ForegroundColor Red
        $failed += "sqlite-only($pattern)"
    } elseif ($count -lt $limit) {
        Write-Host "[check] $pattern 가 $count 곳으로 줄었습니다. scripts/check.ps1 의 기준을 $count 로 낮추세요." -ForegroundColor Yellow
    }
}

if ($Quick) {
    $skipped += "test"
} else {
    Step "test" { cargo test --workspace --quiet --no-fail-fast }
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
            $targetDir = (cargo metadata --format-version 1 --no-deps 2>$null | Out-String | ConvertFrom-Json).target_directory
        } catch {}
        [Console]::OutputEncoding = $prevEncoding
        if (-not $targetDir) {
            Write-Host "[check] cargo metadata 에서 target 경로를 읽지 못했습니다" -ForegroundColor Red
            $failed += "sqlx-prepare(target 경로)"
        } else {
            $checkDb = Join-Path $targetDir "sqlx-check.db"
            Remove-Item "$checkDb*" -Force -ErrorAction SilentlyContinue
            $env:DATABASE_URL = "sqlite:" + ($checkDb -replace "\\", "/")
            Step "sqlx-db" {
                sqlx database create
                if ($LASTEXITCODE -eq 0) { sqlx migrate run --source crates/storage/migrations }
            }
            # 모듈 표도 있어야 모듈의 쿼리 매크로를 검증한다 (ADR-0066).
            foreach ($moduleMigrations in (Get-ChildItem "mods/*/migrations" -Directory -ErrorAction SilentlyContinue)) {
                $source = $moduleMigrations.FullName
                Step "sqlx-db($($moduleMigrations.Parent.Name))" { sqlx migrate run --ignore-missing --source $source }
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
