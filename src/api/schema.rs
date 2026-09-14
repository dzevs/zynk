use serde::{Deserialize, Serialize};

pub mod agents;
pub mod common;
pub mod events;
pub(crate) mod export;
pub mod integrations;
pub mod panes;
pub mod plugins;
pub mod response;
pub mod server;
pub mod session;
pub mod tabs;
pub mod workspaces;
pub mod worktrees;
pub mod zynk;

pub use agents::*;
pub use common::*;
pub use events::*;
pub use integrations::*;
pub use panes::*;
pub use plugins::*;
pub use response::*;
pub use server::*;
pub use session::*;
pub use tabs::*;
pub use workspaces::*;
pub use worktrees::*;
pub use zynk::*;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Request {
    pub id: String,
    #[serde(flatten)]
    pub method: Method,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", content = "params")]
// Request enums are short-lived wire values; keeping variants direct preserves
// the simple serde shape and avoids boxing churn across every caller.
#[allow(clippy::large_enum_variant)]
pub enum Method {
    #[serde(rename = "ping")]
    Ping(PingParams),
    #[serde(rename = "server.stop")]
    ServerStop(EmptyParams),
    #[serde(rename = "server.live_handoff")]
    ServerLiveHandoff(ServerLiveHandoffParams),
    #[serde(rename = "server.reload_config")]
    ServerReloadConfig(EmptyParams),
    #[serde(rename = "server.agent_manifests")]
    ServerAgentManifests(EmptyParams),
    #[serde(rename = "server.reload_agent_manifests")]
    ServerReloadAgentManifests(EmptyParams),
    #[serde(rename = "session.snapshot")]
    SessionSnapshot(EmptyParams),
    #[serde(rename = "notification.show")]
    NotificationShow(NotificationShowParams),
    #[serde(rename = "client.window_title.set")]
    ClientWindowTitleSet(ClientWindowTitleSetParams),
    #[serde(rename = "client.window_title.clear")]
    ClientWindowTitleClear(EmptyParams),
    #[serde(rename = "workspace.create")]
    WorkspaceCreate(WorkspaceCreateParams),
    #[serde(rename = "workspace.list")]
    WorkspaceList(EmptyParams),
    #[serde(rename = "workspace.get")]
    WorkspaceGet(WorkspaceTarget),
    #[serde(rename = "workspace.focus")]
    WorkspaceFocus(WorkspaceTarget),
    #[serde(rename = "workspace.rename")]
    WorkspaceRename(WorkspaceRenameParams),
    #[serde(rename = "workspace.move")]
    WorkspaceMove(WorkspaceMoveParams),
    #[serde(rename = "workspace.report_metadata")]
    WorkspaceReportMetadata(WorkspaceReportMetadataParams),
    #[serde(rename = "workspace.close")]
    WorkspaceClose(WorkspaceTarget),
    #[serde(rename = "worktree.list")]
    WorktreeList(WorktreeListParams),
    #[serde(rename = "worktree.create")]
    WorktreeCreate(WorktreeCreateParams),
    #[serde(rename = "worktree.open")]
    WorktreeOpen(WorktreeOpenParams),
    #[serde(rename = "worktree.remove")]
    WorktreeRemove(WorktreeRemoveParams),
    #[serde(rename = "tab.create")]
    TabCreate(TabCreateParams),
    #[serde(rename = "tab.list")]
    TabList(TabListParams),
    #[serde(rename = "tab.get")]
    TabGet(TabTarget),
    #[serde(rename = "tab.focus")]
    TabFocus(TabTarget),
    #[serde(rename = "tab.rename")]
    TabRename(TabRenameParams),
    #[serde(rename = "tab.move")]
    TabMove(TabMoveParams),
    #[serde(rename = "tab.close")]
    TabClose(TabTarget),
    #[serde(rename = "agent.list")]
    AgentList(EmptyParams),
    #[serde(rename = "agent.get")]
    AgentGet(AgentTarget),
    #[serde(rename = "agent.read")]
    AgentRead(AgentReadParams),
    #[serde(rename = "agent.explain")]
    AgentExplain(AgentTarget),
    #[serde(rename = "agent.send")]
    AgentSend(AgentSendParams),
    #[serde(rename = "agent.rename")]
    AgentRename(AgentRenameParams),
    #[serde(rename = "agent.focus")]
    AgentFocus(AgentTarget),
    #[serde(rename = "agent.start")]
    AgentStart(AgentStartParams),
    #[serde(rename = "pane.split")]
    PaneSplit(PaneSplitParams),
    #[serde(rename = "pane.swap")]
    PaneSwap(PaneSwapParams),
    #[serde(rename = "pane.move")]
    PaneMove(PaneMoveParams),
    #[serde(rename = "pane.zoom")]
    PaneZoom(PaneZoomParams),
    #[serde(rename = "pane.layout")]
    PaneLayout(PaneLayoutParams),
    #[serde(rename = "pane.process_info")]
    PaneProcessInfo(PaneProcessInfoParams),
    #[serde(rename = "layout.export")]
    LayoutExport(LayoutExportParams),
    #[serde(rename = "layout.apply")]
    LayoutApply(LayoutApplyParams),
    #[serde(rename = "layout.set_split_ratio")]
    LayoutSetSplitRatio(LayoutSetSplitRatioParams),
    #[serde(rename = "pane.neighbor")]
    PaneNeighbor(PaneNeighborParams),
    #[serde(rename = "pane.edges")]
    PaneEdges(PaneEdgesParams),
    #[serde(rename = "pane.focus_direction")]
    PaneFocusDirection(PaneFocusDirectionParams),
    #[serde(rename = "pane.resize")]
    PaneResize(PaneResizeParams),
    #[serde(rename = "pane.list")]
    PaneList(PaneListParams),
    #[serde(rename = "pane.current")]
    PaneCurrent(PaneCurrentParams),
    #[serde(rename = "pane.get")]
    PaneGet(PaneTarget),
    #[serde(rename = "pane.focus")]
    PaneFocus(PaneTarget),
    #[serde(rename = "pane.rename")]
    PaneRename(PaneRenameParams),
    #[serde(rename = "pane.send_text")]
    PaneSendText(PaneSendTextParams),
    #[serde(rename = "pane.send_keys")]
    PaneSendKeys(PaneSendKeysParams),
    #[serde(rename = "pane.send_input")]
    PaneSendInput(PaneSendInputParams),
    #[serde(rename = "pane.read")]
    PaneRead(PaneReadParams),
    #[serde(rename = "pane.report_agent")]
    PaneReportAgent(PaneReportAgentParams),
    #[serde(rename = "pane.report_agent_session")]
    PaneReportAgentSession(PaneReportAgentSessionParams),
    #[serde(rename = "pane.report_metadata")]
    PaneReportMetadata(PaneReportMetadataParams),
    #[serde(rename = "pane.clear_agent_authority")]
    PaneClearAgentAuthority(PaneClearAgentAuthorityParams),
    #[serde(rename = "pane.release_agent")]
    PaneReleaseAgent(PaneReleaseAgentParams),
    #[serde(rename = "pane.close")]
    PaneClose(PaneTarget),
    #[serde(rename = "events.subscribe")]
    EventsSubscribe(EventsSubscribeParams),
    #[serde(rename = "events.wait")]
    EventsWait(EventsWaitParams),
    #[serde(rename = "pane.wait_for_output")]
    PaneWaitForOutput(PaneWaitForOutputParams),
    #[serde(rename = "integration.install")]
    IntegrationInstall(IntegrationInstallParams),
    #[serde(rename = "integration.uninstall")]
    IntegrationUninstall(IntegrationUninstallParams),
    #[serde(rename = "plugin.link")]
    PluginLink(PluginLinkParams),
    #[serde(rename = "plugin.list")]
    PluginList(PluginListParams),
    #[serde(rename = "plugin.unlink")]
    PluginUnlink(PluginUnlinkParams),
    #[serde(rename = "plugin.enable")]
    PluginEnable(PluginSetEnabledParams),
    #[serde(rename = "plugin.disable")]
    PluginDisable(PluginSetEnabledParams),
    #[serde(rename = "plugin.action.list")]
    PluginActionList(PluginActionListParams),
    #[serde(rename = "plugin.action.invoke")]
    PluginActionInvoke(PluginActionInvokeParams),
    #[serde(rename = "plugin.log.list")]
    PluginLogList(PluginLogListParams),
    #[serde(rename = "plugin.pane.open")]
    PluginPaneOpen(PluginPaneOpenParams),
    #[serde(rename = "plugin.pane.focus")]
    PluginPaneFocus(PluginPaneFocusParams),
    #[serde(rename = "plugin.pane.close")]
    PluginPaneClose(PluginPaneCloseParams),
    // zynk fork (M3a): native receipt method. Ledger: docs/zynk/fork-patch-ledger.md.
    #[serde(rename = "zynk.message_received")]
    ZynkMessageReceived(ZynkMessageReceivedParams),
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn m828c_terminal_title_json_defaults_and_schema() {
        for (raw, stripped) in [
            (None, None),
            (Some(serde_json::Value::Null), Some(serde_json::Value::Null)),
            (Some(serde_json::json!("  \u{25d0} compiling  ")), None),
            (None, Some(serde_json::json!("compiling"))),
            (
                Some(serde_json::json!("  \u{25d0} compiling  ")),
                Some(serde_json::json!("compiling")),
            ),
        ] {
            let mut input = m828b_pane_json();
            let mut expected = m828b_pane_json();
            for (field, value) in [
                ("terminal_title", raw),
                ("terminal_title_stripped", stripped),
            ] {
                if let Some(value) = value {
                    input[field] = value.clone();
                    if !value.is_null() {
                        expected[field] = value;
                    }
                }
            }
            let pane: PaneInfo = serde_json::from_value(input.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(pane).unwrap(),
                expected,
                "pane {input}"
            );
            let agent: AgentInfo = serde_json::from_value(input.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(agent).unwrap(),
                expected,
                "agent {input}"
            );
        }
        for field in ["terminal_title", "terminal_title_stripped"] {
            for value in [
                serde_json::json!(true),
                serde_json::json!(7),
                serde_json::json!([]),
                serde_json::json!({}),
            ] {
                let mut input = m828b_pane_json();
                input[field] = value;
                assert!(
                    serde_json::from_value::<PaneInfo>(input.clone()).is_err(),
                    "pane {input}"
                );
                assert!(
                    serde_json::from_value::<AgentInfo>(input.clone()).is_err(),
                    "agent {input}"
                );
            }
        }
        let document = export::protocol_schema_document();
        for (schema, definition) in [
            ("event", "PaneInfo"),
            ("success_response", "PaneInfo"),
            ("success_response", "AgentInfo"),
        ] {
            let object = &document["schemas"][schema]["$defs"][definition];
            let required = object["required"].as_array().unwrap();
            assert!(required.contains(&serde_json::json!("pane_id")));
            for field in ["terminal_title", "terminal_title_stripped"] {
                assert_eq!(
                    object["properties"][field]["type"],
                    serde_json::json!(["string", "null"]),
                    "{schema}/{definition}/{field}"
                );
                assert!(
                    !required.contains(&serde_json::json!(field)),
                    "{schema}/{definition}/{field}"
                );
            }
        }
        let request_defs = document["schemas"]["request"]["$defs"].as_object().unwrap();
        assert!(request_defs["PaneReportMetadataParams"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("title"));
        for (name, definition) in request_defs {
            if let Some(properties) = definition
                .get("properties")
                .and_then(serde_json::Value::as_object)
            {
                assert!(!properties.contains_key("terminal_title"), "{name}");
                assert!(
                    !properties.contains_key("terminal_title_stripped"),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn m828c_titles_flow_through_existing_pane_events() {
        let mut pane = m828b_pane_json();
        pane["terminal_title"] = serde_json::json!("\u{25d0} compiling");
        pane["terminal_title_stripped"] = serde_json::json!("compiling");
        pane["tokens"] = serde_json::json!({"build": "ok"});
        for (wire, dot) in [
            ("pane_created", "pane.created"),
            ("pane_updated", "pane.updated"),
        ] {
            let event = serde_json::json!({"event": wire, "data": {"type": wire, "pane": pane}});
            let decoded: EventEnvelope = serde_json::from_value(event.clone()).unwrap();
            assert_eq!(decoded.event.dot_name(), dot);
            assert_eq!(serde_json::to_value(decoded).unwrap(), event, "{wire}");
        }
        for result in [
            serde_json::json!({"type": "pane_info", "pane": pane}),
            serde_json::json!({"type": "pane_list", "panes": [pane]}),
            serde_json::json!({"type": "agent_info", "agent": pane}),
            serde_json::json!({"type": "agent_list", "agents": [pane]}),
        ] {
            let response = serde_json::json!({"id": "titles", "result": result});
            let decoded: SuccessResponse = serde_json::from_value(response.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), response);
        }
        let kinds = serde_declared_event_kind_wire_ids();
        assert_eq!(kinds.len(), 22);
        let created = kinds
            .iter()
            .position(|name| name == "pane_created")
            .unwrap();
        assert_eq!(kinds[created + 1], "pane_updated");
        assert!(!plugin_hook_event_names().contains(&"pane.updated"));
        assert!(plugin_hook_event_names().contains(&"pane.created"));
    }

    fn m828b_pane_json() -> serde_json::Value {
        serde_json::json!({
            "pane_id": "w7:p2", "terminal_id": "term_metadata", "workspace_id": "w7",
            "tab_id": "w7:t1", "focused": false, "agent_status": "unknown", "revision": 7
        })
    }

    #[test]
    fn m828b_pane_token_json_and_schema_contract() {
        let value = serde_json::json!({
            "id": "pane-token-schema", "method": "pane.report_metadata",
            "params": {"pane_id": "w7:p2", "source": "user:build", "seq": 0,
                "ttl_ms": 86_400_000, "tokens": {"build": "ok", "old": null},
                "clear_title": false, "clear_display_agent": false,
                "clear_custom_status": false, "clear_state_labels": false}
        });
        let decoded: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
        for missing in ["pane_id", "source"] {
            let mut invalid = value.clone();
            invalid["params"].as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<Request>(invalid).is_err(),
                "{missing}"
            );
        }
        for tokens in [
            serde_json::json!(42),
            serde_json::json!([]),
            serde_json::json!("ignored before B"),
            serde_json::json!({"build": true}),
            serde_json::json!({"build": 42}),
        ] {
            let mut invalid = value.clone();
            invalid["params"]["tokens"] = tokens.clone();
            assert!(
                serde_json::from_value::<Request>(invalid).is_err(),
                "{tokens}"
            );
        }
        for ttl in [0, 86_400_001] {
            let mut legacy = value.clone();
            legacy["params"].as_object_mut().unwrap().remove("tokens");
            legacy["params"]["ttl_ms"] = serde_json::json!(ttl);
            let decoded: Request = serde_json::from_value(legacy.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), legacy);
        }
        for tokens in [
            None,
            Some(serde_json::json!({})),
            Some(serde_json::json!({"build": "ok"})),
        ] {
            let mut pane = m828b_pane_json();
            if let Some(tokens) = &tokens {
                pane["tokens"] = tokens.clone();
            }
            let mut expected = m828b_pane_json();
            if tokens
                .as_ref()
                .is_some_and(|t| !t.as_object().unwrap().is_empty())
            {
                expected["tokens"] = tokens.unwrap();
            }
            let decoded: PaneInfo = serde_json::from_value(pane.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), expected);
            let decoded: AgentInfo = serde_json::from_value(pane).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), expected);
        }
        let document = export::protocol_schema_document();
        let params = &document["schemas"]["request"]["$defs"]["PaneReportMetadataParams"];
        assert_eq!(params["required"], serde_json::json!(["pane_id", "source"]));
        let tokens = &params["properties"]["tokens"];
        assert_eq!(tokens["maxProperties"], 16);
        assert_eq!(tokens["propertyNames"]["pattern"], "^[A-Za-z0-9_-]{1,32}$");
        assert_eq!(
            tokens["additionalProperties"]["type"],
            serde_json::json!(["string", "null"])
        );
        let optional_u64 = serde_json::to_value(schemars::schema_for!(Option<u64>)).unwrap();
        for key in ["type", "format", "minimum", "maximum"] {
            assert_eq!(
                params["properties"]["ttl_ms"][key], optional_u64[key],
                "{key}"
            );
        }
        let workspace = &document["schemas"]["request"]["$defs"]["WorkspaceReportMetadataParams"];
        assert_eq!(workspace["properties"]["ttl_ms"]["minimum"], 1);
        assert_eq!(workspace["properties"]["ttl_ms"]["maximum"], 86_400_000);
        for name in ["PaneInfo", "AgentInfo"] {
            let values =
                &document["schemas"]["success_response"]["$defs"][name]["properties"]["tokens"];
            assert_eq!(values["maxProperties"], 32, "{name}");
            assert_eq!(values["additionalProperties"]["type"], "string", "{name}");
        }
    }

    #[test]
    fn m828b_pane_updated_event_contract_and_plugin_exclusion() {
        let subscription = serde_json::json!({"type": "pane.updated"});
        let decoded = serde_json::from_value::<Subscription>(subscription.clone());
        assert!(
            decoded.is_ok(),
            "pane subscription JSON refused: {decoded:?}"
        );
        assert_eq!(
            serde_json::to_value(decoded.unwrap()).unwrap(),
            subscription
        );
        let mut pane = m828b_pane_json();
        pane["tokens"] = serde_json::json!({"build": "ok"});
        let event = serde_json::json!({"event": "pane_updated", "data": {
            "type": "pane_updated", "pane": pane
        }});
        let decoded: EventEnvelope = serde_json::from_value(event.clone()).unwrap();
        assert_eq!(decoded.event.dot_name(), "pane.updated");
        assert_eq!(serde_json::to_value(decoded).unwrap(), event);
        assert!(!plugin_hook_event_names().contains(&"pane.updated"));
        assert!(plugin_hook_event_names().contains(&"pane.created"));
    }

    fn m828a_workspace_json() -> serde_json::Value {
        serde_json::json!({
            "workspace_id": "w7", "number": 2, "label": "background", "focused": false,
            "pane_count": 1, "tab_count": 1, "active_tab_id": "w7:t3", "agent_status": "unknown"
        })
    }

    #[test]
    fn m828a_workspace_metadata_json_and_schema_contract() {
        let value = serde_json::json!({
            "id": "metadata-schema", "method": "workspace.report_metadata",
            "params": {"workspace_id": "w7", "source": "user:build", "seq": 0,
                "ttl_ms": 86_400_000, "tokens": {"build": "ok", "old": null}}
        });
        let request = serde_json::from_value::<Request>(value.clone());
        assert!(
            request.is_ok(),
            "workspace report JSON refused: {request:?}"
        );
        assert_eq!(serde_json::to_value(request.unwrap()).unwrap(), value);
        for missing in ["workspace_id", "source", "tokens"] {
            let mut invalid = value.clone();
            invalid["params"].as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<Request>(invalid).is_err(),
                "{missing}"
            );
        }
        let document = export::protocol_schema_document();
        let params = &document["schemas"]["request"]["$defs"]["WorkspaceReportMetadataParams"];
        assert_eq!(
            params["required"],
            serde_json::json!(["workspace_id", "source", "tokens"])
        );
        let tokens = &params["properties"]["tokens"];
        assert_eq!(tokens["maxProperties"], 16);
        assert_eq!(tokens["propertyNames"]["pattern"], "^[A-Za-z0-9_-]{1,32}$");
        assert_eq!(
            tokens["additionalProperties"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert_eq!(params["properties"]["ttl_ms"]["minimum"], 1);
        assert_eq!(params["properties"]["ttl_ms"]["maximum"], 86_400_000);
        let values = &document["schemas"]["success_response"]["$defs"]["WorkspaceInfo"]
            ["properties"]["tokens"];
        assert_eq!(values["maxProperties"], 32);
        assert_eq!(values["additionalProperties"]["type"], "string");
    }

    #[test]
    fn m828a_workspace_metadata_event_json_is_known_but_not_a_plugin_hook() {
        let subscription = serde_json::json!({"type": "workspace.metadata_updated"});
        let decoded = serde_json::from_value::<Subscription>(subscription.clone());
        assert!(
            decoded.is_ok(),
            "workspace subscription JSON refused: {decoded:?}"
        );
        assert_eq!(
            serde_json::to_value(decoded.unwrap()).unwrap(),
            subscription
        );
        let mut workspace = m828a_workspace_json();
        workspace["tokens"] = serde_json::json!({"build": "ok"});
        let event = serde_json::json!({
            "event": "workspace_metadata_updated",
            "data": {"type": "workspace_metadata_updated", "workspace": workspace}
        });
        let decoded: EventEnvelope = serde_json::from_value(event.clone()).unwrap();
        assert_eq!(decoded.event.dot_name(), "workspace.metadata_updated");
        assert_eq!(serde_json::to_value(decoded).unwrap(), event);
        assert!(!plugin_hook_event_names().contains(&"workspace.metadata_updated"));
        assert!(plugin_hook_event_names().contains(&"workspace.created"));
    }

    #[test]
    fn m828a_empty_workspace_tokens_remain_omitted() {
        let old = m828a_workspace_json();
        let decoded: WorkspaceInfo = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), old);
        let mut explicit = old.clone();
        explicit["tokens"] = serde_json::json!({});
        let decoded: WorkspaceInfo = serde_json::from_value(explicit).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), old);
    }

    #[test]
    fn api_schema_bundle_has_all_five_roots() {
        let document = export::protocol_schema_document();
        let names = document["schemas"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "error_response",
                "event",
                "request",
                "subscription_event",
                "success_response"
            ]
        );
    }

    #[test]
    fn m813_layout_event_and_subscription_preserve_canonical_wire_shapes() {
        let request = serde_json::json!({
            "id": "layout-sub", "method": "events.subscribe",
            "params": {"subscriptions": [{"type": "layout.updated"}]}
        });
        let decoded: Request = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), request);
        let event = serde_json::json!({
            "event": "layout_updated",
            "data": {"type": "layout_updated", "layout": {
                "workspace_id": "w7", "tab_id": "w7:t3", "zoomed": true,
                "area": {"x": 3, "y": 4, "width": 97, "height": 23},
                "focused_pane_id": "w7:p8", "panes": [{
                    "pane_id": "w7:p8", "focused": true,
                    "rect": {"x": 3, "y": 4, "width": 97, "height": 23}
                }], "splits": [{"id": "split_0_root", "direction": "right",
                    "ratio": 0.625, "rect": {"x": 3, "y": 4, "width": 97, "height": 23}}]
            }}
        });
        let decoded: EventEnvelope = serde_json::from_value(event.clone()).unwrap();
        assert_eq!(decoded.event, EventKind::LayoutUpdated);
        assert_eq!(decoded.event.dot_name(), "layout.updated");
        assert_eq!(serde_json::to_value(decoded).unwrap(), event);
        assert!(!plugin_hook_event_names().contains(&"layout.updated"));
        assert!(plugin_hook_event_names().contains(&"pane.created"));
        fn requires_eq<T: Eq>() {}
        requires_eq::<SubscriptionEventEnvelope>();
        requires_eq::<SubscriptionEventData>();
    }

    #[test]
    fn m821_scroll_subscription_request_wire_shape() {
        let value = serde_json::json!({
            "id": "scroll-request", "method": "events.subscribe",
            "params": {"subscriptions": [{"type": "pane.scroll_changed", "pane_id": "w2:p4"}]}
        });
        let request: Request =
            serde_json::from_value(value.clone()).expect("scroll subscription must decode");
        assert_eq!(serde_json::to_value(request).unwrap(), value);
        let missing = serde_json::json!({
            "id": "scroll-missing-target", "method": "events.subscribe",
            "params": {"subscriptions": [{"type": "pane.scroll_changed"}]}
        });
        assert!(serde_json::from_value::<Request>(missing).is_err());
    }

    #[test]
    fn m821_scroll_event_round_trip_requires_all_metrics() {
        let value = serde_json::json!({
            "event": "pane.scroll_changed", "data": {
                "pane_id": "w2:p4", "workspace_id": "w2",
                "scroll": {"offset_from_bottom": 12, "max_offset_from_bottom": 240,
                    "viewport_rows": 30}
            }
        });
        let event: SubscriptionEventEnvelope = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(event.event, SubscriptionEventKind::ScrollChanged);
        assert_eq!(serde_json::to_value(event).unwrap(), value);
        for field in [
            "offset_from_bottom",
            "max_offset_from_bottom",
            "viewport_rows",
        ] {
            let mut missing = value.clone();
            missing["data"]["scroll"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                serde_json::from_value::<SubscriptionEventEnvelope>(missing).is_err(),
                "{field}"
            );
        }
        fn requires_eq<T: Eq>() {}
        requires_eq::<PaneScrollInfo>();
        requires_eq::<SubscriptionEventEnvelope>();
        requires_eq::<SubscriptionEventData>();
    }

    #[test]
    fn m821_old_pane_json_omits_scroll_until_available() {
        let old = serde_json::json!({
            "pane_id": "w2:p4", "terminal_id": "term_4", "workspace_id": "w2",
            "tab_id": "w2:t1", "focused": false, "agent_status": "unknown", "revision": 7
        });
        let mut pane: PaneInfo = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(pane.scroll, None);
        assert_eq!(serde_json::to_value(&pane).unwrap(), old);
        pane.scroll = Some(PaneScrollInfo {
            offset_from_bottom: 0,
            max_offset_from_bottom: 42,
            viewport_rows: 24,
        });
        let mut expected = old;
        expected["scroll"] = serde_json::json!({
            "offset_from_bottom": 0, "max_offset_from_bottom": 42, "viewport_rows": 24
        });
        assert_eq!(serde_json::to_value(pane).unwrap(), expected);
    }

    #[test]
    fn m821_dynamic_schema_exposes_scroll_only_as_parameterized_subscription() {
        let document = export::protocol_schema_document();
        let subscriptions = &document["schemas"]["request"]["$defs"]["Subscription"]["oneOf"];
        let scroll = subscriptions
            .as_array()
            .unwrap()
            .iter()
            .find(|variant| {
                variant.pointer("/properties/type/const")
                    == Some(&serde_json::json!("pane.scroll_changed"))
            })
            .expect("scroll subscription schema");
        assert!(scroll["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("pane_id")));
        let defs = &document["schemas"]["subscription_event"]["$defs"];
        assert_eq!(
            defs["PaneScrollInfo"]["required"],
            serde_json::json!([
                "offset_from_bottom",
                "max_offset_from_bottom",
                "viewport_rows"
            ])
        );
        assert_eq!(
            defs["PaneScrollChangedEvent"]["required"],
            serde_json::json!(["pane_id", "workspace_id", "scroll"])
        );
        assert!(!document["schemas"]["event"]["$defs"]["EventKind"]["enum"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("scroll_changed")));
        assert!(!plugin_hook_event_names().contains(&"pane.scroll_changed"));
        assert_eq!(document["protocol"], crate::protocol::PROTOCOL_VERSION);
    }

    #[test]
    fn m813_dynamic_schema_includes_layout_event_and_snapshot_fields() {
        let document = export::protocol_schema_document();
        let definitions = &document["schemas"]["event"]["$defs"];
        assert!(definitions["EventKind"]["enum"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("layout_updated")));
        assert!(definitions["EventData"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|variant| variant.pointer("/properties/type/const")
                == Some(&serde_json::json!("layout_updated"))
                && variant["required"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("layout"))));
        assert!(definitions["PaneLayoutSnapshot"]["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("focused_pane_id")));
        assert_eq!(document["protocol"], crate::protocol::PROTOCOL_VERSION);
    }

    #[test]
    fn api_schema_bundle_metadata_uses_current_protocol() {
        let document = export::protocol_schema_document();
        assert_eq!(document["protocol"], crate::protocol::PROTOCOL_VERSION);
        assert_eq!(document["schema_version"], 1);
        assert_eq!(document["title"], "Zynk API");
        assert_eq!(
            document["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
    }

    #[test]
    fn api_schema_bundle_every_reference_resolves() {
        let document = export::protocol_schema_document();
        let mut pending = vec![&document];
        let mut references = 0;
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(reference) = object.get("$ref") {
                        let reference = reference.as_str().unwrap();
                        assert!(reference.starts_with("#/schemas/"), "{reference}");
                        let pointer = reference.strip_prefix('#').unwrap();
                        assert!(
                            document.pointer(pointer).is_some(),
                            "unresolved {reference}"
                        );
                        references += 1;
                    }
                    pending.extend(object.values());
                }
                serde_json::Value::Array(items) => pending.extend(items),
                _ => {}
            }
        }
        assert!(references > 0);
    }

    #[test]
    fn api_schema_bundle_includes_fork_receipt_and_session_methods() {
        let document = export::protocol_schema_document();
        let mut pending = vec![&document["schemas"]["request"]];
        let mut methods = Vec::new();
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(method) = value
                        .pointer("/properties/method/const")
                        .and_then(serde_json::Value::as_str)
                    {
                        methods.push(method);
                    }
                    pending.extend(object.values());
                }
                serde_json::Value::Array(items) => pending.extend(items),
                _ => {}
            }
        }
        for method in [
            "zynk.message_received",
            "session.snapshot",
            "pane.send_input",
            "pane.read",
        ] {
            assert!(methods.contains(&method), "missing {method}");
        }
        let receipt = &document["schemas"]["request"]["$defs"]["ZynkMessageReceivedParams"];
        assert!(receipt["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("runtime_session_id")));
        assert!(receipt["properties"]
            .get("receiver_agent_session")
            .is_some());
    }

    #[test]
    fn request_uses_dot_method_names() {
        let request = Request {
            id: "req_1".into(),
            method: Method::WorkspaceCreate(WorkspaceCreateParams {
                cwd: Some("/tmp".into()),
                focus: true,
                label: Some("api".into()),
            }),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["method"], "workspace.create");
    }

    #[test]
    fn request_round_trips_for_server_stop() {
        let request = Request {
            id: "req_stop".into(),
            method: Method::ServerStop(EmptyParams::default()),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["method"], "server.stop");
        let restored: Request = serde_json::from_value(json).unwrap();
        assert_eq!(restored, request);
    }

    #[test]
    fn request_round_trips_for_server_reload_config() {
        let request = Request {
            id: "req_reload".into(),
            method: Method::ServerReloadConfig(EmptyParams::default()),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["method"], "server.reload_config");
        let restored: Request = serde_json::from_value(json).unwrap();
        assert_eq!(restored, request);
    }

    #[test]
    fn request_round_trips_for_server_reload_agent_manifests() {
        let request = Request {
            id: "req_reload_agent_manifests".into(),
            method: Method::ServerReloadAgentManifests(EmptyParams::default()),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["method"], "server.reload_agent_manifests");
        let restored: Request = serde_json::from_value(json).unwrap();
        assert_eq!(restored, request);
    }

    #[test]
    fn request_round_trips_for_server_agent_manifests() {
        let request = Request {
            id: "req_agent_manifests".into(),
            method: Method::ServerAgentManifests(EmptyParams::default()),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["method"], "server.agent_manifests");
        let restored: Request = serde_json::from_value(json).unwrap();
        assert_eq!(restored, request);
    }

    #[test]
    fn request_round_trips_for_agent_explain() {
        let request = Request {
            id: "req_agent_explain".into(),
            method: Method::AgentExplain(AgentTarget {
                target: "agent-1".into(),
            }),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["method"], "agent.explain");
        let restored: Request = serde_json::from_value(json).unwrap();
        assert_eq!(restored, request);
    }

    #[test]
    fn notification_show_request_parses() {
        let json = r#"{"id":"req_1","method":"notification.show","params":{"title":"build failed","body":"api workspace","position":"top-left","sound":"request"}}"#;
        let request: Request = serde_json::from_str(json).unwrap();
        let Method::NotificationShow(params) = request.method else {
            panic!("wrong method parsed");
        };
        assert_eq!(params.title, "build failed");
        assert_eq!(params.body.as_deref(), Some("api workspace"));
        assert_eq!(
            params.position,
            Some(crate::config::ToastZynkPosition::TopLeft)
        );
        assert_eq!(params.sound, NotificationShowSound::Request);
    }

    #[test]
    fn notification_show_sound_defaults_to_none() {
        let json =
            r#"{"id":"req_1","method":"notification.show","params":{"title":"build failed"}}"#;
        let request: Request = serde_json::from_str(json).unwrap();
        let Method::NotificationShow(params) = request.method else {
            panic!("wrong method parsed");
        };

        assert_eq!(params.sound, NotificationShowSound::None);
    }

    #[test]
    fn unknown_method_is_rejected() {
        let json = r#"{"id":"req_1","method":"nope","params":{}}"#;
        let err = serde_json::from_str::<Request>(json)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown variant"));
    }

    #[test]
    fn missing_required_params_are_rejected() {
        let json = r#"{"id":"req_1","method":"pane.send_text","params":{"pane_id":"p_1"}}"#;
        let err = serde_json::from_str::<Request>(json)
            .unwrap_err()
            .to_string();
        assert!(err.contains("text"));
    }

    #[test]
    fn pane_send_input_defaults_to_empty_text_and_keys() {
        let json = r#"
        {
            "id": "req_1",
            "method": "pane.send_input",
            "params": {
                "pane_id": "p_1"
            }
        }
        "#;

        let request: Request = serde_json::from_str(json).unwrap();
        let Method::PaneSendInput(params) = request.method else {
            panic!("wrong method parsed");
        };
        assert_eq!(params.pane_id, "p_1");
        assert!(params.text.is_empty());
        assert!(params.keys.is_empty());
    }

    #[test]
    fn pane_wait_for_output_defaults_strip_ansi_to_true() {
        let json = r#"
        {
            "id": "req_1",
            "method": "pane.wait_for_output",
            "params": {
                "pane_id": "p_1",
                "source": "recent",
                "match": { "type": "substring", "value": "ready" }
            }
        }
        "#;

        let request: Request = serde_json::from_str(json).unwrap();
        let Method::PaneWaitForOutput(params) = request.method else {
            panic!("wrong method parsed");
        };
        assert!(params.strip_ansi);
    }

    #[test]
    fn pane_read_defaults_to_text_format() {
        let json = r#"
        {
            "id": "req_1",
            "method": "pane.read",
            "params": {
                "pane_id": "p_1",
                "source": "visible"
            }
        }
        "#;

        let request: Request = serde_json::from_str(json).unwrap();
        let Method::PaneRead(params) = request.method else {
            panic!("wrong method parsed");
        };
        assert_eq!(params.format, ReadFormat::Text);
    }

    #[test]
    fn event_envelope_round_trips() {
        let events = [
            EventEnvelope {
                event: EventKind::PaneOutputChanged,
                data: EventData::PaneOutputChanged {
                    pane_id: "w1:p1".into(),
                    workspace_id: "w1".into(),
                    revision: 42,
                },
            },
            EventEnvelope {
                event: EventKind::WorkspaceMoved,
                data: EventData::WorkspaceMoved {
                    workspace_id: "w1".into(),
                    insert_index: 2,
                    workspaces: vec![],
                },
            },
            EventEnvelope {
                event: EventKind::TabMoved,
                data: EventData::TabMoved {
                    tab_id: "w1:1".into(),
                    workspace_id: "w1".into(),
                    insert_index: 1,
                    tabs: vec![],
                },
            },
        ];

        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let restored: EventEnvelope = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, event);
        }
    }

    #[test]
    fn subscribe_request_parses_parameterized_subscriptions() {
        let json = r#"
        {
            "id": "sub_1",
            "method": "events.subscribe",
            "params": {
                "subscriptions": [
                    {
                        "type": "pane.output_matched",
                        "pane_id": "p_1_1",
                        "source": "recent",
                        "lines": 200,
                        "match": { "type": "substring", "value": "auth: received" }
                    },
                    {
                        "type": "pane.agent_status_changed",
                        "pane_id": "p_1_1",
                        "agent_status": "done"
                    }
                ]
            }
        }
        "#;

        let request: Request = serde_json::from_str(json).unwrap();
        let Method::EventsSubscribe(params) = request.method else {
            panic!("wrong method parsed");
        };
        assert_eq!(params.subscriptions.len(), 2);
        assert!(matches!(
            &params.subscriptions[0],
            Subscription::PaneOutputMatched {
                pane_id,
                source: ReadSource::Recent,
                lines: Some(200),
                r#match: OutputMatch::Substring { value },
                strip_ansi: true,
            } if pane_id == "p_1_1" && value == "auth: received"
        ));
        assert!(matches!(
            &params.subscriptions[1],
            Subscription::PaneAgentStatusChanged {
                pane_id,
                agent_status: Some(AgentStatus::Done),
            } if pane_id == "p_1_1"
        ));
    }

    #[test]
    fn subscription_event_envelope_round_trips() {
        let event = SubscriptionEventEnvelope {
            event: SubscriptionEventKind::PaneOutputMatched,
            data: SubscriptionEventData::PaneOutputMatched(PaneOutputMatchedEvent {
                pane_id: "w1:p1".into(),
                matched_line: "auth: received".into(),
                read: PaneReadResult {
                    pane_id: "w1:p1".into(),
                    workspace_id: "w1".into(),
                    tab_id: "w1:t1".into(),
                    source: ReadSource::Recent,
                    format: ReadFormat::Text,
                    text: "auth: received\n".into(),
                    revision: 0,
                    truncated: false,
                },
            }),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"pane.output_matched\""));
        let restored: SubscriptionEventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, event);
    }

    #[test]
    fn success_response_round_trips() {
        let response = SuccessResponse {
            id: "req_1".into(),
            result: ResponseResult::Pong {
                version: "0.1.2".into(),
                protocol: 6,
                capabilities: Some(ServerCapabilities {
                    live_handoff: true,
                    detached_server_daemon: true,
                }),
            },
        };

        let json = serde_json::to_string(&response).unwrap();
        let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, response);
    }

    #[test]
    fn layout_split_ratio_set_response_round_trips() {
        let response = SuccessResponse {
            id: "layout_ratio".into(),
            result: ResponseResult::LayoutSplitRatioSet {
                layout: LayoutDescription {
                    workspace_id: "w1".into(),
                    tab_id: "w1:1".into(),
                    zoomed: false,
                    focused_pane_id: "w1-1".into(),
                    root: LayoutNode::Pane {
                        pane: LayoutPane {
                            pane_id: Some("w1-1".into()),
                            ..Default::default()
                        },
                    },
                },
            },
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"type\":\"layout_split_ratio_set\""));
        let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, response);
    }

    #[test]
    fn worktree_request_and_response_round_trip() {
        let request = Request {
            id: "req_worktree".into(),
            method: Method::WorktreeCreate(WorktreeCreateParams {
                workspace_id: Some("1".into()),
                branch: Some("worktree/api".into()),
                base: Some("HEAD".into()),
                focus: true,
                ..WorktreeCreateParams::default()
            }),
        };
        let json = serde_json::to_string(&request).unwrap();
        let restored: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, request);

        let response = SuccessResponse {
            id: "req_worktree".into(),
            result: ResponseResult::WorktreeCreated {
                workspace: WorkspaceInfo {
                    workspace_id: "w1".into(),
                    number: 2,
                    label: "zynk".into(),
                    focused: true,
                    pane_count: 1,
                    tab_count: 1,
                    active_tab_id: "w1:t1".into(),
                    agent_status: AgentStatus::Unknown,
                    tokens: HashMap::new(),
                    worktree: Some(WorkspaceWorktreeInfo {
                        repo_key: "/repo/zynk/.git".into(),
                        repo_name: "zynk".into(),
                        repo_root: "/repo/zynk".into(),
                        checkout_path: "/worktrees/zynk/worktree-api".into(),
                        is_linked_worktree: true,
                    }),
                },
                tab: TabInfo {
                    tab_id: "w1:t1".into(),
                    workspace_id: "w1".into(),
                    number: 1,
                    label: "zynk".into(),
                    focused: true,
                    pane_count: 1,
                    agent_status: AgentStatus::Unknown,
                },
                root_pane: PaneInfo {
                    pane_id: "w1:p1".into(),
                    terminal_id: "term_1".into(),
                    workspace_id: "w1".into(),
                    tab_id: "w1:t1".into(),
                    focused: true,
                    cwd: Some("/worktrees/zynk/worktree-api".into()),
                    foreground_cwd: None,
                    label: None,
                    agent: None,
                    title: None,
                    terminal_title: None,
                    terminal_title_stripped: None,
                    display_agent: None,
                    agent_status: AgentStatus::Unknown,
                    custom_status: None,
                    state_labels: HashMap::new(),
                    tokens: HashMap::new(),
                    agent_session: None,
                    scroll: None,
                    revision: 0,
                },
                worktree: WorktreeInfo {
                    path: "/worktrees/zynk/worktree-api".into(),
                    branch: Some("worktree/api".into()),
                    is_bare: false,
                    is_detached: false,
                    is_prunable: false,
                    is_linked_worktree: true,
                    open_workspace_id: Some("w1".into()),
                    label: "zynk".into(),
                },
            },
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"type\":\"worktree_created\""));
        assert!(json.contains("\"worktree\""));
        let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, response);
    }

    #[test]
    fn create_response_round_trips_with_root_pane() {
        let response = SuccessResponse {
            id: "req_2".into(),
            result: ResponseResult::TabCreated {
                tab: TabInfo {
                    tab_id: "w1:t2".into(),
                    workspace_id: "w1".into(),
                    number: 2,
                    label: "review".into(),
                    focused: false,
                    pane_count: 1,
                    agent_status: AgentStatus::Unknown,
                },
                root_pane: PaneInfo {
                    pane_id: "w1:p3".into(),
                    terminal_id: "term_example".into(),
                    workspace_id: "w1".into(),
                    tab_id: "w1:t2".into(),
                    focused: false,
                    cwd: Some("/tmp/review".into()),
                    foreground_cwd: None,
                    label: None,
                    agent: None,
                    title: None,
                    terminal_title: None,
                    terminal_title_stripped: None,
                    display_agent: None,
                    agent_status: AgentStatus::Unknown,
                    custom_status: None,
                    state_labels: HashMap::new(),
                    tokens: HashMap::new(),
                    agent_session: None,
                    scroll: None,
                    revision: 0,
                },
            },
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"type\":\"tab_created\""));
        assert!(json.contains("\"root_pane\""));
        let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, response);
    }

    #[test]
    fn error_response_round_trips() {
        let response = ErrorResponse {
            id: "req_1".into(),
            error: ErrorBody {
                code: "pane_not_found".into(),
                message: "pane p_1 not found".into(),
            },
        };

        let json = serde_json::to_string(&response).unwrap();
        let restored: ErrorResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, response);
    }

    #[test]
    fn event_wait_parses_typed_match() {
        let json = r#"
        {
            "id": "req_9",
            "method": "events.wait",
            "params": {
                "match_event": {
                    "event": "pane_agent_status_changed",
                    "pane_id": "p_1",
                    "agent_status": "done"
                },
                "timeout_ms": 30000
            }
        }
        "#;

        let request: Request = serde_json::from_str(json).unwrap();
        let Method::EventsWait(params) = request.method else {
            panic!("wrong method parsed");
        };
        assert_eq!(
            params.match_event,
            EventMatch::PaneAgentStatusChanged {
                pane_id: "p_1".into(),
                agent_status: AgentStatus::Done,
            }
        );
    }

    /// Every wire id `serde` will accept for a [`Method`], in declaration order.
    /// Each entry is a published socket value: renaming one, reordering the enum,
    /// or adding a variant without pinning its id here silently breaks existing
    /// clients. Mirrors `integration_target_wire_ids_are_stable`
    /// (`src/api/schema/integrations.rs`) for the method contract itself.
    const METHOD_WIRE_IDS: &[&str] = &[
        "ping",
        "server.stop",
        "server.live_handoff",
        "server.reload_config",
        "server.agent_manifests",
        "server.reload_agent_manifests",
        "session.snapshot",
        "notification.show",
        "client.window_title.set",
        "client.window_title.clear",
        "workspace.create",
        "workspace.list",
        "workspace.get",
        "workspace.focus",
        "workspace.rename",
        "workspace.move",
        "workspace.report_metadata",
        "workspace.close",
        "worktree.list",
        "worktree.create",
        "worktree.open",
        "worktree.remove",
        "tab.create",
        "tab.list",
        "tab.get",
        "tab.focus",
        "tab.rename",
        "tab.move",
        "tab.close",
        "agent.list",
        "agent.get",
        "agent.read",
        "agent.explain",
        "agent.send",
        "agent.rename",
        "agent.focus",
        "agent.start",
        "pane.split",
        "pane.swap",
        "pane.move",
        "pane.zoom",
        "pane.layout",
        "pane.process_info",
        "layout.export",
        "layout.apply",
        "layout.set_split_ratio",
        "pane.neighbor",
        "pane.edges",
        "pane.focus_direction",
        "pane.resize",
        "pane.list",
        "pane.current",
        "pane.get",
        "pane.focus",
        "pane.rename",
        "pane.send_text",
        "pane.send_keys",
        "pane.send_input",
        "pane.read",
        "pane.report_agent",
        "pane.report_agent_session",
        "pane.report_metadata",
        "pane.clear_agent_authority",
        "pane.release_agent",
        "pane.close",
        "events.subscribe",
        "events.wait",
        "pane.wait_for_output",
        "integration.install",
        "integration.uninstall",
        "plugin.link",
        "plugin.list",
        "plugin.unlink",
        "plugin.enable",
        "plugin.disable",
        "plugin.action.list",
        "plugin.action.invoke",
        "plugin.log.list",
        "plugin.pane.open",
        "plugin.pane.focus",
        "plugin.pane.close",
        "zynk.message_received",
    ];

    /// The ids `serde` itself reports as acceptable for the `method` tag, taken
    /// from the `unknown variant` error so the list cannot drift from the enum.
    fn serde_declared_method_wire_ids() -> Vec<String> {
        let error = serde_json::from_value::<Method>(serde_json::json!({
            "method": "zynk.__not_a_method__",
            "params": {}
        }))
        .expect_err("an unknown method tag must not deserialize")
        .to_string();
        let (_, expected) = error
            .split_once("expected")
            .unwrap_or_else(|| panic!("serde must report the accepted variants: {error}"));
        expected
            .split('`')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn method_wire_ids_are_stable() {
        assert_eq!(
            serde_declared_method_wire_ids(),
            METHOD_WIRE_IDS,
            "the Method wire contract changed: every id is published to clients"
        );
    }

    /// Every wire id `serde` will accept for an [`EventKind`], in declaration
    /// order, paired with the dot name published on the socket and to plugin
    /// event hooks. Both columns are published values: renaming one, reordering
    /// the enum, or adding a variant without pinning it here silently breaks
    /// existing subscribers. Mirrors `method_wire_ids_are_stable`
    /// (this module) for the event contract.
    const EVENT_KIND_WIRE_IDS: &[(EventKind, &str, &str)] = &[
        (
            EventKind::WorkspaceCreated,
            "workspace_created",
            "workspace.created",
        ),
        (
            EventKind::WorkspaceUpdated,
            "workspace_updated",
            "workspace.updated",
        ),
        (
            EventKind::WorkspaceMetadataUpdated,
            "workspace_metadata_updated",
            "workspace.metadata_updated",
        ),
        (
            EventKind::WorkspaceClosed,
            "workspace_closed",
            "workspace.closed",
        ),
        (
            EventKind::WorkspaceRenamed,
            "workspace_renamed",
            "workspace.renamed",
        ),
        (
            EventKind::WorkspaceMoved,
            "workspace_moved",
            "workspace.moved",
        ),
        (
            EventKind::WorkspaceFocused,
            "workspace_focused",
            "workspace.focused",
        ),
        (EventKind::TabCreated, "tab_created", "tab.created"),
        (EventKind::TabClosed, "tab_closed", "tab.closed"),
        (EventKind::TabRenamed, "tab_renamed", "tab.renamed"),
        (EventKind::TabMoved, "tab_moved", "tab.moved"),
        (EventKind::TabFocused, "tab_focused", "tab.focused"),
        (EventKind::PaneCreated, "pane_created", "pane.created"),
        (EventKind::PaneUpdated, "pane_updated", "pane.updated"),
        (EventKind::PaneClosed, "pane_closed", "pane.closed"),
        (EventKind::PaneFocused, "pane_focused", "pane.focused"),
        (EventKind::PaneMoved, "pane_moved", "pane.moved"),
        (
            EventKind::PaneOutputChanged,
            "pane_output_changed",
            "pane.output_changed",
        ),
        (EventKind::PaneExited, "pane_exited", "pane.exited"),
        (
            EventKind::PaneAgentDetected,
            "pane_agent_detected",
            "pane.agent_detected",
        ),
        (
            EventKind::PaneAgentStatusChanged,
            "pane_agent_status_changed",
            "pane.agent_status_changed",
        ),
        (EventKind::LayoutUpdated, "layout_updated", "layout.updated"),
    ];

    /// The ids `serde` itself reports as acceptable for an [`EventKind`], taken
    /// from the `unknown variant` error so the list cannot drift from the enum.
    fn serde_declared_event_kind_wire_ids() -> Vec<String> {
        let error = serde_json::from_value::<EventKind>(serde_json::json!("__not_an_event__"))
            .expect_err("an unknown event kind must not deserialize")
            .to_string();
        let (_, expected) = error
            .split_once("expected")
            .unwrap_or_else(|| panic!("serde must report the accepted variants: {error}"));
        expected
            .split('`')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn event_kind_wire_ids_are_stable() {
        let pinned: Vec<String> = EVENT_KIND_WIRE_IDS
            .iter()
            .map(|(_, wire_id, _)| (*wire_id).to_string())
            .collect();
        assert_eq!(
            serde_declared_event_kind_wire_ids(),
            pinned,
            "the EventKind wire contract changed: every id is published to subscribers"
        );

        for (kind, wire_id, dot_name) in EVENT_KIND_WIRE_IDS {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(wire_id),
                "{wire_id} must serialize as its wire id"
            );
            assert_eq!(
                kind.dot_name(),
                *dot_name,
                "{wire_id} must keep its published dot name"
            );
        }

        let hook_dot_names: Vec<&str> = PLUGIN_HOOK_EVENT_KINDS
            .iter()
            .copied()
            .map(EventKind::dot_name)
            .collect();
        let expected_hook_dot_names: Vec<&str> = EVENT_KIND_WIRE_IDS
            .iter()
            .filter(|(kind, _, _)| {
                !matches!(
                    kind,
                    EventKind::PaneOutputChanged
                        | EventKind::LayoutUpdated
                        | EventKind::WorkspaceMetadataUpdated
                        | EventKind::PaneUpdated
                )
            })
            .map(|(_, _, dot_name)| *dot_name)
            .collect();
        assert_eq!(
            hook_dot_names, expected_hook_dot_names,
            "plugin event hooks must exclude pane.output_changed, layout.updated, workspace.metadata_updated and pane.updated"
        );
    }

    /// The four runtime-authority mutation methods (upstream 1a4e94e5) named by
    /// variant, so removing or renaming one is a compile error here and a wire
    /// break is caught by the assertions.
    #[test]
    fn authority_mutation_method_wire_ids_are_pinned() {
        let samples = [
            (
                Method::WorkspaceMove(WorkspaceMoveParams {
                    workspace_id: "w1".into(),
                    insert_index: 2,
                }),
                "workspace.move",
            ),
            (
                Method::TabMove(TabMoveParams {
                    tab_id: "w1:1".into(),
                    insert_index: 1,
                }),
                "tab.move",
            ),
            (
                Method::LayoutSetSplitRatio(LayoutSetSplitRatioParams {
                    tab_id: Some("w1:1".into()),
                    pane_id: None,
                    path: vec![false, true],
                    ratio: 0.6,
                }),
                "layout.set_split_ratio",
            ),
            (
                Method::PaneFocus(PaneTarget {
                    pane_id: "w1:1".into(),
                }),
                "pane.focus",
            ),
        ];

        for (method, wire_id) in samples {
            assert!(
                METHOD_WIRE_IDS.contains(&wire_id),
                "{wire_id} must be pinned in METHOD_WIRE_IDS"
            );
            let request = Request {
                id: "req_authority".into(),
                method,
            };
            let json = serde_json::to_value(&request).unwrap();
            assert_eq!(
                json["method"], wire_id,
                "{wire_id} must serialize as its wire id"
            );
            let restored: Request = serde_json::from_value(json).unwrap();
            assert_eq!(restored, request, "{wire_id} must round trip");
        }
    }

    #[test]
    fn authority_mutation_requests_round_trip() {
        let workspace_move = Request {
            id: "move_ws".into(),
            method: Method::WorkspaceMove(WorkspaceMoveParams {
                workspace_id: "w1".into(),
                insert_index: 2,
            }),
        };
        let json = serde_json::to_value(&workspace_move).unwrap();
        assert_eq!(json["method"], "workspace.move");
        let restored: Request = serde_json::from_value(json).unwrap();
        assert_eq!(restored, workspace_move);

        let tab_move = Request {
            id: "move_tab".into(),
            method: Method::TabMove(TabMoveParams {
                tab_id: "w1:1".into(),
                insert_index: 1,
            }),
        };
        let json = serde_json::to_value(&tab_move).unwrap();
        assert_eq!(json["method"], "tab.move");
        let restored: Request = serde_json::from_value(json).unwrap();
        assert_eq!(restored, tab_move);

        let pane_focus = Request {
            id: "focus_pane".into(),
            method: Method::PaneFocus(PaneTarget {
                pane_id: "w1:1".into(),
            }),
        };
        let json = serde_json::to_value(&pane_focus).unwrap();
        assert_eq!(json["method"], "pane.focus");
        let restored: Request = serde_json::from_value(json).unwrap();
        assert_eq!(restored, pane_focus);

        let split_ratio = Request {
            id: "set_ratio".into(),
            method: Method::LayoutSetSplitRatio(LayoutSetSplitRatioParams {
                tab_id: Some("w1:1".into()),
                pane_id: None,
                path: vec![false, true],
                ratio: 0.6,
            }),
        };
        let json = serde_json::to_value(&split_ratio).unwrap();
        assert_eq!(json["method"], "layout.set_split_ratio");
        let restored: Request = serde_json::from_value(json).unwrap();
        assert_eq!(restored, split_ratio);

        let subscription = Request {
            id: "sub_moves".into(),
            method: Method::EventsSubscribe(EventsSubscribeParams {
                subscriptions: vec![Subscription::WorkspaceMoved {}, Subscription::TabMoved {}],
            }),
        };
        let json = serde_json::to_string(&subscription).unwrap();
        assert!(json.contains("\"type\":\"workspace.moved\""));
        assert!(json.contains("\"type\":\"tab.moved\""));
        let restored: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, subscription);
    }
}
