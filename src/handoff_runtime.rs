use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffRuntimeState {
    pub pane_id: u32,
    pub child_pid: u32,
    /// The start time of `child_pid`, so the receiving server keeps ADR 0014's
    /// pane-root principal instead of a bare pid. `0` from a server that
    /// predates the field; the importer re-reads it from the still-live child.
    #[serde(default)]
    pub child_start_time: u64,
    pub rows: u16,
    pub cols: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    #[serde(default)]
    pub keyboard_protocol_flags: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_protocol_ansi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_state: Option<crate::pane::InputState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_history_ansi: Option<String>,
}

impl HandoffRuntimeState {
    pub fn with_pane_id(mut self, pane_id: crate::layout::PaneId) -> Self {
        self.pane_id = pane_id.raw();
        self
    }
}

#[derive(Debug)]
pub(crate) struct ImportedHandoffRuntime {
    pub master_fd: std::os::fd::RawFd,
    pub state: HandoffRuntimeState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_from_a_server_without_the_start_time_field_still_parses() {
        // A live handoff replaces a RUNNING binary, so the exporting server can
        // predate `child_start_time`. It must still hand its panes over; the
        // importer re-reads the start time from the child, which is alive across
        // the handoff by construction (ADR 0014).
        let older: HandoffRuntimeState = serde_json::from_str(
            r#"{"pane_id":3,"child_pid":4242,"rows":24,"cols":80,
                "cell_width_px":8,"cell_height_px":16}"#,
        )
        .expect("an older server's pane state must still parse");
        assert_eq!(older.child_pid, 4242);
        assert_eq!(
            older.child_start_time, 0,
            "an absent start time is 'never captured', not a match for any pid"
        );
    }

    #[test]
    fn a_handoff_carries_the_pane_root_start_time() {
        let state = HandoffRuntimeState {
            pane_id: 3,
            child_pid: 4242,
            child_start_time: 900,
            rows: 24,
            cols: 80,
            cell_width_px: 8,
            cell_height_px: 16,
            keyboard_protocol_flags: 0,
            keyboard_protocol_ansi: None,
            input_state: None,
            initial_history_ansi: None,
        };
        let round_tripped: HandoffRuntimeState =
            serde_json::from_str(&serde_json::to_string(&state).expect("serialize"))
                .expect("deserialize");
        assert_eq!(round_tripped.child_start_time, 900);
        assert_eq!(round_tripped.child_pid, 4242);
    }
}
