// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use regex::Regex;

use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, PaneAgentStatusChangedEvent, PaneOutputMatchedEvent,
    PaneScrollChangedEvent, PaneScrollInfo, Request, Subscription, SubscriptionEventData,
    SubscriptionEventEnvelope, SubscriptionEventKind,
};
use crate::api::server::{dispatch_to_app_with_timeout, APP_RESPONSE_TIMEOUT};
use crate::api::{ApiRequestSender, EventHub};

pub(super) fn output_match_read_source(
    source: &crate::api::schema::ReadSource,
) -> crate::api::schema::ReadSource {
    match source {
        crate::api::schema::ReadSource::Recent => crate::api::schema::ReadSource::RecentUnwrapped,
        other => *other,
    }
}

pub(super) fn match_output(
    text: &str,
    matcher: &crate::api::schema::OutputMatch,
    regex: Option<&Regex>,
) -> Option<String> {
    match matcher {
        crate::api::schema::OutputMatch::Substring { value } => text
            .lines()
            .find(|line| line.contains(value))
            .map(|line| line.to_string()),
        crate::api::schema::OutputMatch::Regex { .. } => regex.and_then(|re| {
            text.lines()
                .find(|line| re.is_match(line))
                .map(|line| line.to_string())
        }),
    }
}

pub(super) struct ActiveOutputMatchedSubscription {
    pane_id: String,
    source: crate::api::schema::ReadSource,
    lines: Option<u32>,
    matcher: crate::api::schema::OutputMatch,
    regex: Option<Regex>,
    strip_ansi: bool,
    currently_matching: bool,
    request_prefix: String,
}

pub(super) struct ActiveAgentStatusChangedSubscription {
    pane_id: String,
    status_filter: Option<crate::api::schema::AgentStatus>,
    last_status: Option<crate::api::schema::AgentStatus>,
    last_presentation: Option<PanePresentationSnapshot>,
    last_sequence: u64,
    initial_event: Option<PaneAgentStatusChangedEvent>,
    request_prefix: String,
}

pub(super) struct ActiveScrollChangedSubscription {
    pane_id: String,
    last_scroll: Option<PaneScrollInfo>,
    request_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PanePresentationSnapshot {
    title: Option<String>,
    display_agent: Option<String>,

    state_labels: std::collections::HashMap<String, String>,
}

impl PanePresentationSnapshot {
    fn from(pane: &crate::api::schema::PaneInfo) -> Self {
        Self {
            title: pane.title.clone(),
            display_agent: pane.display_agent.clone(),

            state_labels: pane.state_labels.clone(),
        }
    }

    fn from_event(
        title: &Option<String>,
        display_agent: &Option<String>,
        state_labels: &std::collections::HashMap<String, String>,
    ) -> Self {
        Self {
            title: title.clone(),
            display_agent: display_agent.clone(),

            state_labels: state_labels.clone(),
        }
    }
}

pub(super) struct ActiveEventSubscription {
    event_kind: crate::api::schema::EventKind,
    last_sequence: u64,
}

pub(super) enum ActiveSubscription {
    Event(ActiveEventSubscription),
    OutputMatched(ActiveOutputMatchedSubscription),
    AgentStatusChanged(Box<ActiveAgentStatusChangedSubscription>),
    ScrollChanged(ActiveScrollChangedSubscription),
}

impl ActiveSubscription {
    pub(super) fn new(
        subscription: Subscription,
        request_id: &str,
        index: usize,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Result<Self, ErrorResponse> {
        match subscription {
            Subscription::WorkspaceCreated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceCreated,
                last_sequence: 0,
            })),
            Subscription::WorkspaceUpdated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceUpdated,
                last_sequence: 0,
            })),
            Subscription::WorkspaceMetadataUpdated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceMetadataUpdated,
                last_sequence: 0,
            })),
            Subscription::WorkspaceRenamed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceRenamed,
                last_sequence: 0,
            })),
            Subscription::WorkspaceMoved {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceMoved,
                last_sequence: 0,
            })),
            Subscription::WorkspaceClosed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceClosed,
                last_sequence: 0,
            })),
            Subscription::WorkspaceFocused {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceFocused,
                last_sequence: 0,
            })),
            Subscription::TabCreated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabCreated,
                last_sequence: 0,
            })),
            Subscription::TabClosed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabClosed,
                last_sequence: 0,
            })),
            Subscription::TabFocused {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabFocused,
                last_sequence: 0,
            })),
            Subscription::TabRenamed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabRenamed,
                last_sequence: 0,
            })),
            Subscription::TabMoved {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabMoved,
                last_sequence: 0,
            })),
            Subscription::PaneCreated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneCreated,
                last_sequence: 0,
            })),
            Subscription::PaneUpdated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneUpdated,
                last_sequence: 0,
            })),
            Subscription::PaneClosed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneClosed,
                last_sequence: 0,
            })),
            Subscription::PaneFocused {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneFocused,
                last_sequence: 0,
            })),
            Subscription::PaneMoved {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneMoved,
                last_sequence: 0,
            })),
            Subscription::PaneExited {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneExited,
                last_sequence: 0,
            })),
            Subscription::PaneAgentDetected {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneAgentDetected,
                last_sequence: 0,
            })),
            Subscription::LayoutUpdated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::LayoutUpdated,
                last_sequence: 0,
            })),
            Subscription::PaneOutputMatched {
                pane_id,
                source,
                lines,
                r#match,
                strip_ansi,
            } => {
                let regex = match &r#match {
                    crate::api::schema::OutputMatch::Regex { value } => match Regex::new(value) {
                        Ok(regex) => Some(regex),
                        Err(err) => {
                            return Err(ErrorResponse {
                                id: request_id.to_string(),
                                error: ErrorBody {
                                    code: "invalid_regex".into(),
                                    message: err.to_string(),
                                },
                            });
                        }
                    },
                    crate::api::schema::OutputMatch::Substring { .. } => None,
                };

                let probe = pane_read(
                    format!("{request_id}:sub:{index}:probe"),
                    &pane_id,
                    source,
                    lines,
                    strip_ansi,
                    api_tx,
                );
                probe?;

                Ok(Self::OutputMatched(ActiveOutputMatchedSubscription {
                    pane_id,
                    source,
                    lines,
                    matcher: r#match,
                    regex,
                    strip_ansi,
                    currently_matching: false,
                    request_prefix: format!("{request_id}:sub:{index}"),
                }))
            }
            Subscription::PaneScrollChanged { pane_id } => {
                let probe = pane_get(format!("{request_id}:sub:{index}:probe"), &pane_id, api_tx)?;
                Ok(Self::ScrollChanged(ActiveScrollChangedSubscription {
                    pane_id: probe.pane_id,
                    last_scroll: probe.scroll,
                    request_prefix: format!("{request_id}:sub:{index}"),
                }))
            }
            Subscription::PaneAgentStatusChanged {
                pane_id,
                agent_status,
            } => {
                let last_sequence = event_hub.current_sequence();
                let probe = pane_get(format!("{request_id}:sub:{index}:probe"), &pane_id, api_tx)?;
                let last_status = probe.agent_status;
                let last_presentation = PanePresentationSnapshot::from(&probe);
                let initial_event = agent_status
                    .is_some_and(|wanted| wanted == probe.agent_status)
                    .then_some(PaneAgentStatusChangedEvent {
                        pane_id: probe.pane_id.clone(),
                        workspace_id: probe.workspace_id,
                        agent_status: probe.agent_status,
                        agent: probe.agent,
                        title: probe.title,
                        display_agent: probe.display_agent,

                        state_labels: probe.state_labels,
                    });

                Ok(Self::AgentStatusChanged(Box::new(
                    ActiveAgentStatusChangedSubscription {
                        pane_id: probe.pane_id,
                        status_filter: agent_status,
                        last_status: Some(last_status),
                        last_presentation: Some(last_presentation),
                        last_sequence,
                        initial_event,
                        request_prefix: format!("{request_id}:sub:{index}"),
                    },
                )))
            }
        }
    }

    pub(super) fn poll(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Option<serde_json::Value> {
        match self {
            Self::Event(subscription) => subscription.poll(event_hub),
            Self::OutputMatched(subscription) => {
                serde_json::to_value(subscription.poll(api_tx)?).ok()
            }
            Self::AgentStatusChanged(subscription) => {
                serde_json::to_value(subscription.poll(api_tx, event_hub)?).ok()
            }
            Self::ScrollChanged(subscription) => {
                serde_json::to_value(subscription.poll(api_tx)?).ok()
            }
        }
    }

    pub(super) fn poll_for_wait(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Result<Option<serde_json::Value>, ErrorResponse> {
        match self {
            Self::AgentStatusChanged(subscription) => Ok(subscription
                .poll_result(api_tx, event_hub)?
                .and_then(|event| serde_json::to_value(event).ok())),
            _ => Ok(self.poll(api_tx, event_hub)),
        }
    }
}

impl ActiveEventSubscription {
    fn poll(&mut self, event_hub: &EventHub) -> Option<serde_json::Value> {
        for (sequence, event) in event_hub.events_after(self.last_sequence) {
            self.last_sequence = sequence;
            if event.event == self.event_kind {
                return serde_json::to_value(event).ok();
            }
        }
        None
    }
}

impl ActiveOutputMatchedSubscription {
    fn poll(&mut self, api_tx: &ApiRequestSender) -> Option<SubscriptionEventEnvelope> {
        let read = pane_read(
            format!("{}:read", self.request_prefix),
            &self.pane_id,
            output_match_read_source(&self.source),
            self.lines,
            self.strip_ansi,
            api_tx,
        )
        .ok()?;

        let matched_line = match_output(&read.text, &self.matcher, self.regex.as_ref());
        match matched_line {
            Some(matched_line) => {
                if self.currently_matching {
                    return None;
                }
                self.currently_matching = true;
                Some(SubscriptionEventEnvelope {
                    event: SubscriptionEventKind::PaneOutputMatched,
                    data: SubscriptionEventData::PaneOutputMatched(PaneOutputMatchedEvent {
                        pane_id: read.pane_id.clone(),
                        matched_line,
                        read,
                    }),
                })
            }
            None => {
                self.currently_matching = false;
                None
            }
        }
    }
}

impl ActiveScrollChangedSubscription {
    fn poll(&mut self, api_tx: &ApiRequestSender) -> Option<SubscriptionEventEnvelope> {
        let pane = pane_get(
            format!("{}:pane", self.request_prefix),
            &self.pane_id,
            api_tx,
        )
        .ok()?;
        self.event_from_snapshot(pane)
    }

    fn event_from_snapshot(
        &mut self,
        pane: crate::api::schema::PaneInfo,
    ) -> Option<SubscriptionEventEnvelope> {
        let scroll = pane.scroll;
        if scroll == self.last_scroll {
            return None;
        }
        self.last_scroll = scroll;
        let scroll = scroll?;
        Some(SubscriptionEventEnvelope {
            event: SubscriptionEventKind::ScrollChanged,
            data: SubscriptionEventData::ScrollChanged(PaneScrollChangedEvent {
                pane_id: pane.pane_id,
                workspace_id: pane.workspace_id,
                scroll,
            }),
        })
    }
}

impl ActiveAgentStatusChangedSubscription {
    fn poll(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Option<SubscriptionEventEnvelope> {
        self.poll_result(api_tx, event_hub).ok().flatten()
    }

    fn poll_result(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Result<Option<SubscriptionEventEnvelope>, ErrorResponse> {
        let mut saw_status_event = false;
        for (sequence, event) in event_hub.events_after(self.last_sequence) {
            self.last_sequence = sequence;
            let crate::api::schema::EventData::PaneAgentStatusChanged {
                pane_id,
                workspace_id,
                agent_status,
                agent,
                title,
                display_agent,

                state_labels,
            } = event.data
            else {
                continue;
            };
            if event.event != crate::api::schema::EventKind::PaneAgentStatusChanged {
                continue;
            }
            if pane_id != self.pane_id {
                continue;
            }
            saw_status_event = true;

            let current_presentation =
                PanePresentationSnapshot::from_event(&title, &display_agent, &state_labels);
            self.last_status = Some(agent_status);
            self.last_presentation = Some(current_presentation);
            if self
                .status_filter
                .is_some_and(|wanted| wanted != agent_status)
            {
                continue;
            }

            self.initial_event = None;
            return Ok(Some(SubscriptionEventEnvelope {
                event: SubscriptionEventKind::PaneAgentStatusChanged,
                data: SubscriptionEventData::PaneAgentStatusChanged(PaneAgentStatusChangedEvent {
                    pane_id,
                    workspace_id,
                    agent_status,
                    agent,
                    title,
                    display_agent,

                    state_labels,
                }),
            }));
        }

        if saw_status_event {
            self.initial_event = None;
        } else if event_hub.current_sequence() != self.last_sequence {
            return Ok(None);
        } else if let Some(event) = self.initial_event.take() {
            return Ok(Some(SubscriptionEventEnvelope {
                event: SubscriptionEventKind::PaneAgentStatusChanged,
                data: SubscriptionEventData::PaneAgentStatusChanged(event),
            }));
        }

        let before_snapshot_sequence = self.last_sequence;
        let pane = pane_get(
            format!("{}:pane", self.request_prefix),
            &self.pane_id,
            api_tx,
        );
        let after_snapshot_sequence = event_hub.current_sequence();
        if after_snapshot_sequence != before_snapshot_sequence {
            return Ok(None);
        }
        let pane = pane?;

        let event = self.event_from_snapshot(pane);
        if event.is_some() {
            self.last_sequence = after_snapshot_sequence;
        }
        Ok(event)
    }

    fn event_from_snapshot(
        &mut self,
        pane: crate::api::schema::PaneInfo,
    ) -> Option<SubscriptionEventEnvelope> {
        let current_status = pane.agent_status;
        let current_presentation = PanePresentationSnapshot::from(&pane);
        let previous_status = self.last_status.replace(current_status);
        let previous_presentation = self.last_presentation.replace(current_presentation.clone());
        let presentation_changed = previous_presentation
            .as_ref()
            .is_some_and(|previous| previous != &current_presentation);
        let status_changed = previous_status.is_some_and(|previous| previous != current_status);
        if !(status_changed || presentation_changed) {
            return None;
        }
        if self
            .status_filter
            .is_some_and(|wanted| wanted != current_status)
        {
            return None;
        }

        Some(SubscriptionEventEnvelope {
            event: SubscriptionEventKind::PaneAgentStatusChanged,
            data: SubscriptionEventData::PaneAgentStatusChanged(PaneAgentStatusChangedEvent {
                pane_id: pane.pane_id,
                workspace_id: pane.workspace_id,
                agent_status: current_status,
                agent: pane.agent,
                title: pane.title,
                display_agent: pane.display_agent,

                state_labels: pane.state_labels,
            }),
        })
    }
}

fn pane_read(
    request_id: String,
    pane_id: &str,
    source: crate::api::schema::ReadSource,
    lines: Option<u32>,
    strip_ansi: bool,
    api_tx: &ApiRequestSender,
) -> Result<crate::api::schema::PaneReadResult, ErrorResponse> {
    let response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::PaneRead(crate::api::schema::PaneReadParams {
                pane_id: pane_id.to_string(),
                source,
                lines,
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi,
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
        // ADR 0014: this re-dispatch only ever carries a READ method
        // (`pane.read` / `pane.get`), never a pane-bound one, so the
        // fail-closed default caller is correct and deliberate.
        crate::api::ApiCaller::default(),
    );
    let value: serde_json::Value = serde_json::from_str(&response).map_err(|_| ErrorResponse {
        id: request_id.clone(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane read response".into(),
        },
    })?;
    if value.get("error").is_some() {
        return serde_json::from_value(value).map_err(|_| ErrorResponse {
            id: request_id,
            error: ErrorBody {
                code: "internal_error".into(),
                message: "failed to decode pane read error".into(),
            },
        });
    }
    serde_json::from_value(value["result"]["read"].clone()).map_err(|_| ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane read result".into(),
        },
    })
}

fn pane_get(
    request_id: String,
    pane_id: &str,
    api_tx: &ApiRequestSender,
) -> Result<crate::api::schema::PaneInfo, ErrorResponse> {
    let response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::PaneGet(crate::api::schema::PaneTarget {
                pane_id: pane_id.to_string(),
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
        // ADR 0014: this re-dispatch only ever carries a READ method
        // (`pane.read` / `pane.get`), never a pane-bound one, so the
        // fail-closed default caller is correct and deliberate.
        crate::api::ApiCaller::default(),
    );
    let value: serde_json::Value = serde_json::from_str(&response).map_err(|_| ErrorResponse {
        id: request_id.clone(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane get response".into(),
        },
    })?;
    if value.get("error").is_some() {
        let response =
            serde_json::from_value::<ErrorResponse>(value).map_err(|_| ErrorResponse {
                id: request_id,
                error: ErrorBody {
                    code: "internal_error".into(),
                    message: "failed to decode pane get error".into(),
                },
            })?;
        return Err(response);
    }
    serde_json::from_value(value["result"]["pane"].clone()).map_err(|_| ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane get result".into(),
        },
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn m837_raced_not_found_is_dropped_then_rederived() {
        let closed = EventEnvelope {
            event: EventKind::PaneClosed,
            data: EventData::PaneClosed {
                pane_id: "w2:p4".into(),
                workspace_id: "w2".into(),
            },
        };
        let requests = m837_responses(
            vec![
                (m837_error_value("pane_not_found"), Some(closed)),
                (m837_error_value("pane_not_found"), None),
            ],
            |tx, hub| {
                let mut subscription = m837_agent_subscription();
                let raced = subscription.poll_for_wait(tx, hub);
                assert_eq!(
                    raced.map_err(|e| serde_json::to_value(e).unwrap()),
                    Ok(None)
                );
                assert_eq!(hub.current_sequence(), 1);
                let stable = subscription.poll_for_wait(tx, hub);
                assert_eq!(
                    stable.map_err(|e| serde_json::to_value(e).unwrap()),
                    Err(serde_json::json!({"id": "m837:pane", "error": {
                        "code": "pane_not_found", "message": "retained app error"
                    }}))
                );
            },
        );
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.id == "m837:pane"));
    }

    #[test]
    fn m837_wait_helper_scroll_fallthrough_suppresses_snapshot_errors() {
        let baseline = m821_metrics(12, 240, 30);
        let requests = m837_responses(
            vec![
                (m837_error_value("pane_not_found"), None),
                (m821_response(Some(baseline)).unwrap(), None),
            ],
            |tx, hub| {
                let mut subscription =
                    ActiveSubscription::ScrollChanged(ActiveScrollChangedSubscription {
                        pane_id: "w2:p4".into(),
                        last_scroll: Some(baseline),
                        request_prefix: "scroll".into(),
                    });
                for _ in 0..2 {
                    let result = subscription.poll_for_wait(tx, hub);
                    assert_eq!(
                        result.map_err(|e| serde_json::to_value(e).unwrap()),
                        Ok(None)
                    );
                }
            },
        );
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn m837_wait_helper_replays_fork_presentation_without_probe() {
        let hub = EventHub::default();
        let mut event = status_event(Some("queued title"));
        if let EventData::PaneAgentStatusChanged {
            pane_id,
            agent_status,
            display_agent,
            state_labels,
            ..
        } = &mut event.data
        {
            *pane_id = "w2:p4".into();
            *agent_status = AgentStatus::Idle;
            *display_agent = Some("Reviewer".into());
            state_labels.insert("idle".into(), "Ready".into());
        } else {
            panic!("wrong fixture event");
        }
        let stimulus = serde_json::to_value(&event).unwrap();
        hub.push(event);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut subscription = m837_agent_subscription();
        let result = subscription.poll_for_wait(&tx, &hub);
        assert_eq!(
            result.map_err(|e| serde_json::to_value(e).unwrap()),
            Ok(Some(
                serde_json::json!({"event": "pane.agent_status_changed", "data": {
                    "pane_id": "w2:p4", "workspace_id": "workspace_1", "agent_status": "idle",
                    "agent": "pi", "title": "queued title", "display_agent": "Reviewer",
                    "state_labels": {"idle": "Ready"}
                }})
            ))
        );
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        let recorded: Vec<_> = hub.events_after(0).into_iter().map(|(_, e)| e).collect();
        assert_eq!(
            serde_json::to_value(recorded).unwrap(),
            serde_json::json!([stimulus])
        );
    }

    fn m837_error_value(code: &str) -> serde_json::Value {
        serde_json::json!({"error": {"code": code, "message": "retained app error"}})
    }

    fn m837_agent_subscription() -> ActiveSubscription {
        ActiveSubscription::AgentStatusChanged(Box::new(ActiveAgentStatusChangedSubscription {
            pane_id: "w2:p4".into(),
            status_filter: Some(AgentStatus::Idle),
            last_status: Some(AgentStatus::Unknown),
            last_presentation: Some(PanePresentationSnapshot {
                title: None,
                display_agent: None,
                state_labels: HashMap::new(),
            }),
            last_sequence: 0,
            initial_event: None,
            request_prefix: "m837".into(),
        }))
    }

    fn m837_responses(
        responses: Vec<(serde_json::Value, Option<EventEnvelope>)>,
        run: impl FnOnce(&ApiRequestSender, &EventHub),
    ) -> Vec<Request> {
        let expected_events: Vec<_> = responses.iter().filter_map(|(_, e)| e.clone()).collect();
        let hub = EventHub::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::api::ApiRequestMessage>();
        std::thread::scope(|scope| {
            let publisher = hub.clone();
            let responder = scope.spawn(move || {
                let mut requests = Vec::new();
                for (mut response, event) in responses {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                    let message = loop {
                        match rx.try_recv() {
                            Ok(message) => break message,
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                                if std::time::Instant::now() < deadline =>
                            {
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                            Err(error) => panic!("bounded m837 responder: {error:?}"),
                        }
                    };
                    assert!(matches!(&message.request.method, Method::PaneGet(_)));
                    if response.get("id").is_none() {
                        response["id"] = message.request.id.clone().into();
                    }
                    if let Some(event) = event {
                        publisher.push(event);
                    }
                    message.respond_to.send(response.to_string()).unwrap();
                    requests.push(message.request);
                }
                requests
            });
            run(&tx, &hub);
            drop(tx);
            let requests = responder.join().unwrap();
            let actual_events: Vec<_> = hub.events_after(0).into_iter().map(|(_, e)| e).collect();
            assert_eq!(
                serde_json::to_value(actual_events).unwrap(),
                serde_json::to_value(expected_events).unwrap()
            );
            requests
        })
    }

    #[test]
    fn m837_pane_get_preserves_typed_remote_error_envelope() {
        for code in ["pane_not_found", "access_denied"] {
            let mut response = m837_error_value(code);
            response["id"] = "remote-id".into();
            let expected = response.clone();
            let requests = m837_responses(vec![(response, None)], |tx, _| {
                let result = pane_get("local-probe".into(), "missing", tx);
                let observed = result.map_err(|error| serde_json::to_value(error).unwrap());
                assert_eq!(observed.map(|_| ()), Err(expected));
            });
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].id, "local-probe");
        }
    }

    #[test]
    fn m837_pane_get_malformed_error_still_refuses() {
        let requests = m837_responses(
            vec![(serde_json::json!({"error": {"code": 42}}), None)],
            |tx, _| {
                let result = pane_get("malformed-probe".into(), "missing", tx);
                assert_eq!(
                    result
                        .map(|_| ())
                        .map_err(|error| serde_json::to_value(error).unwrap()),
                    Err(serde_json::json!({"id": "malformed-probe", "error": {
                        "code": "internal_error", "message": "failed to decode pane get error"
                    }}))
                );
            },
        );
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn m837_agent_setup_preserves_refusal_and_probe_id() {
        let requests = m837_responses(
            vec![(m837_error_value("pane_not_found"), None)],
            |tx, hub| {
                let result = ActiveSubscription::new(
                    Subscription::PaneAgentStatusChanged {
                        pane_id: "missing".into(),
                        agent_status: Some(AgentStatus::Idle),
                    },
                    "setup",
                    0,
                    tx,
                    hub,
                );
                assert_eq!(
                    result
                        .map(|_| ())
                        .map_err(|error| serde_json::to_value(error).unwrap()),
                    Err(serde_json::json!({"id": "setup:sub:0:probe", "error": {
                        "code": "pane_not_found", "message": "retained app error"
                    }}))
                );
            },
        );
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn m837_ordinary_agent_poll_suppresses_errors_and_keeps_presentation() {
        let mut pane = m821_pane(None);
        pane.agent_status = AgentStatus::Idle;
        pane.title = Some("new title".into());
        pane.display_agent = Some("Reviewer".into());
        pane.state_labels.insert("idle".into(), "Ready".into());
        let expected = serde_json::json!({"event": "pane.agent_status_changed", "data": {
            "pane_id": "w2:p4", "workspace_id": "w2", "agent_status": "idle",
            "title": "new title", "display_agent": "Reviewer",
            "state_labels": {"idle": "Ready"}
        }});
        let requests = m837_responses(
            vec![
                (m837_error_value("pane_not_found"), None),
                (
                    serde_json::json!({"result": {"type": "pane_info", "pane": pane}}),
                    None,
                ),
            ],
            |tx, hub| {
                let mut subscription = m837_agent_subscription();
                assert_eq!(subscription.poll(tx, hub), None);
                assert_eq!(subscription.poll(tx, hub), Some(expected));
            },
        );
        assert_eq!(requests.len(), 2);
    }

    use std::collections::HashMap;

    use super::*;
    use crate::api::schema::{AgentStatus, EventData, EventEnvelope, EventKind};

    #[test]
    fn m828b_pane_updated_uses_dedicated_event_stream() {
        let hub = EventHub::default();
        let (api_tx, mut api_rx) = tokio::sync::mpsc::unbounded_channel();
        let decoded =
            serde_json::from_value::<Subscription>(serde_json::json!({"type": "pane.updated"}));
        assert!(
            decoded.is_ok(),
            "pane subscription JSON refused: {decoded:?}"
        );
        let mut subscription =
            ActiveSubscription::new(decoded.unwrap(), "pane-token", 0, &api_tx, &hub).unwrap();
        assert!(matches!(&subscription, ActiveSubscription::Event(event)
            if event.event_kind.dot_name() == "pane.updated"));
        let pane = serde_json::json!({
            "pane_id": "w7:p2", "terminal_id": "term_metadata", "workspace_id": "w7",
            "tab_id": "w7:t1", "focused": false, "agent_status": "unknown", "revision": 1,
            "tokens": {"build": "ready"}
        });
        let decoy = serde_json::json!({"event": "pane_created", "data": {
            "type": "pane_created", "pane": pane
        }});
        hub.push(serde_json::from_value(decoy).unwrap());
        let event = serde_json::json!({"event": "pane_updated", "data": {
            "type": "pane_updated", "pane": pane
        }});
        for _ in 0..2 {
            hub.push(serde_json::from_value(event.clone()).unwrap());
        }
        assert_eq!(subscription.poll(&api_tx, &hub), Some(event.clone()));
        assert_eq!(subscription.poll(&api_tx, &hub), Some(event));
        assert_eq!(subscription.poll(&api_tx, &hub), None);
        assert!(matches!(
            api_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn workspace_metadata_subscription_uses_dedicated_event_kind() {
        let hub = EventHub::default();
        let (api_tx, mut api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut subscription = ActiveSubscription::new(
            Subscription::WorkspaceMetadataUpdated {},
            "workspace-token",
            0,
            &api_tx,
            &hub,
        )
        .unwrap();
        assert!(matches!(&subscription, ActiveSubscription::Event(event)
            if event.event_kind == EventKind::WorkspaceMetadataUpdated));
        let workspace_json = serde_json::json!({
            "workspace_id": "w2", "number": 2, "label": "background", "focused": false,
            "pane_count": 1, "tab_count": 1, "active_tab_id": "w2:t1",
            "agent_status": "unknown", "tokens": {"build": "ready"}
        });
        let workspace: crate::api::schema::WorkspaceInfo =
            serde_json::from_value(workspace_json.clone()).unwrap();
        hub.push(EventEnvelope {
            event: EventKind::WorkspaceUpdated,
            data: EventData::WorkspaceUpdated {
                workspace: workspace.clone(),
            },
        });
        assert_eq!(subscription.poll(&api_tx, &hub), None);
        hub.push(EventEnvelope {
            event: EventKind::WorkspaceMetadataUpdated,
            data: EventData::WorkspaceMetadataUpdated { workspace },
        });
        assert_eq!(
            subscription.poll(&api_tx, &hub),
            Some(serde_json::json!({
                "event": "workspace_metadata_updated", "data": {
                    "type": "workspace_metadata_updated", "workspace": workspace_json
                }
            }))
        );
        assert_eq!(subscription.poll(&api_tx, &hub), None);
        assert!(matches!(
            api_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    fn m821_pane(scroll: Option<PaneScrollInfo>) -> crate::api::schema::PaneInfo {
        serde_json::from_value(serde_json::json!({
            "pane_id": "w2:p4", "terminal_id": "term_4", "workspace_id": "w2",
            "tab_id": "w2:t1", "focused": false, "agent_status": "unknown", "revision": 7,
            "scroll": scroll
        }))
        .unwrap()
    }

    fn m821_metrics(offset: u64, maximum: u64, rows: u64) -> PaneScrollInfo {
        PaneScrollInfo {
            offset_from_bottom: offset,
            max_offset_from_bottom: maximum,
            viewport_rows: rows,
        }
    }

    fn m821_event(metrics: PaneScrollInfo) -> serde_json::Value {
        serde_json::json!({"event": "pane.scroll_changed", "data": {
            "pane_id": "w2:p4", "workspace_id": "w2", "scroll": metrics
        }})
    }

    #[test]
    fn m821_scroll_changes_each_metric_and_deduplicates() {
        let baseline = m821_metrics(12, 240, 30);
        for next in [
            m821_metrics(13, 240, 30),
            m821_metrics(12, 241, 30),
            m821_metrics(12, 240, 31),
        ] {
            let mut subscription = ActiveScrollChangedSubscription {
                pane_id: "unused-selector".into(),
                last_scroll: Some(baseline),
                request_prefix: "scroll".into(),
            };
            assert_eq!(
                subscription.event_from_snapshot(m821_pane(Some(baseline))),
                None
            );
            let event = subscription.event_from_snapshot(m821_pane(Some(next)));
            assert_eq!(
                event.map(|event| serde_json::to_value(event).unwrap()),
                Some(m821_event(next))
            );
            assert_eq!(
                subscription.event_from_snapshot(m821_pane(Some(next))),
                None
            );
        }
    }

    #[test]
    fn m821_scroll_unavailability_clears_baseline_without_emitting() {
        let baseline = m821_metrics(12, 240, 30);
        let mut subscription = ActiveScrollChangedSubscription {
            pane_id: "w2:p4".into(),
            last_scroll: Some(baseline),
            request_prefix: "scroll".into(),
        };
        assert_eq!(subscription.event_from_snapshot(m821_pane(None)), None);
        assert_eq!(subscription.event_from_snapshot(m821_pane(None)), None);
        assert_eq!(
            subscription
                .event_from_snapshot(m821_pane(Some(baseline)))
                .map(|event| serde_json::to_value(event).unwrap()),
            Some(m821_event(baseline))
        );
        assert_eq!(
            subscription.event_from_snapshot(m821_pane(Some(baseline))),
            None
        );
    }

    fn m821_responses(
        responses: Vec<Option<serde_json::Value>>,
        run: impl FnOnce(&ApiRequestSender, &EventHub),
    ) -> Vec<Request> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::api::ApiRequestMessage>();
        std::thread::scope(|scope| {
            let responder = scope.spawn(move || {
                let mut requests = Vec::new();
                for response in responses {
                    let start = std::time::Instant::now();
                    let message = loop {
                        match rx.try_recv() {
                            Ok(message) => break message,
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                                if start.elapsed() < std::time::Duration::from_secs(2) =>
                            {
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                            Err(error) => panic!("bounded scroll responder: {error:?}"),
                        }
                    };
                    assert!(matches!(message.request.method, Method::PaneGet(_)));
                    if let Some(mut response) = response {
                        response["id"] = message.request.id.clone().into();
                        message.respond_to.send(response.to_string()).unwrap();
                    }
                    requests.push(message.request);
                }
                requests
            });
            let hub = EventHub::default();
            run(&tx, &hub);
            assert!(hub.events_after(0).is_empty());
            drop(tx);
            responder.join().unwrap()
        })
    }

    fn m821_response(scroll: Option<PaneScrollInfo>) -> Option<serde_json::Value> {
        Some(serde_json::json!({"result": {"type": "pane_info", "pane": m821_pane(scroll)}}))
    }

    #[test]
    fn m821_scroll_setup_seeds_baseline_and_polls_canonical_target() {
        let baseline = m821_metrics(12, 240, 30);
        let changed = m821_metrics(13, 240, 30);
        let requests = m821_responses(
            vec![
                m821_response(Some(baseline)),
                m821_response(Some(baseline)),
                m821_response(Some(changed)),
            ],
            |tx, hub| {
                let mut subscription = ActiveSubscription::new(
                    Subscription::PaneScrollChanged {
                        pane_id: "legacy-selector".into(),
                    },
                    "scroll",
                    2,
                    tx,
                    hub,
                )
                .unwrap();
                assert_eq!(
                    subscription.poll(tx, hub),
                    None,
                    "setup baseline is not an initial event"
                );
                assert_eq!(subscription.poll(tx, hub), Some(m821_event(changed)));
            },
        );
        let wire: Vec<_> = requests
            .into_iter()
            .map(|r| serde_json::to_value(r).unwrap())
            .collect();
        assert_eq!(
            wire,
            vec![
                serde_json::json!({"id": "scroll:sub:2:probe", "method": "pane.get", "params": {"pane_id": "legacy-selector"}}),
                serde_json::json!({"id": "scroll:sub:2:pane", "method": "pane.get", "params": {"pane_id": "w2:p4"}}),
                serde_json::json!({"id": "scroll:sub:2:pane", "method": "pane.get", "params": {"pane_id": "w2:p4"}}),
            ]
        );
    }

    #[test]
    fn m821_scroll_failed_poll_retains_successful_baseline() {
        let baseline = m821_metrics(12, 240, 30);
        let requests = m821_responses(
            vec![
                m821_response(Some(baseline)),
                None,
                m821_response(Some(baseline)),
            ],
            |tx, hub| {
                let mut subscription = ActiveSubscription::new(
                    Subscription::PaneScrollChanged {
                        pane_id: "w2:p4".into(),
                    },
                    "scroll",
                    0,
                    tx,
                    hub,
                )
                .unwrap();
                assert_eq!(
                    subscription.poll(tx, hub),
                    None,
                    "failed App request emits nothing"
                );
                assert_eq!(
                    subscription.poll(tx, hub),
                    None,
                    "failed read must not clear baseline"
                );
            },
        );
        assert_eq!(requests.len(), 3);
    }

    #[test]
    fn m821_scroll_none_setup_baseline_emits_only_when_available() {
        let baseline = m821_metrics(0, 240, 30);
        let requests = m821_responses(
            vec![
                m821_response(None),
                m821_response(None),
                m821_response(Some(baseline)),
            ],
            |tx, hub| {
                let mut subscription = ActiveSubscription::new(
                    Subscription::PaneScrollChanged {
                        pane_id: "w2:p4".into(),
                    },
                    "scroll",
                    0,
                    tx,
                    hub,
                )
                .unwrap();
                assert_eq!(subscription.poll(tx, hub), None);
                assert_eq!(subscription.poll(tx, hub), Some(m821_event(baseline)));
            },
        );
        assert_eq!(requests.len(), 3);
    }

    #[test]
    fn m821_scroll_setup_error_preserves_existing_probe_error_contract() {
        // M8-37 preserves the real App error, refusal, probe ID, one request and no hub writes.
        let requests = m821_responses(
            vec![Some(serde_json::json!({"error": {
                "code": "pane_not_found", "message": "missing scroll target"
            }}))],
            |tx, hub| {
                let result = ActiveSubscription::new(
                    Subscription::PaneScrollChanged {
                        pane_id: "missing".into(),
                    },
                    "scroll",
                    0,
                    tx,
                    hub,
                );
                let error = match result {
                    Err(error) => error,
                    Ok(_) => panic!("setup error acknowledged"),
                };
                assert_eq!(
                    serde_json::to_value(error).unwrap(),
                    serde_json::json!({
                        "id": "scroll:sub:0:probe", "error": {
                            "code": "pane_not_found", "message": "missing scroll target"
                        }
                    })
                );
            },
        );
        assert_eq!(requests.len(), 1);
    }

    fn status_event(title: Option<&str>) -> EventEnvelope {
        EventEnvelope {
            event: EventKind::PaneAgentStatusChanged,
            data: EventData::PaneAgentStatusChanged {
                pane_id: "pane_1".into(),
                workspace_id: "workspace_1".into(),
                agent_status: AgentStatus::Working,
                agent: Some("pi".into()),
                title: title.map(str::to_string),
                display_agent: None,
                state_labels: HashMap::new(),
            },
        }
    }

    #[test]
    fn m813_layout_subscription_filters_and_replays_without_app_requests() {
        let hub = EventHub::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let make_event = |tab_id: &str| EventEnvelope {
            event: EventKind::LayoutUpdated,
            data: EventData::LayoutUpdated {
                layout: crate::api::schema::PaneLayoutSnapshot {
                    workspace_id: "w3".into(),
                    tab_id: tab_id.into(),
                    zoomed: false,
                    area: crate::api::schema::PaneLayoutRect {
                        x: 2,
                        y: 3,
                        width: 80,
                        height: 20,
                    },
                    focused_pane_id: "w3:p2".into(),
                    panes: vec![],
                    splits: vec![],
                },
            },
        };
        let first = make_event("w3:t2");
        hub.push(status_event(Some("unrelated")));
        hub.push(first.clone());
        let mut subscription =
            ActiveSubscription::new(Subscription::LayoutUpdated {}, "layout", 0, &tx, &hub)
                .unwrap();
        assert!(
            rx.try_recv().is_err(),
            "layout setup must not enqueue an App request"
        );
        assert_eq!(
            subscription.poll(&tx, &hub),
            Some(serde_json::to_value(first).unwrap())
        );
        assert!(subscription.poll(&tx, &hub).is_none());
        let later = make_event("w3:t5");
        hub.push(status_event(Some("still unrelated")));
        hub.push(later.clone());
        assert_eq!(
            subscription.poll(&tx, &hub),
            Some(serde_json::to_value(later).unwrap())
        );
        assert!(subscription.poll(&tx, &hub).is_none());
        assert!(
            rx.try_recv().is_err(),
            "layout polling must not enqueue an App request"
        );
    }

    #[test]
    fn agent_status_subscription_replays_queued_metadata_set_and_expiry_events() {
        let event_hub = EventHub::default();
        let mut subscription = ActiveAgentStatusChangedSubscription {
            pane_id: "pane_1".into(),
            status_filter: None,
            last_status: Some(AgentStatus::Working),
            last_presentation: Some(PanePresentationSnapshot {
                title: None,
                display_agent: None,

                state_labels: HashMap::new(),
            }),
            last_sequence: event_hub.current_sequence(),
            initial_event: None,
            request_prefix: "test".into(),
        };

        event_hub.push(status_event(Some("short lived")));
        event_hub.push(status_event(None));

        let set_event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("set event");
        let SubscriptionEventData::PaneAgentStatusChanged(set_data) = set_event.data else {
            panic!("wrong event data");
        };
        assert_eq!(set_data.title.as_deref(), Some("short lived"));

        let expiry_event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("expiry event");
        let SubscriptionEventData::PaneAgentStatusChanged(expiry_data) = expiry_event.data else {
            panic!("wrong event data");
        };
        assert_eq!(expiry_data.title, None);
    }

    #[test]
    fn agent_status_subscription_prefers_setup_window_events_over_initial_snapshot() {
        let event_hub = EventHub::default();
        let mut subscription = ActiveAgentStatusChangedSubscription {
            pane_id: "pane_1".into(),
            status_filter: Some(AgentStatus::Working),
            last_status: Some(AgentStatus::Working),
            last_presentation: Some(PanePresentationSnapshot {
                title: None,
                display_agent: None,

                state_labels: HashMap::new(),
            }),
            last_sequence: event_hub.current_sequence(),
            initial_event: Some(PaneAgentStatusChangedEvent {
                pane_id: "pane_1".into(),
                workspace_id: "workspace_1".into(),
                agent_status: AgentStatus::Working,
                agent: Some("pi".into()),
                title: None,
                display_agent: None,

                state_labels: HashMap::new(),
            }),
            request_prefix: "test".into(),
        };

        event_hub.push(status_event(Some("short lived")));
        event_hub.push(status_event(None));

        let set_event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("set event");
        let SubscriptionEventData::PaneAgentStatusChanged(set_data) = set_event.data else {
            panic!("wrong event data");
        };
        assert_eq!(set_data.title.as_deref(), Some("short lived"));

        let expiry_event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("expiry event");
        let SubscriptionEventData::PaneAgentStatusChanged(expiry_data) = expiry_event.data else {
            panic!("wrong event data");
        };
        assert_eq!(expiry_data.title, None);
    }

    #[test]
    fn agent_status_subscription_emits_setup_window_event_already_reflected_by_probe() {
        let event_hub = EventHub::default();
        let mut subscription = ActiveAgentStatusChangedSubscription {
            pane_id: "pane_1".into(),
            status_filter: Some(AgentStatus::Working),
            last_status: Some(AgentStatus::Working),
            last_presentation: Some(PanePresentationSnapshot {
                title: Some("short lived".into()),
                display_agent: None,
                state_labels: HashMap::new(),
            }),
            last_sequence: event_hub.current_sequence(),
            initial_event: Some(PaneAgentStatusChangedEvent {
                pane_id: "pane_1".into(),
                workspace_id: "workspace_1".into(),
                agent_status: AgentStatus::Working,
                agent: Some("pi".into()),
                title: Some("short lived".into()),
                display_agent: None,
                state_labels: HashMap::new(),
            }),
            request_prefix: "test".into(),
        };

        event_hub.push(status_event(Some("short lived")));

        let event = subscription
            .poll(&tokio::sync::mpsc::unbounded_channel().0, &event_hub)
            .expect("setup-window event");
        let SubscriptionEventData::PaneAgentStatusChanged(data) = event.data else {
            panic!("wrong event data");
        };
        assert_eq!(data.title.as_deref(), Some("short lived"));
        assert!(subscription.initial_event.is_none());
    }
}
