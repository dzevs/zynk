// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use crate::api::schema::{AgentViewClearParams, AgentViewSetParams, ResponseResult};
use crate::app::App;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_agent_view_set(
        &mut self,
        id: String,
        mut params: AgentViewSetParams,
    ) -> String {
        if let Err(message) = crate::app::agent_view::validate_agent_view(&mut params) {
            return encode_error(id, "invalid_agent_view", message);
        }
        if let Some(plugin_id) = params.source.strip_prefix("plugin:") {
            let Some(plugin_id) = super::plugins::normalize_plugin_id(plugin_id) else {
                return encode_error(
                    id,
                    "invalid_agent_view",
                    "plugin-owned agent view source has an invalid plugin id",
                );
            };
            let Some(plugin) = self.state.installed_plugins.get(&plugin_id) else {
                return encode_error(id, "plugin_not_found", "plugin not found");
            };
            if !plugin.enabled {
                return encode_error(id, "plugin_disabled", "plugin is disabled");
            }
        }
        let source = params.source.clone();
        let label = params.label.clone();
        self.replace_agent_view_override(Some(params));
        encode_success(
            id,
            ResponseResult::AgentView {
                active: true,
                source: Some(source),
                label,
            },
        )
    }

    pub(super) fn handle_agent_view_clear(
        &mut self,
        id: String,
        params: AgentViewClearParams,
    ) -> String {
        let source = match params.source {
            Some(source) => match crate::app::agent_view::validate_agent_view_source(&source) {
                Ok(source) => Some(source),
                Err(message) => return encode_error(id, "invalid_agent_view", message),
            },
            None => None,
        };
        if source.as_deref().is_none_or(|source| {
            self.state
                .agent_view_override
                .as_ref()
                .is_some_and(|active| active.source == source)
        }) {
            self.replace_agent_view_override(None);
        }
        let active = self.state.agent_view_override.as_ref();
        encode_success(
            id,
            ResponseResult::AgentView {
                active: active.is_some(),
                source: active.map(|view| view.source.clone()),
                label: active.and_then(|view| view.label.clone()),
            },
        )
    }

    pub(crate) fn clear_agent_view_for_source(&mut self, source: &str) -> bool {
        if self
            .state
            .agent_view_override
            .as_ref()
            .is_some_and(|active| active.source == source)
        {
            self.replace_agent_view_override(None);
            true
        } else {
            false
        }
    }

    fn replace_agent_view_override(&mut self, view: Option<AgentViewSetParams>) {
        self.state.agent_view_override = view;
        self.state.agent_panel_scroll = 0;
        self.state.mobile_switcher_scroll = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        AgentViewBuiltinField, AgentViewField, AgentViewFilter, AgentViewValue,
    };

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn working_view(source: &str) -> AgentViewSetParams {
        AgentViewSetParams {
            source: source.to_string(),
            label: Some("working".to_string()),
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::Status),
                value: AgentViewValue::String("working".to_string()),
            }),
            sort: Vec::new(),
        }
    }

    #[test]
    fn m844_set_and_source_guarded_clear_reset_projection_scrolls() {
        let mut app = test_app();
        app.state.agent_panel_scroll = 7;
        app.state.mobile_switcher_scroll = 9;

        let response = app.handle_agent_view_set("set".to_string(), working_view("manual.view"));
        let response: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).unwrap();
        assert_eq!(
            response.result,
            ResponseResult::AgentView {
                active: true,
                source: Some("manual.view".to_string()),
                label: Some("working".to_string()),
            }
        );
        assert_eq!(
            (
                app.state.agent_panel_scroll,
                app.state.mobile_switcher_scroll
            ),
            (0, 0)
        );

        app.handle_agent_view_clear(
            "wrong".to_string(),
            AgentViewClearParams {
                source: Some("other.view".to_string()),
            },
        );
        assert!(app.state.agent_view_override.is_some());
        app.handle_agent_view_clear(
            "right".to_string(),
            AgentViewClearParams {
                source: Some("manual.view".to_string()),
            },
        );
        assert!(app.state.agent_view_override.is_none());
    }

    #[test]
    fn m844_detection_labeled_view_never_mints_receipt_authority() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("observed");
        let pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let public_pane = app.public_pane_id(0, pane).unwrap();
        let terminal_id = app.state.workspaces[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Working,
            );

        app.handle_agent_view_set("set".to_string(), working_view("manual.view"));
        assert_eq!(crate::ui::agent_panel_entries(&app.state).len(), 1);
        assert!(app.authoritative_receiver_identity(&public_pane).is_none());
    }
}
