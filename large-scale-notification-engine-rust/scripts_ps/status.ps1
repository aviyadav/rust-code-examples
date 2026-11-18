#Requires -Version 7.0
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

Write-Host "==> infrastructure"
docker compose ps
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Host ""
Write-Host "==> mock provider"
try {
    Invoke-RestMethod -Uri "http://localhost:8081/stats" -Method Get | ConvertTo-Json -Compress -Depth 10
} catch {
    Write-Host "(provider not running)"
}
Write-Host ""

cargo run --quiet -p engine --bin inspect -- @args
exit $LASTEXITCODE
