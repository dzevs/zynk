# installed by zynk
# managed by zynk; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# ZYNK_INTEGRATION_ID=droid
# ZYNK_INTEGRATION_VERSION=2

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
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

if ($null -eq $payload -or [string]::IsNullOrWhiteSpace($payload.session_id)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
try {
    & zynk pane report-agent-session $zynkPaneId --source zynk:droid --agent droid --agent-session-id $payload.session_id --seq $seq 2>$null | Out-Null
} catch {
}
