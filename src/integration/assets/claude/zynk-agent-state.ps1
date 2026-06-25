# installed by zynk
# managed by zynk; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# ZYNK_INTEGRATION_ID=claude
# ZYNK_INTEGRATION_VERSION=7

param([string]$Action = "")

# zynk reads ZYNK_* host-protocol vars (primary) and falls back to the legacy
# ZYNK_* names for already-running hosts (bounded transitional compat).
$zynkEnv = if (-not [string]::IsNullOrWhiteSpace($env:ZYNK_ENV)) { $env:ZYNK_ENV } else { $env:ZYNK_ENV }
$zynkPaneId = if (-not [string]::IsNullOrWhiteSpace($env:ZYNK_PANE_ID)) { $env:ZYNK_PANE_ID } else { $env:ZYNK_PANE_ID }

if ($Action -ne "session") { exit 0 }
if ($zynkEnv -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($zynkPaneId)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    exit 0
}

if (-not [string]::IsNullOrWhiteSpace($payload.agent_id)) { exit 0 }
if ($payload.hook_event_name -eq "SubagentStop") { exit 0 }

$sessionId = $payload.session_id
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
try {
    $args = @(
        "pane",
        "report-agent-session",
        $zynkPaneId,
        "--source",
        "zynk:claude",
        "--agent",
        "claude",
        "--seq",
        "$seq",
        "--agent-session-id",
        "$sessionId"
    )
    if ($payload.transcript_path -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.transcript_path)) {
        $args += @("--agent-session-path", "$($payload.transcript_path)")
    }
    if ($payload.hook_event_name -eq "SessionStart" -and $payload.source -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.source)) {
        $args += @("--session-start-source", "$($payload.source)")
    }
    & zynk @args 2>$null | Out-Null
} catch {
}
