# installed by zynk
# managed by zynk; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# ZYNK_INTEGRATION_ID=kimi
# ZYNK_INTEGRATION_VERSION=3

param([string]$Action = "")

if (@("session", "working", "blocked", "idle", "release") -notcontains $Action) { exit 0 }
# zynk reads ZYNK_* host-protocol vars (primary) and falls back to the legacy
# ZYNK_* names for already-running hosts (bounded transitional compat).
$zynkEnv = if (-not [string]::IsNullOrWhiteSpace($env:ZYNK_ENV)) { $env:ZYNK_ENV } else { $env:ZYNK_ENV }
$zynkPaneId = if (-not [string]::IsNullOrWhiteSpace($env:ZYNK_PANE_ID)) { $env:ZYNK_PANE_ID } else { $env:ZYNK_PANE_ID }
$zynkSocketPath = if (-not [string]::IsNullOrWhiteSpace($env:ZYNK_SOCKET_PATH)) { $env:ZYNK_SOCKET_PATH } else { $env:ZYNK_SOCKET_PATH }

if ($zynkEnv -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($zynkPaneId)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$sessionId = if ($null -ne $payload -and -not [string]::IsNullOrWhiteSpace($payload.session_id)) { $payload.session_id } else { $null }

try {
    if ($Action -eq "release") {
        & zynk pane release-agent $zynkPaneId --source zynk:kimi --agent kimi --seq $seq 2>$null | Out-Null
    } elseif ($Action -eq "session") {
        if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }
        & zynk pane report-agent-session $zynkPaneId --source zynk:kimi --agent kimi --agent-session-id $sessionId --seq $seq 2>$null | Out-Null
    } else {
        if ([string]::IsNullOrWhiteSpace($sessionId)) {
            & zynk pane report-agent $zynkPaneId --source zynk:kimi --agent kimi --state $Action --seq $seq 2>$null | Out-Null
        } else {
            & zynk pane report-agent $zynkPaneId --source zynk:kimi --agent kimi --state $Action --agent-session-id $sessionId --seq $seq 2>$null | Out-Null
        }
    }
} catch {
}
