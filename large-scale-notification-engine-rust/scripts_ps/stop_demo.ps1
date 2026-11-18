#Requires -Version 7.0
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$All = ($args -contains "--all") -or ($args -contains "-all")
$PidFiles = Get-ChildItem -Path (Join-Path $Root "run") -Filter "*.pid" -File -ErrorAction SilentlyContinue

foreach ($File in $PidFiles) {
    $ProcessId = 0
    $RawId = (Get-Content -Raw $File.FullName).Trim()
    if ([int]::TryParse($RawId, [ref]$ProcessId)) {
        if (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue) {
            Stop-Process -Id $ProcessId -ErrorAction SilentlyContinue
            Write-Host "stopped $($File.BaseName) ($ProcessId)"
        }
    }
    Remove-Item -LiteralPath $File.FullName -Force
}

Start-Sleep -Seconds 1

if ($All) {
    docker compose down
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    Write-Host "infrastructure stopped (volumes preserved; use 'docker compose down -v' to wipe data)"
}
