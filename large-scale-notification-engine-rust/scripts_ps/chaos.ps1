#Requires -Version 7.0
param([string]$Command = "help")

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
$ProviderBase = "http://localhost:8081"
$ExeSuffix = if ($IsWindows) { ".exe" } else { "" }
$Generator = Join-Path $Root "target/debug/generator$ExeSuffix"

function Assert-LastExitCode {
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code $LASTEXITCODE."
    }
}

function Invoke-ProviderPost {
    param(
        [string]$Path,
        [string]$Body
    )

    $Uri = "$ProviderBase$Path"
    if ($PSBoundParameters.ContainsKey("Body")) {
        $Response = Invoke-RestMethod -Uri $Uri -Method Post -ContentType "application/json" -Body $Body
    } else {
        $Response = Invoke-RestMethod -Uri $Uri -Method Post
    }
    $Response | ConvertTo-Json -Compress -Depth 10
}

switch ($Command) {
    "latency-on" {
        Invoke-ProviderPost -Path "/control" -Body '{"latency_ms":2800,"latency_jitter_ms":600}'
        break
    }
    "errors-on" {
        Invoke-ProviderPost -Path "/control" -Body '{"transient_rate":0.6}'
        break
    }
    "permanent-on" {
        Invoke-ProviderPost -Path "/control" -Body '{"permanent_rate":0.2}'
        break
    }
    "heal" {
        Invoke-ProviderPost -Path "/control/reset"
        break
    }
    "kill-worker" {
        $PidPath = Join-Path $Root "run/worker-push.pid"
        if (-not (Test-Path $PidPath)) {
            throw "Worker PID file not found: $PidPath"
        }
        $ProcessId = 0
        $RawId = (Get-Content -Raw $PidPath).Trim()
        if (-not [int]::TryParse($RawId, [ref]$ProcessId)) {
            throw "Invalid worker PID in $PidPath"
        }
        Stop-Process -Id $ProcessId -Force
        Write-Host "Killed worker-push ($ProcessId). Leases expire in ~30s."
        Write-Host "Restart with: ./scripts_ps/chaos.ps1 restart-worker"
        break
    }
    "restart-worker" {
        New-Item -ItemType Directory -Force -Path (Join-Path $Root "logs"), (Join-Path $Root "run") | Out-Null
        $StartArgs = @{
            FilePath = Join-Path $Root "target/debug/worker$ExeSuffix"
            ArgumentList = @("--channel", "push")
            WorkingDirectory = $Root
            RedirectStandardOutput = Join-Path $Root "logs/worker-push.log"
            RedirectStandardError = Join-Path $Root "logs/worker-push.err.log"
            PassThru = $true
        }
        if ($IsWindows) {
            $StartArgs["WindowStyle"] = "Hidden"
        }
        $Process = Start-Process @StartArgs
        Set-Content -Path (Join-Path $Root "run/worker-push.pid") -Value $Process.Id -NoNewline
        Write-Host "worker-push restarted (pid $($Process.Id)); it redelivers uncommitted offsets"
        break
    }
    "redis-down" {
        docker compose stop redis
        Assert-LastExitCode
        Write-Host "hot tier down -> workers degrade to PostgreSQL"
        break
    }
    "redis-up" {
        docker compose start redis
        Assert-LastExitCode
        break
    }
    "poison-burst" {
        & $Generator --count 200 --rate 0 --poison-ratio 0.5
        Assert-LastExitCode
        break
    }
    "dup-burst" {
        & $Generator --count 300 --rate 0 --duplicate-ratio 0.8 --id-prefix dupdemo
        Assert-LastExitCode
        break
    }
    default {
        $Usage = @'
usage: chaos.ps1 <command>
  latency-on | errors-on | permanent-on | heal
  kill-worker | restart-worker
  redis-down | redis-up
  poison-burst | dup-burst
'@
        Write-Host $Usage
    }
}
