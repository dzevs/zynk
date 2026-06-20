# installed by zynk
# managed by zynk; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# ZYNK_INTEGRATION_ID=copilot
# ZYNK_INTEGRATION_VERSION=2

# zynk reads ZYNK_* host-protocol vars (primary) and falls back to the legacy
# ZYNK_* names for already-running hosts (bounded transitional compat).
$zynkEnv = if (-not [string]::IsNullOrWhiteSpace($env:ZYNK_ENV)) { $env:ZYNK_ENV } else { $env:ZYNK_ENV }
$zynkPaneId = if (-not [string]::IsNullOrWhiteSpace($env:ZYNK_PANE_ID)) { $env:ZYNK_PANE_ID } else { $env:ZYNK_PANE_ID }
$zynkSocketPath = if (-not [string]::IsNullOrWhiteSpace($env:ZYNK_SOCKET_PATH)) { $env:ZYNK_SOCKET_PATH } else { $env:ZYNK_SOCKET_PATH }

if ($zynkEnv -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($zynkPaneId)) { exit 0 }
if ([string]::IsNullOrWhiteSpace($zynkSocketPath)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { @{} } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = @{}
}

function First-Text {
    param([object[]]$Names)
    foreach ($name in $Names) {
        $value = $payload.$name
        if ($value -is [string] -and -not [string]::IsNullOrWhiteSpace($value)) {
            return $value
        }
    }
    return $null
}

function Normalize-Event {
    param([string]$Event)
    if ([string]::IsNullOrWhiteSpace($Event)) { return "" }
    return $Event.Replace("_", "").Replace("-", "").ToLowerInvariant()
}

$eventName = First-Text @("hook_event_name", "hookEventName")
if ($eventName) {
    if ((Normalize-Event $eventName) -ne "sessionstart") { exit 0 }
} elseif (
    ($payload.PSObject.Properties.Name -contains "prompt") -or
    (First-Text @("tool_name", "toolName", "notification_type", "notificationType", "stop_reason", "stopReason", "reason"))
) {
    exit 0
}

$sessionId = $payload.session_id
if ([string]::IsNullOrWhiteSpace($sessionId)) {
    $sessionId = $payload.sessionId
}
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
& zynk pane report-agent-session $zynkPaneId --source zynk:copilot --agent copilot --agent-session-id $sessionId --seq $seq 2>$null | Out-Null
