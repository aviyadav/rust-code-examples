#Requires -Version 7.0
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

function Assert-LastExitCode {
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code $LASTEXITCODE."
    }
}

$Count = if ($env:COUNT) { $env:COUNT } else { "2000" }
$Rate = if ($env:RATE) { $env:RATE } else { "300" }
$WaitSecs = if ($env:WAIT_SECS) { $env:WAIT_SECS } else { "45" }
$ExeSuffix = if ($IsWindows) { ".exe" } else { "" }
$BinDir = Join-Path $Root "target/debug"

Write-Host "==> starting infrastructure (kafka, postgres, redis)"
docker compose up -d --wait
Assert-LastExitCode

Write-Host "==> building workspace (first build compiles librdkafka; be patient)"
cargo build --workspace
Assert-LastExitCode

New-Item -ItemType Directory -Force -Path (Join-Path $Root "logs"), (Join-Path $Root "run") | Out-Null

function Start-Service {
    param(
        [string]$Name,
        [string]$FilePath,
        [string[]]$ArgumentList = @()
    )

    $PidPath = Join-Path $Root "run/$Name.pid"
    if (Test-Path $PidPath) {
        $ExistingId = 0
        $RawId = (Get-Content -Raw $PidPath).Trim()
        if ([int]::TryParse($RawId, [ref]$ExistingId)) {
            if (Get-Process -Id $ExistingId -ErrorAction SilentlyContinue) {
                Write-Host "    $Name already running"
                return
            }
        }
    }

    $StartArgs = @{
        FilePath = $FilePath
        WorkingDirectory = $Root
        RedirectStandardOutput = Join-Path $Root "logs/$Name.log"
        RedirectStandardError = Join-Path $Root "logs/$Name.err.log"
        PassThru = $true
    }
    if ($ArgumentList.Count -gt 0) {
        $StartArgs["ArgumentList"] = $ArgumentList
    }
    if ($IsWindows) {
        $StartArgs["WindowStyle"] = "Hidden"
    }

    $Process = Start-Process @StartArgs
    Set-Content -Path $PidPath -Value $Process.Id -NoNewline
    Write-Host "    $Name (pid $($Process.Id)) -> logs/$Name.log"
}

Write-Host "==> starting services"
Start-Service -Name "mock-provider" -FilePath (Join-Path $BinDir "mock-provider$ExeSuffix")
Start-Sleep -Seconds 1
Start-Service -Name "ingest-api" -FilePath (Join-Path $BinDir "ingest_api$ExeSuffix")
Start-Service -Name "orchestrator" -FilePath (Join-Path $BinDir "orchestrator$ExeSuffix")
Start-Service -Name "worker-push" -FilePath (Join-Path $BinDir "worker$ExeSuffix") -ArgumentList @("--channel", "push")
Start-Service -Name "worker-sms" -FilePath (Join-Path $BinDir "worker$ExeSuffix") -ArgumentList @("--channel", "sms")
Start-Service -Name "worker-email" -FilePath (Join-Path $BinDir "worker$ExeSuffix") -ArgumentList @("--channel", "email")
Start-Service -Name "retry-scheduler" -FilePath (Join-Path $BinDir "retry_scheduler$ExeSuffix")
Start-Sleep -Seconds 2

Write-Host "==> generating load: $Count events @ $Rate/s (5% duplicates, 1% poison, 2% bad recipients)"
& (Join-Path $BinDir "generator$ExeSuffix") --count $Count --rate $Rate `
    --duplicate-ratio 0.05 --poison-ratio 0.01 --bad-recipient-ratio 0.02 --seed-opt-outs 5
Assert-LastExitCode

Write-Host "==> waiting ${WaitSecs}s for deliveries + retries to settle..."
Start-Sleep -Seconds $WaitSecs

Write-Host "==> provider stats"
try {
    Invoke-RestMethod -Uri "http://localhost:8081/stats" -Method Get | ConvertTo-Json -Compress -Depth 10
} catch {
    Write-Host "(provider stats unavailable)"
}

& (Join-Path $BinDir "inspect$ExeSuffix")
Assert-LastExitCode

$DemoHelp = @"

Demo running. Useful commands:
  Get-Content -Wait logs/worker-push.log      # worker metrics every 5s
  ./scripts_ps/status.ps1                     # dashboard + provider stats
  ./scripts_ps/chaos.ps1 latency-on           # provider latency spike
  ./scripts_ps/chaos.ps1 errors-on            # 60% transient failures -> retry storm
  ./scripts_ps/chaos.ps1 kill-worker          # kill push worker -> lease reclaim
  ./scripts_ps/chaos.ps1 redis-down           # hot tier outage -> PG-only degradation
  ./scripts_ps/chaos.ps1 heal                 # restore provider defaults
  ./scripts_ps/stop_demo.ps1                  # stop services (infra stays up)

Service stderr is written to logs/<name>.err.log.
"@
Write-Host $DemoHelp
