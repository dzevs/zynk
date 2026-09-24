// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
#[test]
fn m832c_planner_and_presentation_cadence_preserve_hidden_pty_policy() {
    for full in [false, true] {
        for graphics in [false, true] {
            for pty in [
                PtyRenderState::Clean,
                PtyRenderState::Hidden,
                PtyRenderState::Visible,
            ] {
                let input = RetainedRenderInput {
                    needs_full_render: full,
                    pty,
                };
                let expected = if full || (graphics && pty == PtyRenderState::Visible) {
                    RetainedRenderPlan::Full
                } else if graphics {
                    RetainedRenderPlan::Graphics
                } else {
                    retained_render_plan(input)
                };
                assert_eq!(
                    retained_render_plan_with_graphics(input, graphics),
                    expected,
                    "full={full} graphics={graphics} pty={pty:?}"
                );
            }
        }
    }
    let (mut server, hidden) = hidden_pty_visibility_test_server(&[]);
    assert!(server.app.render_dirty.request_pty(hidden));
    let t0 = Instant::now();
    server.app.record_render_attempt(t0, true);
    server
        .app
        .record_render_attempt(t0 + Duration::from_secs(1), false);
    let now = t0 + Duration::from_secs(1) + Duration::from_millis(1);
    assert!(!server.app.can_render_now(now));
    assert!(server.app.can_present_now(now));
    assert!(!server.has_pending_presentation_work_with_graphics(false, false));
    assert!(server.has_pending_presentation_work_with_graphics(false, true));
    assert!(server.app.render_dirty.take().pty_sources.contains(&hidden));
}

#[test]
fn m832c_deferred_transition_matrix_keeps_full_priority() {
    for initial in [
        DeferredRender::None,
        DeferredRender::Graphics,
        DeferredRender::Full,
    ] {
        for next in [
            DeferredRender::None,
            DeferredRender::Graphics,
            DeferredRender::Full,
        ] {
            let mut client = ClientConnection::new(
                (80, 24),
                Default::default(),
                Default::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            );
            match initial {
                DeferredRender::None => (),
                DeferredRender::Graphics => client.defer_pane_graphics_render(),
                DeferredRender::Full => client.defer_full_render(),
            }
            match next {
                DeferredRender::None => (),
                DeferredRender::Graphics => client.defer_pane_graphics_render(),
                DeferredRender::Full => client.defer_full_render(),
            }
            let expected = if initial == DeferredRender::Full || next == DeferredRender::Full {
                DeferredRender::Full
            } else if initial == DeferredRender::Graphics || next == DeferredRender::Graphics {
                DeferredRender::Graphics
            } else {
                DeferredRender::None
            };
            assert_eq!(client.deferred_render(), expected);
            assert_eq!(client.render_pending, expected == DeferredRender::Full);
            assert_eq!(client.take_deferred_render(), expected);
            assert_eq!(client.deferred_render(), DeferredRender::None);
            client.defer_pane_graphics_render();
            client.clear_deferred_render();
            assert_eq!(client.deferred_render(), DeferredRender::None);
        }
    }
}

#[tokio::test]
async fn m832c_full_lane_retry_commits_latest_graphics_not_speculative_cache() {
    let (mut server, rx, pane) = retained_test_server(b"aaaa");
    let baseline = enable_graphics_and_render(&mut server, &rx);
    let before = server.clients[&1].graphics_cache.clone();
    set_graphics_layer(&mut server, pane, vec![1, 2, 3]);
    fill_render_lane(&server);
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Deferred
    );
    assert_eq!(server.clients[&1].graphics_cache, before);
    assert_eq!(
        server.clients[&1].deferred_render(),
        DeferredRender::Graphics
    );
    assert_frame_data_eq(
        server.clients[&1].render_state.last_frame().unwrap(),
        &baseline,
    );
    set_graphics_layer(&mut server, pane, vec![7, 8, 9]);
    let mut expected_cache = before.clone();
    let expected = frame_pane_graphics_for_client(
        crate::kitty_graphics::encode_local_pane_graphics(
            &server.app.state,
            &server.app.pane_graphics,
            &server.app.terminal_runtimes,
            server.app.state.view.tab_surface(),
            server.clients[&1].cell_size,
            Some(crate::kitty_graphics::HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
            false,
            &mut expected_cache,
        )
        .bytes,
    );
    assert!(!expected.is_empty());
    assert!(matches!(
        read_server_message(rx.try_recv().unwrap()),
        ServerMessage::ReloadSoundConfig
    ));
    assert_eq!(
        server.handle_server_event_with_render_impact(ServerEvent::ClientWriterDrained {
            client_id: 1
        }),
        RenderImpact::Graphics
    );
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    match read_server_message(rx.try_recv().unwrap()) {
        ServerMessage::Graphics { bytes } => assert_eq!(bytes, expected),
        other => panic!("expected Graphics, got {other:?}"),
    }
    assert_eq!(server.clients[&1].graphics_cache, expected_cache);
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);
    assert_frame_data_eq(
        server.clients[&1].render_state.last_frame().unwrap(),
        &baseline,
    );
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    assert!(
        rx.try_recv().is_err(),
        "unchanged cache emits no duplicate graphics"
    );
    assert_eq!(server.clients[&1].graphics_cache, expected_cache);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn m832c_retained_guards_and_non_app_modes_preserve_cache_and_baseline() {
    for guard in [
        "no-baseline",
        "stale-size",
        "reset",
        "unknown-cell",
        "full-redraw",
    ] {
        let (mut server, rx, pane) = retained_test_server(b"aaaa");
        if guard != "no-baseline" {
            let _ = enable_graphics_and_render(&mut server, &rx);
        }
        server.app.full_redraw_pending = false;
        server.app.state.kitty_graphics_enabled = true;
        server.clients.get_mut(&1).unwrap().cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        set_graphics_layer(&mut server, pane, vec![1, 2, 3]);
        match guard {
            "stale-size" => server.clients.get_mut(&1).unwrap().terminal_size.0 += 1,
            "reset" => {
                server
                    .clients
                    .get_mut(&1)
                    .unwrap()
                    .graphics_surface_reset_pending = true
            }
            "unknown-cell" => server.clients.get_mut(&1).unwrap().cell_size = Default::default(),
            "full-redraw" => server.app.full_redraw_pending = true,
            _ => (),
        }
        let cache = server.clients[&1].graphics_cache.clone();
        assert_eq!(
            server.render_retained_graphics_update_and_stream(),
            RetainedGraphicsOutcome::Fallback,
            "guard={guard}"
        );
        assert_eq!(server.clients[&1].graphics_cache, cache, "guard={guard}");
        assert!(rx.try_recv().is_err(), "guard={guard}");
        shutdown_test_runtimes(&mut server);
    }
    for mode in [
        ClientConnectionMode::TerminalPending,
        ClientConnectionMode::TerminalAttach {
            terminal_id: "direct".into(),
        },
        ClientConnectionMode::TerminalObserve {
            terminal_id: "observe".into(),
        },
    ] {
        let (mut server, rx, pane) = retained_test_server(b"aaaa");
        let baseline = enable_graphics_and_render(&mut server, &rx);
        let cache = server.clients[&1].graphics_cache.clone();
        server.clients.get_mut(&1).unwrap().mode = mode.clone();
        set_graphics_layer(&mut server, pane, vec![4, 5, 6]);
        assert_eq!(
            server.render_retained_graphics_update_and_stream(),
            RetainedGraphicsOutcome::Sent
        );
        assert!(rx.try_recv().is_err(), "mode={mode:?}");
        assert_eq!(server.clients[&1].graphics_cache, cache);
        assert_frame_data_eq(
            server.clients[&1].render_state.last_frame().unwrap(),
            &baseline,
        );
        shutdown_test_runtimes(&mut server);
    }
}

#[tokio::test]
async fn m832c_disconnected_writer_is_removed_and_writerless_target_is_untouched() {
    let (mut server, rx, pane) = retained_test_server(b"aaaa");
    let _ = enable_graphics_and_render(&mut server, &rx);
    set_graphics_layer(&mut server, pane, vec![1, 2, 3]);
    drop(rx);
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    assert!(!server.clients.contains_key(&1));
    shutdown_test_runtimes(&mut server);
    let (mut server, rx, pane) = retained_test_server(b"aaaa");
    let baseline = enable_graphics_and_render(&mut server, &rx);
    let cache = server.clients[&1].graphics_cache.clone();
    server.clients.get_mut(&1).unwrap().writer = None;
    set_graphics_layer(&mut server, pane, vec![7, 8, 9]);
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    assert_eq!(server.clients[&1].graphics_cache, cache);
    assert_frame_data_eq(
        server.clients[&1].render_state.last_frame().unwrap(),
        &baseline,
    );
    shutdown_test_runtimes(&mut server);
}

#[test]
fn m832c_stop_fences_precede_dequeue_and_stream_mutation() {
    let (mut server, api_tx) = test_headless_server_with_api_sender();
    let mut replies = Vec::new();
    for n in 0..3 {
        let (msg, rx) = stream_set_message(&format!("q{n}"), "1:p1", "owner", vec![1, 2, 3]);
        api_tx.send(msg).unwrap();
        replies.push(rx);
        server
            .server_event_tx
            .try_send(ServerEvent::ClientWriterDrained { client_id: n })
            .unwrap();
    }
    server.should_quit.store(true, Ordering::Release);
    assert_eq!(
        server.drain_api_requests_with_render_impact(),
        RenderImpact::None
    );
    assert_eq!(
        server.drain_server_events_with_render_impact(),
        RenderImpact::None
    );
    assert_eq!(server.app.api_rx.len(), 3);
    assert_eq!(server.server_event_rx.len(), 3);
    for atomic in [false, true] {
        let mut server = test_headless_server();
        server.app.state.kitty_graphics_enabled = true;
        server.shutting_down = !atomic;
        server.should_quit.store(atomic, Ordering::Release);
        let before = server.app.pane_graphics.revision();
        let (msg, rx) = stream_set_message("stop", "1:p1", "owner", vec![4, 5, 6]);
        assert_eq!(
            server.handle_api_request_with_render_impact(msg),
            RenderImpact::None
        );
        let response: api::schema::ErrorResponse =
            serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(response.error.code, "server_unavailable", "atomic={atomic}");
        assert_eq!(server.app.pane_graphics.revision(), before);
        assert!(server.app.pane_graphics.slots.is_empty());
    }
    assert_eq!(replies.len(), 3);
}

#[test]
fn m832c_idle_owner_cleanup_promotes_full_impact_without_queued_work() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("cleanup");
    let pane = workspace.tabs[0].root_pane;
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    server.app.state.kitty_graphics_enabled = true;
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let workspace = &server.app.state.workspaces[0];
    let target = crate::workspace::public_pane_id_for_number(
        &workspace.id,
        workspace.public_pane_number(pane).unwrap(),
    );
    let response = server.app.handle_api_request(api::schema::Request {
        id: "open".into(),
        method: api::schema::Method::PaneGraphicsStreamOpen(
            api::schema::PaneGraphicsStreamOpenParams {
                params: api::schema::PaneGraphicsStreamParams {
                    pane_id: target,
                    layer_id: None,
                    z_index: 0,
                    owner: "cleanup".into(),
                },
                active: active.clone(),
            },
        ),
    });
    assert!(serde_json::from_str::<api::schema::SuccessResponse>(&response).is_ok());
    set_graphics_layer(&mut server, pane, vec![1, 2, 3]);
    let revision = server.app.pane_graphics.revision();
    active.store(false, Ordering::Release);
    assert_eq!(
        server.drain_api_requests_with_render_impact(),
        RenderImpact::Full
    );
    assert!(server.app.pane_graphics.slots.is_empty());
    assert_eq!(
        server.app.pane_graphics.revision(),
        revision.wrapping_add(1)
    );
    assert_eq!(
        server.drain_api_requests_with_render_impact(),
        RenderImpact::None
    );
}

#[test]
fn m832c_due_metadata_expiry_dominates_rejected_graphics_frame() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("expiry");
    let pane = workspace.tabs[0].root_pane;
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    server.app.state.kitty_graphics_enabled = true;
    let terminal = server.app.state.terminal_id_for_pane(0, pane).unwrap();
    let _ = server
        .app
        .state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .set_agent_metadata(crate::terminal::AgentMetadataReport {
            source: "user:graphics-expiry".into(),
            agent_label: None,
            applies_to_source: None,
            title: Some("expires".into()),
            display_agent: None,
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_state_labels: false,
            ttl: Some(Duration::ZERO),
            seq: None,
        });
    server.app.sync_agent_metadata_deadline();
    assert!(server
        .app
        .agent_metadata_deadline
        .is_some_and(|d| d <= Instant::now()));
    let workspace = &server.app.state.workspaces[0];
    let target = crate::workspace::public_pane_id_for_number(
        &workspace.id,
        workspace.public_pane_number(pane).unwrap(),
    );
    let (msg, rx) = stream_set_message("expired-frame", &target, "unclaimed", vec![1, 2, 3]);
    assert_eq!(
        server.handle_api_request_with_render_impact(msg),
        RenderImpact::Full
    );
    let response: api::schema::ErrorResponse =
        serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!(response.error.code, "stream_closed");
    assert_eq!(server.app.agent_metadata_deadline, None);
    assert!(!server.app.state.terminals[&terminal]
        .agent_metadata
        .contains_key("user:graphics-expiry"));
    assert!(server.app.pane_graphics.slots.is_empty());
}

#[tokio::test]
async fn m832c_oversized_encoded_frame_does_not_commit_cache() {
    let (mut server, rx, pane) = retained_test_server(b"aaaa");
    let baseline = enable_graphics_and_render(&mut server, &rx);
    set_graphics_layer(
        &mut server,
        pane,
        vec![1; api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES],
    );
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    match protocol::read_message(
        &mut std::io::Cursor::new(rx.try_recv().expect("maximum raw stream frame encoded")),
        crate::protocol::MAX_GRAPHICS_FRAME_SIZE,
    )
    .expect("decode maximum graphics frame")
    {
        ServerMessage::Graphics { bytes } => {
            assert!(bytes.len() < crate::protocol::MAX_GRAPHICS_FRAME_SIZE)
        }
        other => panic!("expected maximum Graphics frame, got {other:?}"),
    }
    let cache = server.clients[&1].graphics_cache.clone();
    set_graphics_layer(
        &mut server,
        pane,
        vec![1; crate::protocol::MAX_GRAPHICS_FRAME_SIZE],
    );
    let _ = server.render_retained_graphics_update_and_stream();
    assert!(rx.try_recv().is_err());
    assert_eq!(server.clients[&1].graphics_cache, cache);
    assert_frame_data_eq(
        server.clients[&1].render_state.last_frame().unwrap(),
        &baseline,
    );
    shutdown_test_runtimes(&mut server);
}

use super::*;

#[test]
fn frames_preserve_cursor_without_changing_ordinary_messages() {
    assert_eq!(frame_pane_graphics_for_client(Vec::new()), Vec::<u8>::new());
    assert_eq!(
        frame_pane_graphics_for_client(b"graphics".to_vec()),
        b"\x1b7graphics\x1b8"
    );
}

fn enable_graphics_and_render(
    server: &mut HeadlessServer,
    client_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> FrameData {
    server.app.state.kitty_graphics_enabled = true;
    server.clients.get_mut(&1).unwrap().cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    server.render_and_stream();
    read_server_frame(
        client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame"),
    )
}

fn set_graphics_layer(server: &mut HeadlessServer, pane_id: crate::layout::PaneId, data: Vec<u8>) {
    let key = (
        pane_id,
        api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID.to_string(),
    );
    let image_id = server
        .app
        .pane_graphics
        .reserve_image_id(&key)
        .expect("test layer image id");
    let layer = crate::app::pane_graphics::Layer::inline(
        api::schema::PaneGraphicsFormat::Png,
        1,
        1,
        data,
        api::schema::PaneGraphicsPlacementParams::default(),
        0,
    );
    if let Some(slot) = server.app.pane_graphics.slots.get_mut(&key) {
        slot.layer = Some(layer);
    } else {
        server.app.pane_graphics.slots.insert(
            key,
            crate::app::pane_graphics::Slot::test(image_id, Some(layer)),
        );
    }
    server.app.pane_graphics.mark_changed();
}

fn fill_render_lane(server: &HeadlessServer) {
    let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
        .expect("dummy frame");
    server
        .clients
        .get(&1)
        .unwrap()
        .writer
        .as_ref()
        .unwrap()
        .render
        .try_send(queued)
        .expect("pre-fill render lane");
}

fn stream_set_message(
    id: &str,
    pane_id: &str,
    owner: &str,
    data: Vec<u8>,
) -> (api::ApiRequestMessage, std::sync::mpsc::Receiver<String>) {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    (
        api::ApiRequestMessage {
            request: api::schema::Request {
                id: id.into(),
                method: api::schema::Method::PaneGraphicsStreamSet(
                    api::schema::PaneGraphicsSetParams {
                        pane_id: pane_id.into(),
                        layer_id: None,
                        z_index: 0,
                        owner: owner.into(),
                        format: api::schema::PaneGraphicsFormat::Png,
                        image_width: 1,
                        image_height: 1,
                        data: Some(data),
                        data_base64: String::new(),
                        placement: api::schema::PaneGraphicsPlacementParams::default(),
                    },
                ),
            },
            respond_to,
            response_write_complete: None,
            caller: api::ApiCaller::default(),
        },
        response_rx,
    )
}

#[tokio::test]
async fn retained_update_sends_only_graphics_message() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
    let baseline = enable_graphics_and_render(&mut server, &client_rx);
    set_graphics_layer(&mut server, pane_id, vec![1, 2, 3]);

    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    match read_server_message(
        client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("graphics-only update"),
    ) {
        ServerMessage::Graphics { bytes } => {
            assert!(bytes.windows(3).any(|window| window == b"\x1b_G"));
        }
        other => panic!("expected graphics-only message, got {other:?}"),
    }
    assert_frame_data_eq(
        server
            .clients
            .get(&1)
            .unwrap()
            .render_state
            .last_frame()
            .expect("semantic baseline"),
        &baseline,
    );
}

#[tokio::test]
async fn retained_update_defers_on_full_render_lane() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
    let _ = enable_graphics_and_render(&mut server, &client_rx);
    fill_render_lane(&server);
    set_graphics_layer(&mut server, pane_id, vec![4, 5, 6]);

    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Deferred
    );
    let client = server.clients.get(&1).unwrap();
    assert_eq!(client.deferred_render(), DeferredRender::Graphics);
    assert!(matches!(
        read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
        ServerMessage::ReloadSoundConfig
    ));
    assert_eq!(
        server.handle_server_event_with_render_impact(ServerEvent::ClientWriterDrained {
            client_id: 1
        }),
        RenderImpact::Graphics
    );
}

#[tokio::test]
async fn retained_update_does_not_downgrade_pending_full_render() {
    let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
    let _ = enable_graphics_and_render(&mut server, &client_rx);
    fill_render_lane(&server);
    let client = server.clients.get_mut(&1).unwrap();
    client.request_repaint();
    server.render_and_stream();
    assert_eq!(
        server.clients.get(&1).unwrap().deferred_render(),
        DeferredRender::Full
    );

    set_graphics_layer(&mut server, pane_id, vec![7, 8, 9]);
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Deferred
    );
    assert_eq!(
        server.clients.get(&1).unwrap().deferred_render(),
        DeferredRender::Full
    );
    assert_eq!(
        server.handle_server_event_with_render_impact(ServerEvent::ClientWriterDrained {
            client_id: 1
        }),
        RenderImpact::Full
    );
}

#[tokio::test]
async fn retained_update_falls_back_for_mixed_app_geometry() {
    let (mut server, client_rx, _pane_id) = retained_test_server(b"aaaa");
    let _ = enable_graphics_and_render(&mut server, &client_rx);

    let (writer, _control_rx, _render_rx) = test_client_writer();
    server.clients.insert(
        2,
        ClientConnection::new(
            (60, 20),
            crate::kitty_graphics::HostCellSize {
                width_px: 10,
                height_px: 20,
            },
            crate::terminal_theme::TerminalTheme::default(),
            None,
            2,
            RenderEncoding::SemanticFrame,
            Some(writer),
        ),
    );

    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Fallback
    );
}

#[test]
fn stream_set_has_graphics_only_render_impact() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("graphics");
    let pane_id = workspace.tabs[0].root_pane;
    let public_pane_id = format!("{}:p1", workspace.id);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.kitty_graphics_enabled = true;
    server.app.state.ensure_test_terminals();
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let open = server.app.handle_api_request(api::schema::Request {
        id: "claim".into(),
        method: api::schema::Method::PaneGraphicsStreamOpen(
            api::schema::PaneGraphicsStreamOpenParams {
                params: api::schema::PaneGraphicsStreamParams {
                    pane_id: public_pane_id.clone(),
                    layer_id: None,
                    z_index: 0,
                    owner: "owner-a".into(),
                },
                active: active.clone(),
            },
        ),
    });
    assert!(serde_json::from_str::<api::schema::SuccessResponse>(&open).is_ok());
    let key = (
        pane_id,
        api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID.to_string(),
    );
    assert_eq!(
        server.app.pane_graphics.slots[&key].stream_owner.as_deref(),
        Some("owner-a")
    );

    let (request, response_rx) =
        stream_set_message("wrong-owner", &public_pane_id, "owner-b", vec![1, 2, 3]);
    assert_eq!(
        server.handle_api_request_with_render_impact(request),
        RenderImpact::None
    );
    assert!(serde_json::from_str::<api::schema::ErrorResponse>(
        &response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
    )
    .is_ok());

    let (request, response_rx) =
        stream_set_message("stream-frame", &public_pane_id, "owner-a", vec![1, 2, 3]);
    assert_eq!(
        server.handle_api_request_with_render_impact(request),
        RenderImpact::Graphics
    );
    assert!(serde_json::from_str::<api::schema::SuccessResponse>(
        &response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
    )
    .is_ok());

    server
        .app
        .event_tx
        .try_send(AppEvent::UpdateReady {
            version: "9.9.9".into(),
            install_command: "zynk update".into(),
        })
        .unwrap();
    let (request, _response_rx) = stream_set_message(
        "stream-frame-with-internal-event",
        &public_pane_id,
        "owner-a",
        vec![4, 5, 6],
    );
    assert_eq!(
        server.handle_api_request_with_render_impact(request),
        RenderImpact::Full
    );

    server.app.pane_graphics.clear();
    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    let impact = server.handle_api_request_with_render_impact(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "direct-frame".into(),
            method: api::schema::Method::PaneGraphicsSet(api::schema::PaneGraphicsSetParams {
                pane_id: public_pane_id,
                layer_id: None,
                z_index: 0,
                owner: String::new(),
                format: api::schema::PaneGraphicsFormat::Png,
                image_width: 1,
                image_height: 1,
                data: Some(vec![1, 2, 3]),
                data_base64: String::new(),
                placement: api::schema::PaneGraphicsPlacementParams::default(),
            }),
        },
        respond_to,
        response_write_complete: None,
        caller: api::ApiCaller::default(),
    });
    assert_eq!(impact, RenderImpact::Full);
}

#[test]
fn rejected_or_stale_requests_do_not_schedule_rendering() {
    let mut server = test_headless_server();
    let workspace = crate::workspace::Workspace::test_new("graphics");
    let pane_id = workspace.tabs[0].root_pane;
    let public_pane_id = format!("{}:p1", workspace.id);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;

    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "disabled-set".into(),
            method: api::schema::Method::PaneGraphicsSet(api::schema::PaneGraphicsSetParams {
                pane_id: public_pane_id.clone(),
                layer_id: None,
                z_index: 0,
                owner: String::new(),
                format: api::schema::PaneGraphicsFormat::Png,
                image_width: 1,
                image_height: 1,
                data: Some(vec![1, 2, 3]),
                data_base64: String::new(),
                placement: api::schema::PaneGraphicsPlacementParams::default(),
            }),
        },
        respond_to,
        response_write_complete: None,
        caller: api::ApiCaller::default(),
    });
    assert!(!changed);
    let response = response_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<api::schema::ErrorResponse>(&response)
            .unwrap()
            .error
            .code,
        "feature_disabled"
    );

    server.app.state.kitty_graphics_enabled = true;
    let key = (
        pane_id,
        api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID.to_string(),
    );
    let mut slot = crate::app::pane_graphics::Slot::test(1 << 31, None);
    slot.stream_owner = Some("current-owner".into());
    slot.stream_active = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
        true,
    )));
    server.app.pane_graphics.slots.insert(key.clone(), slot);
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    let impact = server.handle_api_request_with_render_impact(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "stale-close".into(),
            method: api::schema::Method::PaneGraphicsStreamClose(
                api::schema::PaneGraphicsStreamParams {
                    pane_id: public_pane_id,
                    layer_id: None,
                    z_index: 0,
                    owner: "stale-owner".into(),
                },
            ),
        },
        respond_to,
        response_write_complete: None,
        caller: api::ApiCaller::default(),
    });
    assert_eq!(impact, RenderImpact::None);
    assert_eq!(
        server.app.pane_graphics.slots[&key].stream_owner.as_deref(),
        Some("current-owner")
    );
    assert!(serde_json::from_str::<api::schema::SuccessResponse>(
        &response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
    )
    .is_ok());
}

#[tokio::test]
async fn m833_popup_graphics_deletion_waits_for_writer_acceptance() {
    let (mut server, rx, pane) = retained_test_server(b"tile");
    set_graphics_layer(&mut server, pane, vec![1, 2, 3]);
    let baseline = enable_graphics_and_render(&mut server, &rx);
    let before = server.clients[&1].graphics_cache.clone();
    assert!(!before.is_empty());
    let (runtime, _input) = crate::terminal::TerminalRuntime::test_with_channel(40, 12);
    server.app.install_test_popup_runtime(runtime);
    assert!(!crate::kitty_graphics::has_visible_pane_graphics(
        &server.app.state,
        &server.app.pane_graphics,
        &server.app.terminal_runtimes,
        server.app.state.view.tab_surface(),
        server.clients[&1].cell_size,
    ));
    fill_render_lane(&server);
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Deferred
    );
    assert_eq!(server.clients[&1].graphics_cache, before);
    assert_frame_data_eq(
        server.clients[&1].render_state.last_frame().unwrap(),
        &baseline,
    );
    assert_eq!(
        server.clients[&1].deferred_render(),
        DeferredRender::Graphics
    );
    assert!(matches!(
        read_server_message(rx.try_recv().unwrap()),
        ServerMessage::ReloadSoundConfig
    ));
    assert_eq!(
        server.handle_server_event_with_render_impact(ServerEvent::ClientWriterDrained {
            client_id: 1
        }),
        RenderImpact::Graphics
    );
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    match read_server_message(rx.try_recv().unwrap()) {
        ServerMessage::Graphics { bytes } => {
            let bytes = std::str::from_utf8(&bytes).unwrap();
            assert!(bytes.contains("a=d,d=i,"));
            assert!(!bytes.contains("a=t,"));
        }
        other => panic!("expected Graphics deletion, got {other:?}"),
    }
    assert_eq!(server.clients[&1].graphics_cache.test_image_count(), 1);
    assert_eq!(server.clients[&1].graphics_cache.test_placement_count(), 0);
    assert_frame_data_eq(
        server.clients[&1].render_state.last_frame().unwrap(),
        &baseline,
    );
    assert!(server.app.close_popup_pane());
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    match read_server_message(rx.try_recv().unwrap()) {
        ServerMessage::Graphics { bytes } => {
            let bytes = std::str::from_utf8(&bytes).unwrap();
            assert!(bytes.contains("a=p,"));
            assert!(!bytes.contains("a=t,"));
        }
        other => panic!("expected Graphics reveal, got {other:?}"),
    }
    assert!(!server.clients[&1].graphics_cache.is_empty());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn m833_popup_hidden_stream_replaces_data_without_losing_claim() {
    let (mut server, rx, pane) = retained_test_server(b"tile");
    let _ = enable_graphics_and_render(&mut server, &rx);
    server.app.state.ensure_test_terminals();
    let workspace = &server.app.state.workspaces[0];
    let target = crate::workspace::public_pane_id_for_number(
        &workspace.id,
        workspace.public_pane_number(pane).unwrap(),
    );
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let opened = server.app.handle_api_request(api::schema::Request {
        id: "popup-stream".into(),
        method: api::schema::Method::PaneGraphicsStreamOpen(
            api::schema::PaneGraphicsStreamOpenParams {
                params: api::schema::PaneGraphicsStreamParams {
                    pane_id: target.clone(),
                    layer_id: None,
                    z_index: 0,
                    owner: "popup-owner".into(),
                },
                active: active.clone(),
            },
        ),
    });
    assert!(serde_json::from_str::<api::schema::SuccessResponse>(&opened).is_ok());
    let (runtime, _input) = crate::terminal::TerminalRuntime::test_with_channel(40, 12);
    server.app.install_test_popup_runtime(runtime);
    for data in [vec![1, 2, 3], vec![7, 8, 9]] {
        let (message, _response) =
            stream_set_message("frame", &target, "popup-owner", data.clone());
        let reply = server.app.handle_api_request(message.request);
        assert!(serde_json::from_str::<api::schema::SuccessResponse>(&reply).is_ok());
        assert_eq!(server.app.pane_graphics.slots.len(), 1);
        let key = (
            pane,
            api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID.to_string(),
        );
        assert_eq!(
            server.app.pane_graphics.slots[&key]
                .layer
                .as_ref()
                .and_then(crate::app::pane_graphics::Layer::inline_data),
            Some(data.as_slice())
        );
        assert_eq!(
            server.app.pane_graphics.slots[&key].stream_owner.as_deref(),
            Some("popup-owner")
        );
        assert!(active.load(Ordering::Acquire));
        assert!(!server.app.sync_pane_graphics_streams());
        assert_eq!(
            server.render_retained_graphics_update_and_stream(),
            RetainedGraphicsOutcome::Sent
        );
        assert!(rx.try_recv().is_err());
        assert!(server.clients[&1].graphics_cache.is_empty());
    }
    assert!(server.app.close_popup_pane());
    assert_eq!(
        server.render_retained_graphics_update_and_stream(),
        RetainedGraphicsOutcome::Sent
    );
    let graphics = match read_server_message(rx.try_recv().unwrap()) {
        ServerMessage::Graphics { bytes } => String::from_utf8(bytes).unwrap(),
        other => panic!("expected latest graphics, got {other:?}"),
    };
    assert!(graphics.contains("BwgJ"));
    assert!(!graphics.contains("AQID"));
    let key = (
        pane,
        api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID.to_string(),
    );
    assert_eq!(
        server.app.pane_graphics.slots[&key].stream_owner.as_deref(),
        Some("popup-owner")
    );
    assert!(active.load(Ordering::Acquire));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn m833_popup_full_frame_pressure_preserves_baseline_until_retry() {
    let (mut server, rx, pane) = retained_test_server(b"tile");
    set_graphics_layer(&mut server, pane, vec![1, 2, 3]);
    let baseline = enable_graphics_and_render(&mut server, &rx);
    let before = server.clients[&1].graphics_cache.clone();
    let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(40, 12, b"POPUP");
    server.app.install_test_popup_runtime(runtime);
    server.app.full_redraw_pending = false;
    assert!(!server.retained_pty_update_allowed_by_app_state());
    assert!(!server.render_retained_pty_update_and_stream());
    fill_render_lane(&server);
    server.render_and_stream();
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::Full);
    assert_eq!(server.clients[&1].graphics_cache, before);
    assert_frame_data_eq(
        server.clients[&1].render_state.last_frame().unwrap(),
        &baseline,
    );
    assert!(matches!(
        read_server_message(rx.try_recv().unwrap()),
        ServerMessage::ReloadSoundConfig
    ));
    assert_eq!(
        server.handle_server_event_with_render_impact(ServerEvent::ClientWriterDrained {
            client_id: 1
        }),
        RenderImpact::Full
    );
    server.render_and_stream();
    let shown = read_server_frame(rx.try_recv().unwrap());
    assert!(frame_text(&shown).contains("POPUP"));
    assert!(std::str::from_utf8(&shown.graphics)
        .unwrap()
        .contains("a=d,d=i,"));
    assert_eq!(server.clients[&1].graphics_cache.test_image_count(), 1);
    assert_eq!(server.clients[&1].graphics_cache.test_placement_count(), 0);
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);
    assert_frame_data_eq(
        server.clients[&1].render_state.last_frame().unwrap(),
        &shown,
    );
    server.app.close_popup_pane();
    shutdown_test_runtimes(&mut server);
}
