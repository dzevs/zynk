// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::time::Instant;

#[cfg(test)]
use std::time::Duration;

use crossterm::terminal;

use super::{
    background_update_check_enabled, App, AUTO_UPDATE_CHECK_INTERVAL, MIN_RENDER_INTERVAL,
    RESIZE_POLL_INTERVAL, SELECTION_AUTOSCROLL_INTERVAL,
};
fn retain_detached_process_after_wait(
    pid: u32,
    result: std::io::Result<Option<std::process::ExitStatus>>,
) -> bool {
    match result {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => true,
        Err(err) => {
            tracing::warn!(pid, err = %err, "failed to reap detached process");
            false
        }
    }
}

impl App {
    pub(crate) fn reap_finished_detached_processes(&mut self) {
        self.detached_process_children
            .retain_mut(|child| retain_detached_process_after_wait(child.id(), child.try_wait()));
    }

    pub(crate) fn shutdown_detached_terminal_runtimes(&mut self) {
        let terminal_ids = std::mem::take(&mut self.state.terminal_runtime_shutdowns);
        for terminal_id in terminal_ids {
            self.shutdown_terminal_runtime(terminal_id);
        }
    }

    pub(crate) fn shutdown_terminal_runtime(&mut self, terminal_id: crate::terminal::TerminalId) {
        let target = super::TerminalInputTarget {
            terminal_id: terminal_id.clone(),
        };
        self.release_input_target_headless(&target);
        if let Some(runtime) = self.terminal_runtimes.remove(&terminal_id) {
            runtime.shutdown();
        }
    }

    pub(crate) fn drain_api_requests(&mut self) -> bool {
        let mut changed = self.sync_pane_graphics_streams();
        while let Ok(msg) = self.api_rx.try_recv() {
            changed |= self.handle_api_request_message(msg);
            self.shutdown_detached_terminal_runtimes();
        }
        changed
    }

    pub(super) fn handle_api_request_message(
        &mut self,
        msg: crate::api::ApiRequestMessage,
    ) -> bool {
        let mut changed = self.sync_pane_graphics_streams();
        changed |= self.expire_due_metadata(Instant::now());
        changed |= crate::api::request_changes_ui(&msg.request);
        let skip_default_workspace = matches!(
            &msg.request.method,
            crate::api::schema::Method::ServerStop(_)
                | crate::api::schema::Method::ServerLiveHandoff(_)
        );
        // Worktree create/remove are dispatched asynchronously: the background git
        // command + workspace mutation respond later via msg.respond_to, so they
        // bypass the synchronous response path. (upstream 46a2b25)
        if matches!(
            &msg.request.method,
            crate::api::schema::Method::WorktreeCreate(_)
                | crate::api::schema::Method::WorktreeRemove(_)
        ) {
            self.drain_all_internal_events();
            let deferred_changed =
                self.handle_deferred_worktree_api_request(msg.request, msg.respond_to);
            changed |= self.sync_pane_graphics_streams();
            if !skip_default_workspace {
                changed |= self.ensure_default_workspace();
            }
            return changed | deferred_changed;
        }
        let response = self.handle_api_request_from_socket(msg.request, msg.caller);
        changed |= self.sync_pane_graphics_streams();
        if !skip_default_workspace {
            changed |= self.ensure_default_workspace();
        }
        let _ = msg.respond_to.send(response);
        changed
    }

    pub(super) async fn handle_raw_input_batch(
        &mut self,
        first: crate::raw_input::RawInputEvent,
    ) -> bool {
        let mut changed = self.handle_raw_input_event(first).await;

        while let Some(rx) = self.input_rx.as_mut() {
            match rx.try_recv() {
                Ok(event) => changed |= self.handle_raw_input_event(event).await,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.input_rx = None;
                    break;
                }
            }
        }

        changed
    }

    async fn execute_repeat_plan(
        &mut self,
        lease_key: super::input::InputLeaseKey,
        key: crate::input::TerminalKey,
        plan: super::input::RepeatPlan,
    ) -> bool {
        match plan {
            super::input::RepeatPlan::Forwarded(target) => {
                if !self.forward_terminal_key_to_target(&target, key).await {
                    self.discard_failed_repeat_lease(&lease_key, &target);
                }
                true
            }
            super::input::RepeatPlan::Reprocess {
                context,
                repetitions,
                tracked,
            } => {
                let key = key
                    .with_kind(crossterm::event::KeyEventKind::Repeat)
                    .with_repeat_count(1);
                let mut forwarded_target = None;
                for _ in 0..repetitions {
                    if let Some(target) = &forwarded_target {
                        if !self
                            .forward_terminal_key_to_target(target, key.clone())
                            .await
                        {
                            self.discard_failed_repeat_lease(&lease_key, target);
                            break;
                        }
                        continue;
                    }
                    let current_context = self.terminal_input_context();
                    if !self.input_leases.reprocess_allowed(
                        lease_key,
                        &context,
                        current_context.as_ref(),
                        tracked,
                    ) {
                        break;
                    }
                    if let Some(target) = self.handle_key(key.clone()).await {
                        if tracked {
                            self.input_leases.insert_forwarded(
                                lease_key,
                                target.clone(),
                                key.clone(),
                            );
                            forwarded_target = Some(target);
                        }
                    }
                }
                true
            }
            super::input::RepeatPlan::Ignore => false,
        }
    }

    pub(super) async fn handle_raw_input_event(
        &mut self,
        event: crate::raw_input::RawInputEvent,
    ) -> bool {
        let changed = match event {
            crate::raw_input::RawInputEvent::Key(key) => {
                let lease_key = super::input::InputLeaseKey::new(super::LOCAL_INPUT_SOURCE, &key);
                let key = self.input_leases.normalize_press(&lease_key, key);
                match key.kind {
                    crossterm::event::KeyEventKind::Press => {
                        let initial_context = self.terminal_input_context();
                        let target = self.handle_key(key.clone()).await;
                        let resulting_context = self.terminal_input_context();
                        let plan = self.input_leases.complete_press(
                            lease_key,
                            &key,
                            initial_context.as_ref(),
                            resulting_context.as_ref(),
                            target,
                        );
                        self.execute_repeat_plan(lease_key, key, plan).await;
                        true
                    }
                    crossterm::event::KeyEventKind::Repeat => {
                        let current_context = self.terminal_input_context();
                        let plan = self.input_leases.plan_repeat(
                            lease_key,
                            &key,
                            current_context.as_ref(),
                        );
                        self.execute_repeat_plan(lease_key, key, plan).await
                    }
                    crossterm::event::KeyEventKind::Release => {
                        if let Some(lease) = self.input_leases.remove_forwarded(&lease_key) {
                            let _ = self
                                .forward_terminal_key_to_target(&lease.target, key)
                                .await;
                        }
                        false
                    }
                }
            }
            crate::raw_input::RawInputEvent::Text(text) => {
                self.handle_text_commit(text.into_string()).await;
                true
            }
            crate::raw_input::RawInputEvent::Paste(text) => {
                self.handle_paste(text).await;
                true
            }
            crate::raw_input::RawInputEvent::Mouse(mouse) => {
                let changes_view = !matches!(mouse.kind, crossterm::event::MouseEventKind::Moved)
                    || self.state.mode.mouse_motion_changes_view();
                if self.state.mouse_capture || self.state.popup_pane.is_some() {
                    self.handle_mouse(mouse);
                } else {
                    self.state.handle_pane_mouse_only(
                        &self.terminal_runtimes,
                        super::LOCAL_INPUT_SOURCE,
                        mouse,
                    );
                }
                changes_view
            }
            crate::raw_input::RawInputEvent::OuterFocusGained => {
                self.query_host_terminal_appearance();
                self.send_outer_focus_event(crate::ghostty::FocusEvent::Gained);
                if self.state.redraw_on_focus_gained {
                    self.request_repaint();
                }
                self.state.outer_terminal_focus = Some(true);
                self.state.mark_active_tab_seen();
                true
            }
            crate::raw_input::RawInputEvent::OuterFocusLost => {
                self.release_input_source(super::LOCAL_INPUT_SOURCE).await;
                self.send_outer_focus_event(crate::ghostty::FocusEvent::Lost);
                self.state.outer_terminal_focus = Some(false);
                false
            }
            crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                self.update_host_terminal_theme(kind, color)
            }
            crate::raw_input::RawInputEvent::HostPaletteColors { colors } => {
                self.update_host_terminal_palette_colors(&colors)
            }
            crate::raw_input::RawInputEvent::HostColorSchemeChanged(appearance) => {
                self.query_host_terminal_theme();
                self.set_host_terminal_appearance(appearance, true)
            }
            crate::raw_input::RawInputEvent::HostCellSizeReport { .. }
            | crate::raw_input::RawInputEvent::Unsupported => false,
        };
        self.shutdown_detached_terminal_runtimes();
        changed
    }

    fn handle_resize_poll(&mut self) -> bool {
        let Ok(size) = terminal::size() else {
            return false;
        };
        if self.last_terminal_size != Some(size) {
            self.last_terminal_size = Some(size);
            return true;
        }
        false
    }

    pub(crate) fn handle_scheduled_tasks(&mut self, now: Instant, geometry_dirty: bool) -> bool {
        let mut changed = false;
        let mut resized = false;

        if now >= self.next_resize_poll {
            resized = self.handle_resize_poll();
            changed |= resized;
            self.next_resize_poll = now + RESIZE_POLL_INTERVAL;
        }

        if self
            .config_diagnostic_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.config_diagnostic_deadline = None;
            self.state.config_diagnostic = None;
            changed = true;
        }

        if self.toast_deadline.is_some_and(|deadline| now >= deadline) {
            self.toast_deadline = None;
            self.state.toast = None;
            changed = true;
        }

        if self
            .state
            .next_pending_agent_notification_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            let previous_toast = self.state.toast.clone();
            let mut deliveries = self.state.drain_due_agent_notifications(now);
            if !deliveries.is_empty() {
                self.refresh_agent_notification_delivery_contexts(&mut deliveries);
                self.emit_delayed_client_local_agent_notifications(&deliveries);
                self.sync_toast_deadline(previous_toast);
                changed = true;
            }
        }

        if self
            .copy_feedback_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.copy_feedback_deadline = None;
            self.state.copy_feedback = None;
            changed = true;
        }

        if self
            .selection_autoscroll_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.tick_selection_autoscroll(now);
            changed = true;
        }

        changed |= self.clear_due_selection_highlight(now);

        self.start_git_status_refresh_if_due(now);

        if self
            .next_auto_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.run_auto_update_check();
        }

        if self
            .next_agent_manifest_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.run_agent_manifest_update_check();
        }

        if self
            .session_save_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.start_background_session_save();
        }

        changed |= self.expire_due_metadata(now);

        changed |= self.reconcile_due_managed_agents(now);

        if geometry_dirty || resized {
            self.pending_agent_resume_deadline = None;
        } else {
            self.sync_pending_agent_resume_deadline(now);
            changed |= self.start_pending_agent_resumes(self.pending_agent_resume_due(now));
        }
        changed
    }

    pub(crate) fn reconcile_due_managed_agents(&mut self, now: Instant) -> bool {
        if !self
            .state
            .next_managed_agent_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            return false;
        }
        let panes = self.state.reconcile_managed_agents_at(now);
        if panes.is_empty() {
            return false;
        }
        for (ws_idx, pane_id) in panes {
            self.emit_pane_updated(ws_idx, pane_id);
        }
        self.schedule_session_save();
        true
    }

    /// Clears temporary copied-token highlights, such as after double-click copy.
    pub(crate) fn clear_due_selection_highlight(&mut self, now: Instant) -> bool {
        if self
            .selection_highlight_clear_deadline
            .is_none_or(|deadline| now < deadline)
        {
            return false;
        }

        self.selection_highlight_clear_deadline = None;
        if self
            .state
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.is_in_progress())
        {
            self.state.clear_selection();
            return true;
        }
        false
    }

    pub(crate) fn sync_agent_metadata_deadline(&mut self) {
        self.agent_metadata_deadline = self.state.next_agent_metadata_expiry();
    }

    pub(crate) fn expire_due_metadata(&mut self, now: Instant) -> bool {
        let Some(deadline) = self
            .agent_metadata_deadline
            .filter(|deadline| now >= *deadline)
        else {
            return false;
        };
        self.expire_metadata_at(deadline, now);
        true
    }

    pub(crate) fn expire_metadata_at(&mut self, deadline: Instant, now: Instant) {
        let previous_toast = self.state.toast.clone();
        for update in self.state.expire_agent_metadata_at(deadline, now) {
            self.refresh_new_zynk_toast_context_for_update(&update, &previous_toast);
            self.emit_pane_state_update(&update);
        }
        let (panes, workspaces) = self.state.expire_metadata_tokens(now);
        for (ws_idx, pane_id) in panes {
            self.emit_pane_updated(ws_idx, pane_id);
        }
        for ws_idx in workspaces {
            self.emit_workspace_token_updated(ws_idx);
        }
        self.sync_agent_metadata_deadline();
    }

    pub(crate) fn tick_selection_autoscroll(&mut self, now: Instant) {
        let Some(autoscroll) = self.state.selection_autoscroll.clone() else {
            // Self-heal: state cleared but deadline leaked
            self.selection_autoscroll_deadline = None;
            return;
        };

        // Selection must still be in progress for autoscroll to continue
        let Some(pane_id) = self.state.selection.as_ref().map(|s| s.pane_id) else {
            self.stop_selection_autoscroll();
            return;
        };
        if !self
            .state
            .selection
            .as_ref()
            .is_some_and(|s| s.is_dragging())
        {
            self.stop_selection_autoscroll();
            return;
        }

        // Rect-change detection: if inner_rect changed since drag, stop
        let current_rect = self
            .state
            .pane_info_by_id(pane_id)
            .map(|info| info.inner_rect);
        if current_rect != Some(autoscroll.inner_rect) {
            self.stop_selection_autoscroll();
            return;
        }

        // Scrollback boundary detection via ScrollMetrics — fail-closed if unavailable
        let Some(metrics) = self
            .state
            .pane_scroll_metrics(&self.terminal_runtimes, pane_id)
        else {
            self.stop_selection_autoscroll();
            return;
        };
        match autoscroll.direction {
            crate::app::state::SelectionAutoscrollDirection::Up => {
                let at_top = metrics.offset_from_bottom >= metrics.max_offset_from_bottom;
                if at_top {
                    self.stop_selection_autoscroll();
                    return;
                }
                self.state
                    .scroll_pane_up(&self.terminal_runtimes, pane_id, 1);
            }
            crate::app::state::SelectionAutoscrollDirection::Down => {
                let at_bottom = metrics.offset_from_bottom == 0;
                if at_bottom {
                    self.stop_selection_autoscroll();
                    return;
                }
                self.state
                    .scroll_pane_down(&self.terminal_runtimes, pane_id, 1);
            }
        }

        // Extend selection cursor to last known mouse position
        self.state.update_selection_cursor(
            &self.terminal_runtimes,
            pane_id,
            autoscroll.last_mouse_screen_col,
            autoscroll.last_mouse_screen_row,
        );

        // Reschedule
        self.selection_autoscroll_deadline = Some(now + SELECTION_AUTOSCROLL_INTERVAL);
    }

    pub(crate) fn stop_selection_autoscroll(&mut self) {
        self.state.stop_selection_autoscroll_state();
        self.selection_autoscroll_deadline = None;
    }

    pub(crate) fn can_render_now(&self, now: Instant) -> bool {
        match self.last_render_at {
            Some(last_render_at) => now.duration_since(last_render_at) >= MIN_RENDER_INTERVAL,
            None => true,
        }
    }

    pub(crate) fn can_present_now(&self, now: Instant) -> bool {
        match self.last_presentation_at {
            Some(last_presentation_at) => {
                now.duration_since(last_presentation_at) >= MIN_RENDER_INTERVAL
            }
            None => true,
        }
    }

    pub(crate) fn record_render_attempt(&mut self, now: Instant, presentation: bool) {
        self.last_render_at = Some(now);
        if presentation {
            self.last_presentation_at = Some(now);
        }
    }

    pub(crate) fn run_auto_update_check(&mut self) {
        if !background_update_check_enabled(self.no_session, self.update_version_check_enabled) {
            self.next_auto_update_check = None;
            return;
        }

        self.next_auto_update_check = self
            .state
            .update_available
            .is_none()
            .then_some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL);

        if self.state.update_available.is_some() {
            return;
        }

        let update_tx = self.event_tx.clone();
        std::thread::spawn(move || crate::update::auto_update(update_tx));
    }

    pub(crate) fn run_agent_manifest_update_check(&mut self) {
        if !background_update_check_enabled(self.no_session, self.update_manifest_check_enabled) {
            self.next_agent_manifest_update_check = None;
            return;
        }

        self.next_agent_manifest_update_check = Some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL);

        let manifest_update_tx = self.event_tx.clone();
        std::thread::spawn(move || crate::detect::manifest_update::auto_update(manifest_update_tx));
    }

    pub(crate) fn next_loop_deadline(&self, now: Instant, needs_render: bool) -> Option<Instant> {
        self.next_loop_deadline_with_resize_poll(now, needs_render, true, true)
    }

    pub(crate) fn next_headless_loop_deadline_with_git_refresh(
        &self,
        now: Instant,
        needs_render: bool,
        include_git_refresh: bool,
    ) -> Option<Instant> {
        self.next_loop_deadline_with_resize_poll(now, needs_render, false, include_git_refresh)
    }

    fn next_loop_deadline_with_resize_poll(
        &self,
        now: Instant,
        needs_render: bool,
        include_resize_poll: bool,
        include_git_refresh: bool,
    ) -> Option<Instant> {
        let render_deadline = if needs_render {
            self.last_render_at
                .map(|last_render_at| last_render_at + MIN_RENDER_INTERVAL)
                .filter(|deadline| *deadline > now)
        } else {
            None
        };

        [
            include_resize_poll.then_some(self.next_resize_poll),
            self.config_diagnostic_deadline,
            self.toast_deadline,
            self.state.next_pending_agent_notification_deadline(),
            self.state.next_managed_agent_deadline(),
            self.copy_feedback_deadline,
            include_git_refresh
                .then(|| self.git_refresh_deadline())
                .flatten(),
            self.next_auto_update_check,
            self.next_agent_manifest_update_check,
            self.agent_metadata_deadline,
            self.pending_agent_resume_deadline,
            self.session_save_deadline,
            self.selection_autoscroll_deadline,
            self.selection_highlight_clear_deadline,
            render_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn drain_internal_events(&mut self) -> bool {
        self.drain_internal_events_up_to(super::APP_EVENT_DRAIN_LIMIT)
            .1
    }

    pub(crate) fn drain_all_internal_events(&mut self) -> bool {
        let mut changed = false;
        loop {
            let (had_event, batch_changed) =
                self.drain_internal_events_up_to(super::APP_EVENT_DRAIN_LIMIT);
            changed |= batch_changed;
            if !had_event {
                break;
            }
        }
        changed
    }

    fn drain_internal_events_up_to(&mut self, limit: usize) -> (bool, bool) {
        let mut had_event = false;
        let mut changed = false;
        for _ in 0..limit {
            let Ok(ev) = self.event_rx.try_recv() else {
                break;
            };
            had_event = true;
            changed |= self.handle_internal_event_with_render_impact(ev);
        }
        (had_event, changed)
    }
}

#[cfg(test)]
mod tests {
    fn m832b_primary_key(pane: crate::layout::PaneId) -> crate::app::pane_graphics::Key {
        (
            pane,
            crate::api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID.to_owned(),
        )
    }

    #[test]
    fn m832b_late_queued_open_preserves_future_static_layer() {
        use crate::api::schema::*;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let (mut app, pane) = test_app_with_pane();
        app.state.ensure_test_terminals();
        app.state.kitty_graphics_enabled = true;
        let target = app.public_pane_id(0, pane).unwrap();
        let revision = app.pane_graphics.revision();
        let active = Arc::new(AtomicBool::new(true));
        let late = Request {
            id: "late".into(),
            method: Method::PaneGraphicsStreamOpen(PaneGraphicsStreamOpenParams {
                params: PaneGraphicsStreamParams {
                    pane_id: target.clone(),
                    layer_id: None,
                    z_index: 0,
                    owner: "late".into(),
                },
                active: active.clone(),
            }),
        };
        let retained = late.clone();
        let set: Request = serde_json::from_value(serde_json::json!({
            "id":"static", "method":"pane.graphics.set", "params": {
                "pane_id":target,"format":"rgba","image_width":1,"image_height":1,
                "data_base64":"AQIDBA=="}}))
        .unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        app.api_rx = rx;
        let (respond_to, replies) = std::sync::mpsc::channel();
        for request in [set, late] {
            tx.send(crate::api::ApiRequestMessage {
                request,
                respond_to: respond_to.clone(),
                caller: Default::default(),
                response_write_complete: None,
            })
            .unwrap();
        }
        active.store(false, Ordering::Release);
        assert!(app.drain_api_requests());
        let first: serde_json::Value = serde_json::from_str(&replies.try_recv().unwrap()).unwrap();
        let second: serde_json::Value = serde_json::from_str(&replies.try_recv().unwrap()).unwrap();
        assert_eq!(first["result"]["type"], "ok");
        assert_eq!(second["error"]["code"], "stream_closed");
        assert!(!app.sync_pane_graphics_streams());
        let slot = &app.pane_graphics.slots[&m832b_primary_key(pane)];
        assert!(slot.stream_owner.is_none());
        assert_eq!(
            slot.layer.as_ref().unwrap().inline_data().unwrap(),
            [1, 2, 3, 4]
        );
        assert_eq!(app.pane_graphics.revision(), revision.wrapping_add(1));
        drop(retained);
    }

    #[test]
    fn m832b_idle_cleanup_is_app_local_and_requests_full_render() {
        use crate::api::schema::*;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        for drop_token in [false, true] {
            let (mut a, pane) = test_app_with_pane();
            let (mut b, b_pane) = test_app_with_pane();
            let aa = Arc::new(AtomicBool::new(true));
            let bb = Arc::new(AtomicBool::new(true));
            for (app, active, pane) in [(&mut a, &aa, pane), (&mut b, &bb, b_pane)] {
                app.state.ensure_test_terminals();
                app.state.kitty_graphics_enabled = true;
                let target = app.public_pane_id(0, pane).unwrap();
                let response = app.handle_api_request(Request {
                    id: "open".into(),
                    method: Method::PaneGraphicsStreamOpen(PaneGraphicsStreamOpenParams {
                        params: PaneGraphicsStreamParams {
                            pane_id: target,
                            layer_id: None,
                            z_index: 0,
                            owner: "same".into(),
                        },
                        active: active.clone(),
                    }),
                });
                assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
            }
            a.pane_graphics
                .slots
                .get_mut(&m832b_primary_key(pane))
                .unwrap()
                .layer = Some(crate::app::pane_graphics::Layer::inline(
                PaneGraphicsFormat::Rgba,
                1,
                1,
                vec![1, 2, 3, 4],
                Default::default(),
                0,
            ));
            let revision = a.pane_graphics.revision();
            let _ = a.render_dirty.take();
            if drop_token {
                a.pane_graphics
                    .slots
                    .get_mut(&m832b_primary_key(pane))
                    .unwrap()
                    .stream_active = None;
            } else {
                aa.store(false, Ordering::Release);
            }
            assert!(a.drain_api_requests(), "drop_token={drop_token}");
            assert!(a.render_dirty.take().generic);
            assert!(a.pane_graphics.slots.is_empty());
            assert_eq!(a.pane_graphics.revision(), revision.wrapping_add(1));
            assert!(bb.load(Ordering::Acquire));
            assert!(!b.sync_pane_graphics_streams());
            assert_eq!(
                b.pane_graphics.slots[&m832b_primary_key(b_pane)]
                    .stream_owner
                    .as_deref(),
                Some("same")
            );
        }
    }

    #[test]
    fn m832b_request_boundary_cancels_removed_owner_before_next_dequeue() {
        use crate::api::schema::*;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let (mut app, pane) = test_app_with_pane();
        app.state.ensure_test_terminals();
        app.state.kitty_graphics_enabled = true;
        let target = app.public_pane_id(0, pane).unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let response = app.handle_api_request(Request {
            id: "open".into(),
            method: Method::PaneGraphicsStreamOpen(PaneGraphicsStreamOpenParams {
                params: PaneGraphicsStreamParams {
                    pane_id: target,
                    layer_id: None,
                    z_index: 0,
                    owner: "removed".into(),
                },
                active: active.clone(),
            }),
        });
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        app.state.workspaces.clear();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        app.api_rx = rx;
        let (respond_to, replies) = std::sync::mpsc::channel();
        for n in 0..3 {
            tx.send(crate::api::ApiRequestMessage {
                request: serde_json::from_value(
                    serde_json::json!({"id":format!("ping-{n}"),"method":"ping","params":{}}),
                )
                .unwrap(),
                respond_to: respond_to.clone(),
                caller: Default::default(),
                response_write_complete: None,
            })
            .unwrap();
        }
        let first = app.api_rx.try_recv().unwrap();
        app.handle_api_request_message(first);
        assert!(!active.load(Ordering::Acquire));
        assert!(app.pane_graphics.slots.is_empty());
        assert_eq!(app.api_rx.len(), 2);
        assert!(replies.try_recv().is_ok());
    }
    use super::*;
    use crate::app::state;
    use crate::workspace::Workspace;

    fn m828c_wrapper_request(
        app: &mut App,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: serde_json::from_value(
                serde_json::json!({"id": "m828c", "method": method, "params": params}),
            )
            .unwrap(),
            respond_to,
            caller: crate::api::ApiCaller::default(),
            response_write_complete: None,
        });
        let response: serde_json::Value =
            serde_json::from_str(&response_rx.recv_timeout(Duration::from_secs(1)).unwrap())
                .unwrap();
        assert_eq!(response["id"], "m828c");
        assert!(response.get("error").is_none(), "{response}");
        response
    }

    #[tokio::test]
    async fn m828c_monolithic_request_wrapper_syncs_observed_titles() {
        let (mut app, pane) = test_app_with_pane();
        app.state.ensure_test_terminals();
        let target = app.public_pane_id(0, pane).unwrap();
        let terminal = app.state.workspaces[0].terminal_id(pane).cloned().unwrap();
        let raw = "\u{25d0} compiling";
        app.state.terminals.get_mut(&terminal).unwrap().agent_name = Some("m828c-fixture".into());
        app.terminal_runtimes.insert(
            terminal.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
        let runtime = app.terminal_runtimes.get(&terminal).unwrap();
        runtime.test_process_pty_bytes(format!("\x1b]2;{raw}\x07").as_bytes());
        assert_eq!(runtime.agent_osc_title(), raw);
        app.render_dirty.request_terminal_title(pane);
        assert_eq!(app.state.terminals[&terminal].revision, 0);
        let sequence = app.event_hub.current_sequence();
        let first =
            m828c_wrapper_request(&mut app, "pane.get", serde_json::json!({"pane_id": target}));
        let expected = first["result"]["pane"].clone();
        assert_eq!(expected["terminal_title"], raw);
        assert_eq!(expected["terminal_title_stripped"], "compiling");
        assert_eq!(expected["revision"], 1);
        let events: Vec<_> = app
            .event_hub
            .events_after(sequence)
            .into_iter()
            .map(|(_, event)| serde_json::to_value(event).unwrap())
            .collect();
        assert_eq!(
            events,
            vec![
                serde_json::json!({"event": "pane_updated", "data": {"type": "pane_updated", "pane": expected}})
            ]
        );
        let sequence = app.event_hub.current_sequence();
        for (method, params, pointer) in [
            ("pane.list", serde_json::json!({}), "/result/panes/0"),
            (
                "agent.get",
                serde_json::json!({"target": target}),
                "/result/agent",
            ),
            ("agent.list", serde_json::json!({}), "/result/agents/0"),
        ] {
            let response = m828c_wrapper_request(&mut app, method, params);
            let info = response.pointer(pointer).unwrap();
            assert_eq!(info["terminal_title"], raw, "{method}");
            assert_eq!(info["terminal_title_stripped"], "compiling", "{method}");
            assert_eq!(info["revision"], 1, "{method}");
        }
        let spinner = "\u{25d1} compiling";
        app.terminal_runtimes
            .get(&terminal)
            .unwrap()
            .test_process_pty_bytes(format!("\x1b]2;{spinner}\x07").as_bytes());
        app.render_dirty.request_terminal_title(pane);
        let response =
            m828c_wrapper_request(&mut app, "pane.get", serde_json::json!({"pane_id": target}));
        assert_eq!(response["result"]["pane"]["terminal_title"], spinner);
        assert_eq!(response["result"]["pane"]["revision"], 1);
        assert!(app.event_hub.events_after(sequence).is_empty());
    }

    #[tokio::test]
    async fn m828c_token_and_title_writers_share_revision_without_substitution() {
        let (mut app, pane) = test_app_with_pane();
        app.state.ensure_test_terminals();
        let target = app.public_pane_id(0, pane).unwrap();
        let terminal = app.state.workspaces[0].terminal_id(pane).cloned().unwrap();
        app.state.terminals.get_mut(&terminal).unwrap().agent_name = Some("m828c-fixture".into());
        app.terminal_runtimes.insert(
            terminal.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
        let updates = |app: &App, sequence| {
            app.event_hub
                .events_after(sequence)
                .into_iter()
                .map(|(_, event)| serde_json::to_value(event).unwrap())
                .collect::<Vec<_>>()
        };
        let sequence = app.event_hub.current_sequence();
        let result = m828c_wrapper_request(
            &mut app,
            "pane.report_metadata",
            serde_json::json!({"pane_id": target, "source": "user:history", "seq": 1, "tokens": {"build": "ready"}}),
        );
        assert_eq!(result["result"]["type"], "ok");
        assert_eq!(app.state.terminals[&terminal].revision, 1);
        assert_eq!(app.agent_metadata_deadline, None);
        let first = serde_json::to_value(app.pane_info(0, pane).unwrap()).unwrap();
        assert_eq!(first["tokens"]["build"], "ready");
        assert!(first.get("terminal_title").is_none());
        assert_eq!(
            updates(&app, sequence),
            vec![
                serde_json::json!({"event": "pane_updated", "data": {"type": "pane_updated", "pane": first}})
            ]
        );

        for (raw, emits) in [("\u{25d0} compiling", true), ("\u{25d1} compiling", false)] {
            let sequence = app.event_hub.current_sequence();
            app.terminal_runtimes
                .get(&terminal)
                .unwrap()
                .test_process_pty_bytes(format!("\x1b]2;{raw}\x07").as_bytes());
            app.render_dirty.request_terminal_title(pane);
            let response =
                m828c_wrapper_request(&mut app, "pane.get", serde_json::json!({"pane_id": target}));
            let info = &response["result"]["pane"];
            assert_eq!(info["revision"], 2, "{raw}");
            assert_eq!(info["terminal_title"], raw);
            assert_eq!(info["terminal_title_stripped"], "compiling");
            assert_eq!(info["tokens"]["build"], "ready");
            assert_eq!(app.agent_metadata_deadline, None);
            let expected = if emits {
                vec![
                    serde_json::json!({"event": "pane_updated", "data": {"type": "pane_updated", "pane": info}}),
                ]
            } else {
                vec![]
            };
            assert_eq!(updates(&app, sequence), expected);
        }
        let sequence = app.event_hub.current_sequence();
        let earliest = Instant::now() + Duration::from_secs(30);
        m828c_wrapper_request(
            &mut app,
            "pane.report_metadata",
            serde_json::json!({"pane_id": target, "source": "user:history", "seq": 2, "ttl_ms": 30_000, "tokens": {"build": "ready"}}),
        );
        let latest = Instant::now() + Duration::from_secs(30);
        let deadline = app.state.terminals[&terminal]
            .metadata_tokens
            .next_expiry()
            .unwrap();
        assert!(deadline >= earliest && deadline <= latest);
        assert_eq!(app.agent_metadata_deadline, Some(deadline));
        assert_eq!(app.state.terminals[&terminal].revision, 3);
        let info = serde_json::to_value(app.pane_info(0, pane).unwrap()).unwrap();
        assert_eq!(info["tokens"]["build"], "ready");
        assert_eq!(info["terminal_title"], "\u{25d1} compiling");
        assert_eq!(
            updates(&app, sequence),
            vec![
                serde_json::json!({"event": "pane_updated", "data": {"type": "pane_updated", "pane": info}})
            ]
        );
        let sequence = app.event_hub.current_sequence();
        app.expire_metadata_at(deadline, deadline);
        assert_eq!(app.state.terminals[&terminal].revision, 4);
        assert!(app.state.terminals[&terminal]
            .metadata_tokens
            .values()
            .is_empty());
        assert_eq!(app.agent_metadata_deadline, None);
        let info = serde_json::to_value(app.pane_info(0, pane).unwrap()).unwrap();
        assert_eq!(
            updates(&app, sequence),
            vec![
                serde_json::json!({"event": "pane_updated", "data": {"type": "pane_updated", "pane": info}})
            ]
        );
        let sequence = app.event_hub.current_sequence();
        app.terminal_runtimes
            .get(&terminal)
            .unwrap()
            .test_process_pty_bytes(b"\x1b]2;finished\x07");
        let response =
            m828c_wrapper_request(&mut app, "pane.get", serde_json::json!({"pane_id": target}));
        let info = &response["result"]["pane"];
        assert_eq!(info["terminal_title"], "finished");
        assert_eq!(info["terminal_title_stripped"], "finished");
        assert_eq!(info["revision"], 5);
        assert_eq!(
            updates(&app, sequence),
            vec![
                serde_json::json!({"event": "pane_updated", "data": {"type": "pane_updated", "pane": info}})
            ]
        );
        let sequence = app.event_hub.current_sequence();
        let agent =
            m828c_wrapper_request(&mut app, "agent.get", serde_json::json!({"target": target}));
        assert_eq!(agent["result"]["agent"]["revision"], info["revision"]);
        assert_eq!(agent["result"]["agent"]["terminal_title"], "finished");
        for (method, params) in [
            (
                "pane.read",
                serde_json::json!({"pane_id": target, "source": "visible"}),
            ),
            (
                "agent.read",
                serde_json::json!({"target": target, "source": "visible"}),
            ),
        ] {
            let response = m828c_wrapper_request(&mut app, method, params);
            assert_eq!(response["result"]["read"]["revision"], 0, "{method}");
        }
        assert!(app.event_hub.events_after(sequence).is_empty());
    }

    #[test]
    fn m828b_pane_expiry_sweeps_at_now_and_preserves_workspace_and_presentation_order() {
        use crate::api::schema::{EventData, EventKind};

        let (mut app, first_pane) = test_app_with_pane();
        app.state.workspaces.push(Workspace::test_new("later-pane"));
        app.state.ensure_test_terminals();
        let second_pane = app.state.workspaces[1].tabs[0].root_pane;
        let targets = [
            app.public_pane_id(0, first_pane).unwrap(),
            app.public_pane_id(1, second_pane).unwrap(),
        ];
        let report = |app: &mut super::super::App, method: &str, params: serde_json::Value| {
            let request = serde_json::from_value(
                serde_json::json!({"id":"expiry-order", "method":method, "params":params}),
            )
            .unwrap();
            let response: serde_json::Value =
                serde_json::from_str(&app.handle_api_request(request)).unwrap();
            assert_eq!(response["result"]["type"], "ok");
        };
        report(
            &mut app,
            "pane.report_metadata",
            serde_json::json!({
                "pane_id":targets[0], "source":"legacy presentation", "title":"temporary title", "ttl_ms":30_000
            }),
        );
        let presentation_deadline = app.agent_metadata_deadline.unwrap();
        for (index, ttl) in [10_000, 20_000].into_iter().enumerate() {
            report(
                &mut app,
                "pane.report_metadata",
                serde_json::json!({
                    "pane_id":targets[index], "source":"user:tokens", "tokens":{"build":"due"}, "ttl_ms":ttl
                }),
            );
        }
        let workspace = app.public_workspace_id(0);
        report(
            &mut app,
            "workspace.report_metadata",
            serde_json::json!({
                "workspace_id":workspace, "source":"user:workspace", "tokens":{"status":"due"}, "ttl_ms":25_000
            }),
        );
        let terminal_ids = [
            app.state.workspaces[0]
                .pane_state(first_pane)
                .unwrap()
                .attached_terminal_id
                .clone(),
            app.state.workspaces[1]
                .pane_state(second_pane)
                .unwrap()
                .attached_terminal_id
                .clone(),
        ];
        let deadlines: Vec<_> = terminal_ids
            .iter()
            .map(|id| {
                app.state.terminals[id]
                    .metadata_tokens
                    .next_expiry()
                    .unwrap()
            })
            .collect();
        assert!(deadlines[0] < deadlines[1]);
        assert_eq!(app.agent_metadata_deadline, Some(deadlines[0]));
        let now = presentation_deadline.max(deadlines[1]).max(
            app.state.workspaces[0]
                .metadata_tokens
                .next_expiry()
                .unwrap(),
        ) + Duration::from_millis(1);
        let sequence = app.event_hub.current_sequence();
        app.expire_metadata_at(deadlines[0], now);
        for id in &terminal_ids {
            assert!(app.state.terminals[id].metadata_tokens.values().is_empty());
            assert_eq!(app.state.terminals[id].metadata_tokens.next_expiry(), None);
            assert_eq!(app.state.terminals[id].revision, 2);
        }
        assert!(app.state.terminals[&terminal_ids[0]]
            .agent_metadata
            .is_empty());
        assert!(app.state.workspaces[0].metadata_tokens.values().is_empty());
        assert_eq!(app.agent_metadata_deadline, None);
        let events = app.event_hub.events_after(sequence);
        assert_eq!(
            events
                .iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            vec![
                EventKind::PaneAgentStatusChanged,
                EventKind::PaneUpdated,
                EventKind::PaneUpdated,
                EventKind::WorkspaceMetadataUpdated,
            ]
        );
        assert!(
            matches!(&events[0].1.data, EventData::PaneAgentStatusChanged { pane_id, title: None, .. } if pane_id == &targets[0])
        );
        for (index, (ws_idx, pane)) in [(0, first_pane), (1, second_pane)].into_iter().enumerate() {
            assert_eq!(
                events[index + 1].1.data,
                EventData::PaneUpdated {
                    pane: app.pane_info(ws_idx, pane).unwrap()
                }
            );
        }
        assert_eq!(
            events[3].1.data,
            EventData::WorkspaceMetadataUpdated {
                workspace: app.workspace_info(0)
            }
        );
        let sequence = app.event_hub.current_sequence();
        app.expire_metadata_at(deadlines[0], now);
        assert!(app.event_hub.events_after(sequence).is_empty());
        assert_eq!(app.state.terminals[&terminal_ids[0]].revision, 2);
        assert_eq!(app.state.terminals[&terminal_ids[1]].revision, 2);
    }

    #[test]
    fn m828b_monolithic_api_read_expires_pane_tokens_first() {
        let (mut app, pane) = test_app_with_pane();
        app.state.ensure_test_terminals();
        assert!(app.pane_info(0, pane).is_some(), "registered pane fixture");
        let target = app.public_pane_id(0, pane).unwrap();
        let report = serde_json::from_value(serde_json::json!({"id": "m828b-seed", "method": "pane.report_metadata",
            "params": {"pane_id": target, "source": "user:runtime", "title": "due", "ttl_ms": 20, "tokens": {"status": "due"}}})).unwrap();
        let response: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(report)).unwrap();
        assert_eq!(response["result"]["type"], "ok");
        assert_eq!(
            serde_json::to_value(app.pane_info(0, pane).unwrap()).unwrap()["tokens"]["status"],
            "due"
        );
        let deadline = app.agent_metadata_deadline.expect("pane deadline");
        let limit = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            assert!(Instant::now() < limit, "pane deadline wait exceeded bound");
            std::thread::sleep(Duration::from_millis(1));
        }
        let before = serde_json::to_value(app.pane_info(0, pane).unwrap()).unwrap();
        assert_eq!(before["tokens"]["status"], "due");
        let sequence = app.event_hub.current_sequence();
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: serde_json::from_value(serde_json::json!({"id": "m828b-read", "method": "pane.get", "params": {"pane_id": target}})).unwrap(),
            respond_to,
            caller: crate::api::ApiCaller::default(),
            response_write_complete: None,
        });
        let response: serde_json::Value =
            serde_json::from_str(&response_rx.recv_timeout(Duration::from_secs(1)).unwrap())
                .unwrap();
        assert_eq!(response["id"], "m828b-read");
        assert_eq!(response["result"]["type"], "pane_info");
        assert!(
            response["result"]["pane"].get("tokens").is_none(),
            "{response}"
        );
        assert_eq!(
            response["result"]["pane"]["revision"].as_u64(),
            Some(before["revision"].as_u64().unwrap() + 1)
        );
        assert_eq!(app.agent_metadata_deadline, None);
        let updates = app
            .event_hub
            .events_after(sequence)
            .into_iter()
            .map(|(_, event)| serde_json::to_value(event).unwrap())
            .filter(|event| event["event"] == "pane_updated")
            .collect::<Vec<_>>();
        assert_eq!(
            updates,
            vec![
                serde_json::json!({"event": "pane_updated", "data": {"type": "pane_updated", "pane": response["result"]["pane"]}})
            ]
        );
    }

    #[test]
    fn m828a_monolithic_api_read_expires_workspace_tokens_first() {
        let (mut app, _) = test_app_with_pane();
        let workspace_id = app.public_workspace_id(0);
        let request = serde_json::from_value::<crate::api::schema::Request>(serde_json::json!({
            "id": "m828a-seed", "method": "workspace.report_metadata", "params": {
                "workspace_id": workspace_id, "source": "user:runtime", "ttl_ms": 20,
                "tokens": {"status": "due"}
            }
        }));
        assert!(
            request.is_ok(),
            "workspace report JSON refused: {request:?}"
        );
        let response: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request.unwrap())).unwrap();
        assert_eq!(response["result"]["type"], "ok");
        let deadline = app.agent_metadata_deadline.expect("workspace deadline");
        let limit = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            assert!(
                Instant::now() < limit,
                "workspace deadline wait exceeded bound"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            serde_json::to_value(app.workspace_info(0)).unwrap()["tokens"]["status"],
            "due"
        );
        let sequence = app.event_hub.current_sequence();
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: serde_json::from_value(serde_json::json!({
                "id": "m828a-read", "method": "workspace.get", "params": {"workspace_id": workspace_id}
            })).unwrap(),
            respond_to,
            caller: crate::api::ApiCaller::default(),
            response_write_complete: None,
        });
        let response: serde_json::Value =
            serde_json::from_str(&response_rx.recv_timeout(Duration::from_secs(1)).unwrap())
                .unwrap();
        assert_eq!(response["id"], "m828a-read");
        assert_eq!(response["result"]["type"], "workspace_info");
        assert!(
            response["result"]["workspace"].get("tokens").is_none(),
            "{response}"
        );
        assert_eq!(app.agent_metadata_deadline, None);
        let events = app
            .event_hub
            .events_after(sequence)
            .into_iter()
            .map(|(_, event)| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![serde_json::json!({
                "event": "workspace_metadata_updated", "data": {
                    "type": "workspace_metadata_updated", "workspace": response["result"]["workspace"]
                }
            })]
        );
    }

    #[test]
    fn hidden_render_attempt_keeps_presentation_cadence_available() {
        let (mut app, _) = test_app_with_pane();
        let initial_presentation = Instant::now();
        app.record_render_attempt(initial_presentation, true);

        let hidden_attempt = initial_presentation + MIN_RENDER_INTERVAL;
        app.record_render_attempt(hidden_attempt, false);
        let foreground_echo = hidden_attempt + Duration::from_millis(1);

        assert!(!app.can_render_now(foreground_echo));
        assert!(app.can_present_now(foreground_echo));
    }

    #[test]
    fn interrupted_custom_command_wait_keeps_child_for_retry() {
        let interrupted = std::io::Error::new(std::io::ErrorKind::Interrupted, "test interrupt");

        assert!(retain_detached_process_after_wait(42, Err(interrupted)));
    }

    #[test]
    fn m828a_scheduled_expiry_preserves_agent_presentation_and_workspace_tokens() {
        use crate::api::schema::{EventData, EventKind};
        use crate::events::AppEvent;

        let (mut app, pane_id) = test_app_with_pane();
        app.state.ensure_test_terminals();
        app.next_resize_poll = Instant::now() + Duration::from_secs(3600);
        app.handle_internal_event(AppEvent::HookStateReported {
            pane_id,
            source: "zynk:pi".into(),
            agent_label: "pi".into(),
            state: crate::detect::AgentState::Working,
            message: None,

            seq: None,
            session_ref: None,
        });
        app.handle_internal_event(AppEvent::HookMetadataReported {
            pane_id,
            source: "user:pi-display".into(),
            agent_label: Some("pi".into()),
            applies_to_source: Some("zynk:pi".into()),
            title: Some("temporary title".into()),
            display_agent: Some("display pi".into()),

            state_labels: std::collections::HashMap::from([(
                "working".into(),
                "busy label".into(),
            )]),
            clear_title: false,
            clear_display_agent: false,

            clear_state_labels: false,
            seq: Some(4),
            ttl: Some(Duration::from_secs(60)),
        });
        let pane_deadline = app.agent_metadata_deadline.expect("presentation deadline");
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.state, crate::detect::AgentState::Working);
        assert!(terminal.agent_metadata.contains_key("user:pi-display"));
        assert_eq!(
            terminal.effective_title().as_deref(),
            Some("temporary title")
        );
        let authority = terminal.hook_authority.clone();
        let identity = terminal.hook_identity.clone();
        let report = serde_json::from_value(serde_json::json!({
            "id": "co-expiry", "method": "workspace.report_metadata", "params": {
                "workspace_id": app.state.workspaces[0].id, "source": "user:build",
                "ttl_ms": 1000, "seq": 2, "tokens": {"build": "ready"}
            }
        }))
        .unwrap();
        let response: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(report)).unwrap();
        assert_eq!(response["result"]["type"], "ok");
        let token_deadline = app.state.workspaces[0]
            .metadata_tokens
            .next_expiry()
            .unwrap();
        assert_eq!(
            app.agent_metadata_deadline,
            Some(pane_deadline.min(token_deadline))
        );
        app.state.toast = Some(state::ToastNotification {
            kind: state::ToastKind::Finished,
            title: "existing toast".into(),
            context: "existing context".into(),
            position: None,
            target: Some(state::ToastTarget {
                workspace_id: app.state.workspaces[0].id.clone(),
                pane_id,
            }),
        });
        app.toast_deadline = Some(app.next_resize_poll);
        let toast = app.state.toast.clone();
        let start = app.event_hub.current_sequence();
        let now = pane_deadline.max(token_deadline) + Duration::from_nanos(1);
        assert!(app.handle_scheduled_tasks(now, false));
        let terminal = &app.state.terminals[&terminal_id];
        assert!(
            terminal.agent_metadata.is_empty(),
            "stored presentation must be purged"
        );
        assert_eq!(terminal.effective_title(), None);
        assert_eq!(terminal.state, crate::detect::AgentState::Working);
        assert_eq!(terminal.hook_authority, authority);
        assert_eq!(terminal.hook_identity, identity);
        assert_eq!(app.state.toast, toast);
        assert_eq!(app.agent_metadata_deadline, None);
        assert!(app.state.workspaces[0].metadata_tokens.values().is_empty());
        let events = app.event_hub.events_after(start);
        assert_eq!(events.len(), 2, "fresh suffix only: {events:?}");
        assert_eq!(events[0].1.event, EventKind::PaneAgentStatusChanged);
        assert!(
            matches!(&events[0].1.data, EventData::PaneAgentStatusChanged {
            agent_status: crate::api::schema::AgentStatus::Working,
            title: None, display_agent: None, state_labels, ..
        } if state_labels.is_empty())
        );
        assert_eq!(events[1].1.event, EventKind::WorkspaceMetadataUpdated);
        assert!(
            matches!(&events[1].1.data, EventData::WorkspaceMetadataUpdated { workspace }
            if workspace.tokens.is_empty() && workspace.workspace_id == app.state.workspaces[0].id)
        );
        let after = app.event_hub.current_sequence();
        assert!(!app.handle_scheduled_tasks(now, false));
        assert!(app.event_hub.events_after(after).is_empty());
    }

    fn test_app_with_pane() -> (super::super::App, crate::layout::PaneId) {
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        app.state.workspaces.push(ws);
        app.state.active = Some(0);
        app.state.view.pane_infos.push(crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, 80, 24),
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::NONE,
            is_focused: true,
        });
        (app, pane_id)
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_metrics_unavailable() {
        // Without a runtime, pane_scroll_metrics returns None.
        // Fail-closed: stop autoscroll instead of rescheduling forever.
        let (mut app, pane_id) = test_app_with_pane();
        let now = Instant::now();
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        // Drag to a different cell so it becomes Dragging
        sel.drag(5, 5, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 5,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        // Should stop because no runtime metrics available
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_selection_done() {
        let (mut app, pane_id) = test_app_with_pane();
        let now = Instant::now();
        // Create a selection that is already finished (not in progress)
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        // Drag to a different cell so it becomes visible, then finish
        sel.drag(5, 5, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        sel.finish(); // now it's Done, not in progress
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_selection_cleared() {
        let (mut app, _pane_id) = test_app_with_pane();
        let now = Instant::now();
        app.state.selection = None;
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_when_selection_anchored() {
        // Anchored (click, no drag) should not keep the timer running.
        let (mut app, pane_id) = test_app_with_pane();
        let now = Instant::now();
        app.state.selection = Some(crate::selection::Selection::anchor(pane_id, 0, 0, None));
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    /// Creates an app with a real TerminalRuntime (no PTY) so scroll_metrics
    /// returns meaningful data. Uses test_with_scrollback_bytes.
    fn test_app_with_runtime(
        cols: u16,
        rows: u16,
        bytes: &[u8],
    ) -> (super::super::App, crate::layout::PaneId) {
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let runtime =
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(cols, rows, 0, bytes);
        ws.tabs[0].runtimes.insert(pane_id, runtime);
        app.state.workspaces.push(ws);
        app.state.active = Some(0);
        app.state.view.pane_infos.push(crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, cols, rows),
            inner_rect: ratatui::layout::Rect::new(0, 0, cols, rows),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::NONE,
            is_focused: true,
        });
        (app, pane_id)
    }

    #[tokio::test]
    async fn tick_selection_autoscroll_stops_at_scrollback_top() {
        // Create a runtime with no scrollback content — we're already at
        // the top (offset_from_bottom == max_offset_from_bottom).
        let (mut app, pane_id) = test_app_with_runtime(80, 24, &[]);
        let now = Instant::now();
        let mut sel = crate::selection::Selection::anchor(pane_id, 5, 5, None);
        sel.drag(0, 0, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Up,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 0,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        // At scrollback top, can't scroll further up — should stop
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[tokio::test]
    async fn tick_selection_autoscroll_stops_at_scrollback_bottom() {
        // Create a runtime with no scrollback content — we're already at
        // the bottom (offset_from_bottom == 0).
        let (mut app, pane_id) = test_app_with_runtime(80, 24, &[]);
        let now = Instant::now();
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        sel.drag(5, 5, ratatui::layout::Rect::new(0, 0, 80, 24), None);
        app.state.selection = Some(sel);
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 5,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        // At scrollback bottom, can't scroll further down — should stop
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[tokio::test]
    async fn passive_mouse_motion_does_not_request_monolithic_render() {
        let (mut app, _) = test_app_with_pane();
        app.state.mode = crate::app::Mode::Terminal;
        let motion = || {
            crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Moved,
                column: 10,
                row: 5,
                modifiers: crossterm::event::KeyModifiers::empty(),
            })
        };

        assert!(!app.handle_raw_input_event(motion()).await);
        app.state.mode = crate::app::Mode::GlobalMenu;
        assert!(app.handle_raw_input_event(motion()).await);
    }

    #[tokio::test]
    async fn raw_input_batch_does_not_start_pending_agent_resume_before_render() {
        let (mut app, pane_id) = test_app_with_pane();
        app.state.ensure_test_terminals();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane should have a terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "zynk:codex\0codex\0Id\0codex-session".into(),
        });

        assert!(
            app.handle_raw_input_batch(crate::raw_input::RawInputEvent::HostDefaultColor {
                kind: crate::terminal_theme::DefaultColorKind::Foreground,
                color: crate::terminal_theme::RgbColor {
                    r: 220,
                    g: 220,
                    b: 220,
                },
            })
            .await
        );
        assert!(
            app.terminal_runtimes.get(&terminal_id).is_none(),
            "raw input can mutate active geometry; pending resumes must wait for render to refresh pane_infos"
        );
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
    }

    #[tokio::test]
    async fn scheduled_tasks_do_not_start_pending_agent_resume_when_geometry_dirty() {
        let (mut app, pane_id) = test_app_with_pane();
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane should have a terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "zynk:codex\0codex\0Id\0codex-session".into(),
        });
        app.pending_agent_resume_deadline = Some(Instant::now() - Duration::from_millis(1));

        assert!(!app.handle_scheduled_tasks(Instant::now(), true));
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
        assert!(app.pending_agent_resume_deadline.is_none());
    }
}
