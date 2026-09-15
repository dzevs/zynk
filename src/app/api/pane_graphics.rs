// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use base64::Engine;
use std::sync::{atomic::Ordering, Arc};

use crate::api::schema::{
    PaneGraphicsClearParams, PaneGraphicsSetParams, PaneGraphicsStreamOpenParams,
    PaneGraphicsStreamParams, ResponseResult, PANE_GRAPHICS_SET_MAX_BYTES,
    PANE_GRAPHICS_STREAM_MAX_BYTES,
};
use crate::app::state::PaneGraphicsLayer;
use crate::app::App;
use crate::layout::PaneId;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_pane_graphics_info(
        &mut self,
        id: String,
        target: crate::api::schema::PaneTarget,
    ) -> String {
        if let Err(response) = require_pane_graphics_enabled(self, &id) {
            return response;
        }
        if self.parse_pane_id(&target.pane_id).is_none() {
            return pane_not_found(id, &target.pane_id);
        }
        if !self.state.host_cell_size.is_known() {
            return encode_error(id, "cell_size_unavailable", "host cell size is unavailable");
        }
        encode_success(
            id,
            ResponseResult::PaneGraphicsInfo {
                cell_width_px: self.state.host_cell_size.width_px,
                cell_height_px: self.state.host_cell_size.height_px,
            },
        )
    }

    pub(super) fn handle_pane_graphics_set(
        &mut self,
        id: String,
        params: PaneGraphicsSetParams,
    ) -> String {
        if let Err(response) = require_pane_graphics_enabled(self, &id) {
            return response;
        }
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if self.state.pane_graphics_streams.contains_key(&pane_id) {
            return stream_conflict(id);
        }
        self.set_pane_graphics_layer(id, pane_id, params, PANE_GRAPHICS_SET_MAX_BYTES)
    }

    fn set_pane_graphics_layer(
        &mut self,
        id: String,
        pane_id: PaneId,
        params: PaneGraphicsSetParams,
        max_bytes: usize,
    ) -> String {
        if params.image_width == 0 || params.image_height == 0 {
            return encode_error(
                id,
                "invalid_image",
                "image_width and image_height must be greater than zero",
            );
        }
        let data = match params.data {
            Some(data) => data,
            None => match decode_public_image(&params.data_base64) {
                Ok(data) => data,
                Err((code, message)) => return encode_error(id, code, message),
            },
        };
        if data.len() > max_bytes {
            return encode_error(id, "image_too_large", "image data is too large");
        }
        if data.is_empty() {
            return encode_error(id, "invalid_image", "image data must not be empty");
        }
        match pane_graphics_expected_data_len(
            params.format,
            params.image_width,
            params.image_height,
        ) {
            Ok(Some(expected_len)) if data.len() != expected_len => {
                return encode_error(
                    id,
                    "invalid_image",
                    "image data length does not match format and dimensions",
                );
            }
            Ok(_) => {}
            Err(()) => {
                return encode_error(id, "invalid_image", "image dimensions are too large");
            }
        }

        let layer = PaneGraphicsLayer::new(
            params.format,
            params.image_width,
            params.image_height,
            data,
            params.placement,
        );
        self.state.pane_graphics_layers.insert(pane_id, layer);
        self.state.pane_graphics_revision = self.state.pane_graphics_revision.wrapping_add(1);
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_clear(
        &mut self,
        id: String,
        params: PaneGraphicsClearParams,
    ) -> String {
        if let Err(response) = require_pane_graphics_enabled(self, &id) {
            return response;
        }
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if self.state.pane_graphics_streams.contains_key(&pane_id) {
            return stream_conflict(id);
        }
        if self.state.pane_graphics_layers.remove(&pane_id).is_some() {
            self.state.pane_graphics_revision = self.state.pane_graphics_revision.wrapping_add(1);
        }
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_stream_open(
        &mut self,
        id: String,
        open: PaneGraphicsStreamOpenParams,
    ) -> String {
        if let Err(response) = require_pane_graphics_enabled(self, &id) {
            return response;
        }
        if !open.active.load(Ordering::Acquire) {
            return encode_error(id, "stream_closed", "pane graphics stream is not active");
        }
        let params = open.params;
        if params.owner.is_empty() {
            return encode_error(
                id,
                "invalid_stream",
                "pane graphics stream owner is required",
            );
        }
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if self.state.pane_graphics_streams.contains_key(&pane_id)
            || self
                .pane_graphics_stream_registrations
                .contains_key(&params.owner)
        {
            return stream_conflict(id);
        }
        self.state
            .pane_graphics_streams
            .insert(pane_id, params.owner.clone());
        self.pane_graphics_stream_registrations
            .insert(params.owner, (pane_id, Arc::downgrade(&open.active)));
        self.state.pane_graphics_layers.remove(&pane_id);
        self.state.pane_graphics_revision = self.state.pane_graphics_revision.wrapping_add(1);
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_stream_set(
        &mut self,
        id: String,
        params: PaneGraphicsSetParams,
    ) -> String {
        if let Err(response) = require_pane_graphics_enabled(self, &id) {
            return response;
        }
        let pane_id = self
            .pane_graphics_stream_registrations
            .get(&params.owner)
            .filter(|(pane, active)| {
                self.state.pane_graphics_streams.get(pane) == Some(&params.owner)
                    && active
                        .upgrade()
                        .is_some_and(|active| active.load(Ordering::Acquire))
            })
            .map(|(pane, _)| *pane);
        let Some(pane_id) = pane_id else {
            return encode_error(id, "stream_closed", "pane graphics stream is not active");
        };
        self.set_pane_graphics_layer(id, pane_id, params, PANE_GRAPHICS_STREAM_MAX_BYTES)
    }

    pub(super) fn handle_pane_graphics_stream_close(
        &mut self,
        id: String,
        params: PaneGraphicsStreamParams,
    ) -> String {
        if let Some((pane_id, active)) = self
            .pane_graphics_stream_registrations
            .remove(&params.owner)
        {
            if let Some(active) = active.upgrade() {
                active.store(false, Ordering::Release);
            }
            if self.state.pane_graphics_streams.get(&pane_id) == Some(&params.owner) {
                self.state.pane_graphics_streams.remove(&pane_id);
                if self.state.pane_graphics_layers.remove(&pane_id).is_some() {
                    self.state.pane_graphics_revision =
                        self.state.pane_graphics_revision.wrapping_add(1);
                }
            }
        }
        encode_success(id, ResponseResult::Ok {})
    }

    pub(crate) fn sync_pane_graphics_streams(&mut self) -> bool {
        let mut changed = false;
        self.pane_graphics_stream_registrations
            .retain(|owner, (pane, weak)| {
                let active = weak.upgrade();
                let owns = self.state.pane_graphics_streams.get(pane) == Some(owner);
                if owns
                    && self.state.kitty_graphics_enabled
                    && active.as_ref().is_some_and(|a| a.load(Ordering::Acquire))
                {
                    return true;
                }
                if let Some(active) = active {
                    active.store(false, Ordering::Release);
                }
                if owns {
                    self.state.pane_graphics_streams.remove(pane);
                    changed |= self.state.pane_graphics_layers.remove(pane).is_some();
                }
                false
            });
        if changed {
            self.state.pane_graphics_revision = self.state.pane_graphics_revision.wrapping_add(1);
            self.render_dirty.request_generic();
        }
        changed
    }
}

fn stream_conflict(id: String) -> String {
    encode_error(
        id,
        "stream_conflict",
        "pane already has an active graphics stream",
    )
}

fn decode_public_image(encoded: &str) -> Result<Vec<u8>, (&'static str, &'static str)> {
    const MAX_ENCODED_BYTES: usize = PANE_GRAPHICS_SET_MAX_BYTES.div_ceil(3) * 4;
    if encoded.len() > MAX_ENCODED_BYTES {
        return Err(("image_too_large", "image data is too large"));
    }
    let capacity = base64::decoded_len_estimate(encoded.len()).min(PANE_GRAPHICS_SET_MAX_BYTES);
    let mut data = vec![0; capacity];
    match base64::engine::general_purpose::STANDARD.decode_slice(encoded, &mut data) {
        Ok(len) => {
            data.truncate(len);
            Ok(data)
        }
        Err(base64::DecodeSliceError::OutputSliceTooSmall) => {
            Err(("image_too_large", "image data is too large"))
        }
        Err(base64::DecodeSliceError::DecodeError(_)) => {
            Err(("invalid_image", "data_base64 is not valid base64"))
        }
    }
}

fn require_pane_graphics_enabled(app: &App, id: &str) -> Result<(), String> {
    if app.state.kitty_graphics_enabled {
        return Ok(());
    }
    Err(encode_error(
        id.to_owned(),
        "feature_disabled",
        "pane graphics require experimental.kitty_graphics",
    ))
}

fn pane_graphics_expected_data_len(
    format: crate::api::schema::PaneGraphicsFormat,
    image_width: u32,
    image_height: u32,
) -> Result<Option<usize>, ()> {
    let bytes_per_pixel = match format {
        crate::api::schema::PaneGraphicsFormat::Png => return Ok(None),
        crate::api::schema::PaneGraphicsFormat::Rgb => 3_u64,
        crate::api::schema::PaneGraphicsFormat::Rgba => 4_u64,
    };
    let pixels = u64::from(image_width)
        .checked_mul(u64::from(image_height))
        .ok_or(())?;
    pixels
        .checked_mul(bytes_per_pixel)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .map(Some)
        .ok_or(())
}

fn pane_not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn m832b_claim_conflicts_and_cleanup_follow_resolved_pane() {
        use crate::api::schema::*;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let (mut app, _, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        app.state
            .workspaces
            .push(crate::workspace::Workspace::test_new("other"));
        app.state.ensure_test_terminals();
        let other = app.state.workspaces[1].tabs[0].root_pane;
        let active = Arc::new(AtomicBool::new(true));
        let open = |owner: &str, active: Arc<AtomicBool>| Request {
            id: "open".into(),
            method: Method::PaneGraphicsStreamOpen(PaneGraphicsStreamOpenParams {
                params: PaneGraphicsStreamParams {
                    pane_id: "1:p1".into(),
                    owner: owner.into(),
                },
                active,
            }),
        };
        let response = app.handle_api_request(open("owner", active.clone()));
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert_eq!(app.state.pane_graphics_streams[&pane], "owner");
        app.state
            .pane_graphics_layers
            .insert(pane, m832a_static_seed());
        let before = app.state.pane_graphics_layers.clone();
        let revision = app.state.pane_graphics_revision;
        for method in ["pane.graphics.set", "pane.graphics.clear"] {
            let extra = if method.ends_with("set") {
                serde_json::json!({"format":"rgba",
                "image_width":1,"image_height":1,"data_base64":"BQYHCA=="})
            } else {
                serde_json::json!({})
            };
            let response: ErrorResponse = serde_json::from_str(
                &app.handle_api_request(m832a_static_request(method, "1:p1", extra)),
            )
            .unwrap();
            assert_eq!(response.error.code, "stream_conflict", "method={method}");
            assert_eq!(app.state.pane_graphics_layers, before);
            assert_eq!(app.state.pane_graphics_revision, revision);
        }
        let response: ErrorResponse = serde_json::from_str(
            &app.handle_api_request(open("second", Arc::new(AtomicBool::new(true)))),
        )
        .unwrap();
        assert_eq!(response.error.code, "stream_conflict");
        assert_eq!(app.state.pane_graphics_layers, before);
        app.state.workspaces.swap(0, 1);
        app.state
            .pane_graphics_layers
            .insert(other, m832a_static_seed());
        let response = app.handle_api_request(Request {
            id: "frame".into(),
            method: Method::PaneGraphicsStreamSet(PaneGraphicsSetParams {
                pane_id: "1:p1".into(),
                owner: "owner".into(),
                format: PaneGraphicsFormat::Rgba,
                image_width: 1,
                image_height: 1,
                data: Some(vec![5, 6, 7, 8]),
                data_base64: String::new(),
                placement: Default::default(),
            }),
        });
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert_eq!(app.state.pane_graphics_layers[&pane].data, vec![5, 6, 7, 8]);
        assert_eq!(
            app.state.pane_graphics_layers[&other].data,
            vec![1, 2, 3, 4]
        );
        let close = || Request {
            id: "close".into(),
            method: Method::PaneGraphicsStreamClose(PaneGraphicsStreamParams {
                pane_id: "missing-spelling".into(),
                owner: "owner".into(),
            }),
        };
        let response = app.handle_api_request(close());
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert!(!active.load(Ordering::Acquire));
        assert!(!app.state.pane_graphics_layers.contains_key(&pane));
        assert_eq!(
            app.state.pane_graphics_layers[&other].data,
            vec![1, 2, 3, 4]
        );
        let successor = Arc::new(AtomicBool::new(true));
        let response = app.handle_api_request(Request {
            id: "successor".into(),
            method: Method::PaneGraphicsStreamOpen(PaneGraphicsStreamOpenParams {
                params: PaneGraphicsStreamParams {
                    pane_id: app.public_pane_id(1, pane).unwrap(),
                    owner: "successor".into(),
                },
                active: successor.clone(),
            }),
        });
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        app.state
            .pane_graphics_layers
            .insert(pane, m832a_static_seed());
        let response = app.handle_api_request(close());
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert_eq!(app.state.pane_graphics_streams[&pane], "successor");
        assert!(successor.load(Ordering::Acquire));
        assert_eq!(app.state.pane_graphics_layers[&pane].data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn m832b_disabled_stream_admission_keeps_cleanup_available() {
        use crate::api::schema::*;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let (mut app, target, pane) = m832a_static_app();
        let active = Arc::new(AtomicBool::new(true));
        let open = |target: &str| Request {
            id: "open".into(),
            method: Method::PaneGraphicsStreamOpen(PaneGraphicsStreamOpenParams {
                params: PaneGraphicsStreamParams {
                    pane_id: target.into(),
                    owner: "disabled".into(),
                },
                active: active.clone(),
            }),
        };
        let frame = |target: &str| Request {
            id: "frame".into(),
            method: Method::PaneGraphicsStreamSet(PaneGraphicsSetParams {
                pane_id: target.into(),
                owner: "disabled".into(),
                format: PaneGraphicsFormat::Rgba,
                image_width: 0,
                image_height: 0,
                data: Some(vec![1]),
                data_base64: String::new(),
                placement: Default::default(),
            }),
        };
        for request in [open("missing"), frame("missing")] {
            let response: ErrorResponse =
                serde_json::from_str(&app.handle_api_request(request)).unwrap();
            assert_eq!(response.error.code, "feature_disabled");
            assert!(app.state.pane_graphics_streams.is_empty());
            assert!(app.state.pane_graphics_layers.is_empty());
        }
        app.state.kitty_graphics_enabled = true;
        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_api_request(open(&target))).is_ok()
        );
        app.state
            .pane_graphics_layers
            .insert(pane, m832a_static_seed());
        app.state.kitty_graphics_enabled = false;
        let before = app.state.pane_graphics_layers.clone();
        let response: ErrorResponse =
            serde_json::from_str(&app.handle_api_request(frame(&target))).unwrap();
        assert_eq!(response.error.code, "feature_disabled");
        assert_eq!(app.state.pane_graphics_layers, before);
        let close = Request {
            id: "close".into(),
            method: Method::PaneGraphicsStreamClose(PaneGraphicsStreamParams {
                pane_id: "missing".into(),
                owner: "disabled".into(),
            }),
        };
        assert!(serde_json::from_str::<SuccessResponse>(&app.handle_api_request(close)).is_ok());
        assert!(!active.load(Ordering::Acquire));
        assert!(app.state.pane_graphics_layers.is_empty());
        assert!(app.state.pane_graphics_streams.is_empty());
        assert!(app.pane_graphics_stream_registrations.is_empty());
    }

    #[test]
    fn m832b_raw_stream_cap_and_invalid_frame_preserve_claim_and_layer() {
        use crate::api::schema::*;
        use std::sync::{atomic::AtomicBool, Arc};
        let (mut app, target, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        let active = Arc::new(AtomicBool::new(true));
        let response = app.handle_api_request(Request {
            id: "open".into(),
            method: Method::PaneGraphicsStreamOpen(PaneGraphicsStreamOpenParams {
                params: PaneGraphicsStreamParams {
                    pane_id: target.clone(),
                    owner: "cap".into(),
                },
                active: active.clone(),
            }),
        });
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        let request = |length, format, width| Request {
            id: "frame".into(),
            method: Method::PaneGraphicsStreamSet(PaneGraphicsSetParams {
                pane_id: target.clone(),
                owner: "cap".into(),
                format,
                image_width: width,
                image_height: 1,
                data: Some(vec![1; length]),
                data_base64: String::new(),
                placement: Default::default(),
            }),
        };
        assert_eq!(PANE_GRAPHICS_STREAM_MAX_BYTES, 16 * 1024 * 1024);
        let response = app.handle_api_request(request(
            PANE_GRAPHICS_STREAM_MAX_BYTES,
            PaneGraphicsFormat::Png,
            1,
        ));
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert_eq!(
            app.state.pane_graphics_layers[&pane].data.len(),
            PANE_GRAPHICS_STREAM_MAX_BYTES
        );
        let before = app.state.pane_graphics_layers.clone();
        let revision = app.state.pane_graphics_revision;
        for (length, format, width, code) in [
            (
                PANE_GRAPHICS_STREAM_MAX_BYTES + 1,
                PaneGraphicsFormat::Png,
                1,
                "image_too_large",
            ),
            (3, PaneGraphicsFormat::Rgba, 1, "invalid_image"),
            (4, PaneGraphicsFormat::Rgba, 0, "invalid_image"),
        ] {
            let response: ErrorResponse =
                serde_json::from_str(&app.handle_api_request(request(length, format, width)))
                    .unwrap();
            assert_eq!(response.error.code, code);
            assert_eq!(app.state.pane_graphics_layers, before);
            assert_eq!(app.state.pane_graphics_revision, revision);
            assert_eq!(app.state.pane_graphics_streams[&pane], "cap");
        }
    }
    use crate::app::App;

    fn m832a_static_app() -> (App, String, crate::layout::PaneId) {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("static-graphics")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.public_pane_id(0, pane).unwrap();
        (app, target, pane)
    }

    fn m832a_static_request(
        method: &str,
        target: &str,
        extra: serde_json::Value,
    ) -> crate::api::schema::Request {
        let mut params = serde_json::json!({"pane_id": target});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(
            serde_json::json!({"id": "static-api", "method": method, "params": params}),
        )
        .unwrap()
    }

    fn m832a_static_seed() -> crate::app::state::PaneGraphicsLayer {
        crate::app::state::PaneGraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Rgba,
            1,
            1,
            vec![1, 2, 3, 4],
            crate::api::schema::PaneGraphicsPlacementParams::default(),
        )
    }

    #[test]
    fn m832a_all_static_methods_are_gated_without_mutation() {
        for method in [
            "pane.graphics.set",
            "pane.graphics.clear",
            "pane.graphics.info",
        ] {
            let (mut app, target, pane) = m832a_static_app();
            assert!(!app.state.kitty_graphics_enabled);
            app.state
                .pane_graphics_layers
                .insert(pane, m832a_static_seed());
            app.state.pane_graphics_revision = 17;
            let before = app.state.pane_graphics_layers.clone();
            let cases = if method == "pane.graphics.set" { 3 } else { 2 };
            for case in 0..cases {
                let request_target = if case == 1 { "missing-pane" } else { &target };
                let extra = if method == "pane.graphics.set" {
                    if case == 2 {
                        serde_json::json!({"format": "rgba", "image_width": 0, "image_height": 1, "data_base64": "!!!!"})
                    } else {
                        serde_json::json!({"format": "rgba", "image_width": 1, "image_height": 1, "data_base64": "BQYHCA=="})
                    }
                } else {
                    serde_json::json!({})
                };
                let response: crate::api::schema::ErrorResponse = serde_json::from_str(
                    &app.handle_api_request(m832a_static_request(method, request_target, extra)),
                )
                .unwrap();
                assert_eq!(
                    response.error.code, "feature_disabled",
                    "method={method} case={case}"
                );
                assert_eq!(
                    app.state.pane_graphics_layers, before,
                    "method={method} case={case}"
                );
                assert_eq!(app.state.pane_graphics_revision, 17);
                assert!(!app.state.session_dirty);
            }
        }
    }

    #[tokio::test]
    async fn m832a_static_set_replace_clear_preserve_terminal_and_session_state() {
        let (mut app, target, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        let terminal = app.state.terminal_id_for_pane(0, pane).unwrap();
        app.state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                crate::detect::AgentState::Working,
                Some("static preservation".into()),
                crate::agent_resume::AgentSessionRef::path("/m832a-fixture/session.jsonl"),
                Some(20),
            )
            .unwrap();
        let authority = app.state.terminals[&terminal].hook_authority.clone();
        assert!(authority.as_ref().unwrap().session_ref.is_some());
        let persisted = app.state.terminals[&terminal]
            .persisted_agent_session
            .clone();
        assert!(persisted.is_none());
        app.terminal_runtimes.insert(
            terminal.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                80,
                24,
                b"\x1b[31mterminal sentinel\x1b[0m",
            ),
        );
        let text = app.terminal_runtimes.get(&terminal).unwrap().visible_text();
        let ansi = app.terminal_runtimes.get(&terminal).unwrap().visible_ansi();
        assert!(text.contains("terminal sentinel"));
        let terminal_revision = app.state.terminals[&terminal].revision;
        let sequence = app.event_hub.current_sequence();
        let unchanged = |app: &App| {
            assert_eq!(app.state.terminals[&terminal].hook_authority, authority);
            assert_eq!(
                app.state.terminals[&terminal].persisted_agent_session,
                persisted
            );
            assert_eq!(app.state.terminals[&terminal].revision, terminal_revision);
            assert_eq!(
                app.terminal_runtimes.get(&terminal).unwrap().visible_text(),
                text
            );
            assert_eq!(
                app.terminal_runtimes.get(&terminal).unwrap().visible_ansi(),
                ansi
            );
            assert_eq!(app.event_hub.current_sequence(), sequence);
            assert!(!app.state.session_dirty);
        };
        app.state.pane_graphics_revision = u64::MAX;
        for (data, expected, revision) in [
            ("AQIDBA==", vec![1, 2, 3, 4], 0),
            ("BQYHCA==", vec![5, 6, 7, 8], 1),
        ] {
            let request = m832a_static_request(
                "pane.graphics.set",
                &target,
                serde_json::json!({
                "format": "rgba", "image_width": 1, "image_height": 1, "data_base64": data,
                "placement": {"viewport_col": -1, "viewport_row": 2, "grid_cols": 3, "grid_rows": 4}}),
            );
            let response: crate::api::schema::SuccessResponse =
                serde_json::from_str(&app.handle_api_request(request)).unwrap();
            assert!(matches!(
                response.result,
                crate::api::schema::ResponseResult::Ok {}
            ));
            assert_eq!(app.state.pane_graphics_layers[&pane].data, expected);
            assert_eq!(app.state.pane_graphics_layers[&pane].image_width, 1);
            assert_eq!(app.state.pane_graphics_layers[&pane].image_height, 1);
            assert_eq!(
                app.state.pane_graphics_layers[&pane].render.viewport_col,
                -1
            );
            assert_eq!(app.state.pane_graphics_layers[&pane].render.grid_rows, 4);
            assert_eq!(app.state.pane_graphics_layers[&pane].render.grid_cols, 3);
            assert_eq!(app.state.pane_graphics_revision, revision);
            unchanged(&app);
        }
        for _ in 0..2 {
            let response: crate::api::schema::SuccessResponse =
                serde_json::from_str(&app.handle_api_request(m832a_static_request(
                    "pane.graphics.clear",
                    &target,
                    serde_json::json!({}),
                )))
                .unwrap();
            assert!(matches!(
                response.result,
                crate::api::schema::ResponseResult::Ok {}
            ));
            assert!(!app.state.pane_graphics_layers.contains_key(&pane));
            assert_eq!(app.state.pane_graphics_revision, 2);
            unchanged(&app);
        }
    }

    #[test]
    fn m832a_static_payload_boundaries_and_refusals_preserve_prior_layer() {
        use base64::Engine as _;
        let cap = 512 * 1024;
        let (mut app, target, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        for length in [cap - 2, cap - 1, cap] {
            let bytes = vec![42_u8; length];
            let response: crate::api::schema::SuccessResponse =
                serde_json::from_str(&app.handle_api_request(m832a_static_request(
                    "pane.graphics.set",
                    &target,
                    serde_json::json!({
                    "format": "png", "image_width": 1, "image_height": 1,
                    "data_base64": base64::engine::general_purpose::STANDARD.encode(&bytes)}),
                )))
                .unwrap();
            assert!(matches!(
                response.result,
                crate::api::schema::ResponseResult::Ok {}
            ));
            assert_eq!(
                app.state.pane_graphics_layers[&pane].data, bytes,
                "length={length}"
            );
        }
        let rgb: crate::api::schema::SuccessResponse = serde_json::from_str(
            &app.handle_api_request(m832a_static_request("pane.graphics.set", &target,
                serde_json::json!({"format":"rgb", "image_width":1, "image_height":1, "data_base64":"AQID"}))),
        ).unwrap();
        assert!(matches!(
            rgb.result,
            crate::api::schema::ResponseResult::Ok {}
        ));
        assert_eq!(app.state.pane_graphics_layers[&pane].data, vec![1, 2, 3]);
        assert_eq!(
            app.state.pane_graphics_layers[&pane].format,
            crate::api::schema::PaneGraphicsFormat::Rgb
        );
        let before = app.state.pane_graphics_layers.clone();
        let revision = app.state.pane_graphics_revision;
        let invalid = [
            (
                "png",
                1_u32,
                1_u32,
                base64::engine::general_purpose::STANDARD.encode(vec![42_u8; cap + 1]),
                "image_too_large",
            ),
            (
                "png",
                1,
                1,
                "!".repeat(cap.div_ceil(3) * 4 + 1),
                "image_too_large",
            ),
            ("png", 1, 1, "!!!!".into(), "invalid_image"),
            ("png", 1, 1, "AQI=".to_owned() + "=", "invalid_image"),
            ("rgba", 0, 1, "AQIDBA==".into(), "invalid_image"),
            ("rgba", 1, 0, "AQIDBA==".into(), "invalid_image"),
            (
                "rgba",
                u32::MAX,
                u32::MAX,
                "AQIDBA==".into(),
                "invalid_image",
            ),
            ("rgba", 2, 1, "AQIDBA==".into(), "invalid_image"),
            ("rgb", 1, 1, "AQI=".into(), "invalid_image"),
            ("rgb", 1, 1, "AQIDBA==".into(), "invalid_image"),
            ("rgb", 2, 1, "AQID".into(), "invalid_image"),
            ("png", 1, 1, String::new(), "invalid_image"),
        ];
        for (index, (format, width, height, data, code)) in invalid.into_iter().enumerate() {
            let response: crate::api::schema::ErrorResponse = serde_json::from_str(&app.handle_api_request(
                m832a_static_request("pane.graphics.set", &target, serde_json::json!({
                    "format": format, "image_width": width, "image_height": height, "data_base64": data})))).unwrap();
            assert_eq!(response.error.code, code, "invalid index={index}");
            assert_eq!(
                app.state.pane_graphics_layers, before,
                "invalid index={index}"
            );
            assert_eq!(
                app.state.pane_graphics_revision, revision,
                "invalid index={index}"
            );
        }
        assert!(!app.state.session_dirty);
    }

    #[test]
    fn m832a_info_uses_available_cell_size_and_preserves_layer() {
        let (mut app, target, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        app.state
            .pane_graphics_layers
            .insert(pane, m832a_static_seed());
        let before = app.state.pane_graphics_layers.clone();
        let revision = app.state.pane_graphics_revision;
        let unknown: crate::api::schema::ErrorResponse =
            serde_json::from_str(&app.handle_api_request(m832a_static_request(
                "pane.graphics.info",
                &target,
                serde_json::json!({}),
            )))
            .unwrap();
        assert_eq!(unknown.error.code, "cell_size_unavailable");
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 11,
            height_px: 22,
        };
        let observed: crate::api::schema::SuccessResponse =
            serde_json::from_str(&app.handle_api_request(m832a_static_request(
                "pane.graphics.info",
                &target,
                serde_json::json!({}),
            )))
            .unwrap();
        assert!(matches!(
            observed.result,
            crate::api::schema::ResponseResult::PaneGraphicsInfo {
                cell_width_px: 11,
                cell_height_px: 22
            }
        ));
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 8,
            height_px: 16,
        };
        let fallback_hint: crate::api::schema::SuccessResponse =
            serde_json::from_str(&app.handle_api_request(m832a_static_request(
                "pane.graphics.info",
                &target,
                serde_json::json!({}),
            )))
            .unwrap();
        assert!(matches!(
            fallback_hint.result,
            crate::api::schema::ResponseResult::PaneGraphicsInfo {
                cell_width_px: 8,
                cell_height_px: 16
            }
        ));
        for method in [
            "pane.graphics.set",
            "pane.graphics.clear",
            "pane.graphics.info",
        ] {
            let extra = if method == "pane.graphics.set" {
                serde_json::json!({"format": "rgba", "image_width": 1, "image_height": 1, "data_base64": "AQIDBA=="})
            } else {
                serde_json::json!({})
            };
            let response: crate::api::schema::ErrorResponse = serde_json::from_str(
                &app.handle_api_request(m832a_static_request(method, "missing-pane", extra)),
            )
            .unwrap();
            assert_eq!(response.error.code, "pane_not_found", "method={method}");
        }
        assert_eq!(app.state.pane_graphics_layers, before);
        assert_eq!(app.state.pane_graphics_revision, revision);
        assert!(!app.state.session_dirty);
    }
}
