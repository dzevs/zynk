//! Codex session hints constrain caller attribution; they never select a pane.

pub(crate) const CALLER_IDENTITY_CONFLICT: &str = "caller_identity_conflict";
pub(crate) const CODEX_THREAD_ID_ENV: &str = "CODEX_THREAD_ID";
pub(crate) const CODEX_SESSION_ID_ENV: &str = "CODEX_SESSION_ID";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallerIdentityConflict {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CodexSessionHints {
    value: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CodexHintVerification {
    NotPresent,
    Verified,
    Unverified,
}

impl CodexSessionHints {
    pub(crate) fn from_values(
        thread_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<Self, CallerIdentityConflict> {
        let thread_id = nonempty(thread_id);
        let session_id = nonempty(session_id);
        if let (Some(thread), Some(session)) = (&thread_id, &session_id) {
            if thread != session {
                return Err(CallerIdentityConflict {
                    code: CALLER_IDENTITY_CONFLICT,
                    message: format!(
                        "{CODEX_THREAD_ID_ENV} ({thread}) contradicts {CODEX_SESSION_ID_ENV} ({session})"
                    ),
                });
            }
        }
        Ok(Self {
            value: thread_id.or(session_id),
        })
    }

    pub(crate) fn from_process_env() -> Result<Self, CallerIdentityConflict> {
        Self::from_values(
            std::env::var(CODEX_THREAD_ID_ENV).ok(),
            std::env::var(CODEX_SESSION_ID_ENV).ok(),
        )
    }

    #[cfg(test)]
    pub(crate) fn present(value: impl Into<String>) -> Self {
        Self {
            value: nonempty(Some(value.into())),
        }
    }

    pub(crate) fn is_present(&self) -> bool {
        self.value.is_some()
    }

    pub(crate) fn validate(
        &self,
        authoritative_codex_session: Option<&str>,
    ) -> Result<CodexHintVerification, CallerIdentityConflict> {
        let Some(hint) = self.value.as_deref() else {
            return Ok(CodexHintVerification::NotPresent);
        };
        let Some(authoritative) = authoritative_codex_session else {
            return Ok(CodexHintVerification::Unverified);
        };
        if hint == authoritative {
            return Ok(CodexHintVerification::Verified);
        }
        Err(CallerIdentityConflict {
            code: CALLER_IDENTITY_CONFLICT,
            message: format!(
                "Codex session hint {hint} contradicts the hook-authoritative session {authoritative} on ZYNK_PANE_ID"
            ),
        })
    }

    pub(crate) fn validate_pane_info(
        &self,
        pane: &serde_json::Value,
    ) -> Result<CodexHintVerification, CallerIdentityConflict> {
        self.validate_agent_session(pane.get("agent_session"))
    }

    pub(crate) fn validate_agent_session(
        &self,
        session: Option<&serde_json::Value>,
    ) -> Result<CodexHintVerification, CallerIdentityConflict> {
        self.validate(authoritative_codex_session_from_value(session))
    }
}

fn authoritative_codex_session_from_value(session: Option<&serde_json::Value>) -> Option<&str> {
    let session = session?;
    (session.get("source")?.as_str()? == "zynk:codex"
        && session.get("agent")?.as_str()? == "codex"
        && session.get("kind")?.as_str()? == "id")
        .then(|| session.get("value")?.as_str())
        .flatten()
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_hint_pair_must_agree() {
        assert_eq!(
            CodexSessionHints::from_values(None, None).unwrap(),
            CodexSessionHints::default()
        );
        assert_eq!(
            CodexSessionHints::from_values(Some("same".into()), Some("same".into())).unwrap(),
            CodexSessionHints::present("same")
        );
        let conflict =
            CodexSessionHints::from_values(Some("thread".into()), Some("session".into()))
                .unwrap_err();
        assert_eq!(conflict.code, "caller_identity_conflict");
        assert!(conflict.message.contains("CODEX_THREAD_ID"));
        assert!(conflict.message.contains("CODEX_SESSION_ID"));
    }

    #[test]
    fn present_hint_only_fails_on_an_authoritative_contradiction() {
        let hints = CodexSessionHints::present("expected");
        assert_eq!(
            hints.validate(None).unwrap(),
            CodexHintVerification::Unverified
        );
        assert_eq!(
            hints.validate(Some("expected")).unwrap(),
            CodexHintVerification::Verified
        );
        let conflict = hints.validate(Some("other")).unwrap_err();
        assert_eq!(conflict.code, "caller_identity_conflict");
        assert!(conflict.message.contains("expected"));
        assert!(conflict.message.contains("other"));
        assert_eq!(
            CodexSessionHints::default().validate(None).unwrap(),
            CodexHintVerification::NotPresent
        );
    }
}
