# Runs every check CI runs, in the same order, from the repository root.
#
#   ./tools/check.ps1
#
# Stops at the first failure so the output shows the problem rather than
# scrolling past it.

$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

function Invoke-Step {
    param([string]$Name, [scriptblock]$Body)
    Write-Host "`n=== $Name ===" -ForegroundColor Cyan
    & $Body
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $Name" -ForegroundColor Red
        exit $LASTEXITCODE
    }
}

Invoke-Step 'Formatting'  { cargo fmt --all -- --check }
Invoke-Step 'Lint'        { cargo clippy --all-targets --workspace -- -D warnings }
Invoke-Step 'Tests'       { cargo test --workspace }
Invoke-Step 'Schema (TS)' { npm run schema:check }
Invoke-Step 'Client types' { npm run client:typecheck }
Invoke-Step 'Client tests' { npm run client:test }

Write-Host "`nAll checks passed." -ForegroundColor Green
