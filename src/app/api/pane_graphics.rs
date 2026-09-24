// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use base64::Engine;

use crate::api::schema::{
    PaneGraphicsClearParams, PaneGraphicsSetParams, PaneGraphicsStreamOpenParams,
    PaneGraphicsStreamParams, ResponseResult, PANE_GRAPHICS_DIRECT_FILE_MAX_BYTES,
    PANE_GRAPHICS_MAX_LAYERS_PER_PANE, PANE_GRAPHICS_PRIMARY_LAYER_ID, PANE_GRAPHICS_SET_MAX_BYTES,
    PANE_GRAPHICS_STREAM_MAX_BYTES,
};
use crate::app::pane_graphics::{Key as PaneGraphicsKey, Layer, Slot};
use crate::app::App;
use crate::layout::PaneId;

use super::responses::{encode_error, encode_success};

impl App {
    fn pane_graphics_visible(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        if self.state.active != Some(ws_idx) {
            return false;
        }
        let Some(tab) = self.state.workspaces[ws_idx].active_tab() else {
            return false;
        };
        if tab.zoomed {
            tab.layout.focused() == pane_id
        } else {
            tab.layout.pane_ids().contains(&pane_id)
        }
    }

    pub(super) fn handle_pane_graphics_info(
        &mut self,
        id: String,
        target: crate::api::schema::PaneTarget,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        if !self.state.host_cell_size.is_known() {
            return encode_error(id, "cell_size_unavailable", "host cell size is unavailable");
        }
        let file_frame_directory = self
            .direct_graphics_available
            .then(|| self.pane_graphics_files.source_directory().ok())
            .flatten();
        let direct = file_frame_directory.is_some();
        encode_success(
            id,
            ResponseResult::PaneGraphicsInfo {
                cell_width_px: self.state.host_cell_size.width_px,
                cell_height_px: self.state.host_cell_size.height_px,
                pane_visible: self.pane_graphics_visible(ws_idx, pane_id),
                file_frame_directory: file_frame_directory
                    .map(|directory| directory.to_string_lossy().into_owned()),
                file_frame_formats: if direct {
                    vec!["rgba".into(), "bgra".into()]
                } else {
                    Vec::new()
                },
                file_frame_max_bytes: direct.then_some(PANE_GRAPHICS_STREAM_MAX_BYTES),
                file_frame_direct_max_bytes: direct.then_some(PANE_GRAPHICS_DIRECT_FILE_MAX_BYTES),
                file_frame_damage: true,
                max_layers_per_pane: PANE_GRAPHICS_MAX_LAYERS_PER_PANE,
                pixel_mouse: self.pixel_mouse_available,
                file_frame_transport: direct.then(|| "direct-kitty".into()),
            },
        )
    }

    pub(super) fn handle_pane_graphics_set(
        &mut self,
        id: String,
        params: PaneGraphicsSetParams,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = match graphics_key(pane_id, params.layer_id.as_deref()) {
            Ok(key) => key,
            Err(message) => return encode_error(id, "invalid_layer_id", message),
        };
        self.reclaim_inactive_stream(&key);
        if self
            .pane_graphics
            .slots
            .get(&key)
            .is_some_and(|slot| slot.stream_owner.is_some())
        {
            return encode_error(
                id,
                "stream_conflict",
                "pane graphics layer has an active stream",
            );
        }
        self.set_layer(id, key, params)
    }

    pub(super) fn handle_pane_graphics_clear(
        &mut self,
        id: String,
        params: PaneGraphicsClearParams,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = match graphics_key(pane_id, params.layer_id.as_deref()) {
            Ok(key) => key,
            Err(message) => return encode_error(id, "invalid_layer_id", message),
        };
        self.reclaim_inactive_stream(&key);
        if self
            .pane_graphics
            .slots
            .get(&key)
            .is_some_and(|slot| slot.stream_owner.is_some())
        {
            return encode_error(
                id,
                "stream_conflict",
                "pane graphics layer has an active stream",
            );
        }
        if self.pane_graphics.slots.remove(&key).is_some() {
            self.pane_graphics.mark_changed();
        }
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_stream_open(
        &mut self,
        id: String,
        open: PaneGraphicsStreamOpenParams,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        if !open.active.load(std::sync::atomic::Ordering::Acquire) {
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
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = match graphics_key(pane_id, params.layer_id.as_deref()) {
            Ok(key) => key,
            Err(message) => return encode_error(id, "invalid_layer_id", message),
        };
        self.reclaim_inactive_stream(&key);
        if self
            .pane_graphics
            .slots
            .get(&key)
            .and_then(|slot| slot.stream_owner.as_ref())
            .is_some()
        {
            return encode_error(
                id,
                "stream_conflict",
                "pane graphics layer has an active stream",
            );
        }
        let exists = self.pane_graphics.slots.contains_key(&key);
        if !self.pane_graphics.can_add_slot(&key)
            || (!exists
                && self.pane_graphics.layer_count(pane_id) >= PANE_GRAPHICS_MAX_LAYERS_PER_PANE)
        {
            return encode_error(id, "layer_limit", "pane graphics layer limit reached");
        }
        let Some(host_image_id) = self.pane_graphics.reserve_image_id(&key) else {
            return encode_error(id, "layer_limit", "pane graphics image ids are exhausted");
        };
        self.pane_graphics.slots.insert(
            key,
            Slot {
                host_image_id,
                layer: None,
                stream_owner: Some(params.owner),
                stream_active: Some(open.active),
                direct_gate: None,
            },
        );
        self.pane_graphics.mark_changed();
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_stream_set(
        &mut self,
        id: String,
        params: PaneGraphicsSetParams,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = match graphics_key(pane_id, params.layer_id.as_deref()) {
            Ok(key) => key,
            Err(message) => return encode_error(id, "invalid_layer_id", message),
        };
        match self.pane_graphics.slots.get(&key) {
            Some(slot)
                if slot.stream_owner.as_ref() == Some(&params.owner) && slot.stream_is_active() =>
            {
                self.set_layer(id, key, params)
            }
            Some(slot) if slot.stream_owner.as_ref() == Some(&params.owner) => {
                encode_error(id, "stream_closed", "pane graphics stream is not active")
            }
            Some(_) => encode_error(
                id,
                "stream_conflict",
                "pane graphics stream owner does not match active stream",
            ),
            None => encode_error(id, "stream_closed", "pane graphics stream is not active"),
        }
    }

    pub(super) fn handle_pane_graphics_stream_close(
        &mut self,
        id: String,
        params: PaneGraphicsStreamParams,
    ) -> String {
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if let Ok(key) = graphics_key(pane_id, params.layer_id.as_deref()) {
            if self
                .pane_graphics
                .slots
                .get(&key)
                .and_then(|slot| slot.stream_owner.as_ref())
                .is_some_and(|owner| owner == &params.owner)
            {
                self.pane_graphics.slots.remove(&key);
                self.pane_graphics.mark_changed();
            }
        }
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_graphics_stream_direct(
        &mut self,
        id: String,
        params: crate::api::schema::PaneGraphicsDirectParams,
    ) -> String {
        if let Err(response) = require_enabled(self, &id) {
            return response;
        }
        let Some((_, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = match graphics_key(pane_id, params.layer_id.as_deref()) {
            Ok(key) => key,
            Err(message) => return encode_error(id, "invalid_layer_id", message),
        };
        let owner_matches = self.pane_graphics.slots.get(&key).is_some_and(|slot| {
            slot.stream_owner.as_deref() == Some(params.owner.as_str()) && slot.stream_is_active()
        });
        if !owner_matches {
            return encode_error(id, "stream_closed", "pane graphics stream is not active");
        }
        if params.image_width == 0 || params.image_height == 0 {
            return encode_error(
                id,
                "invalid_image",
                "image_width and image_height must be greater than zero",
            );
        }
        if !matches!(
            params.format,
            crate::api::schema::PaneGraphicsFormat::Rgba
                | crate::api::schema::PaneGraphicsFormat::Bgra
        ) {
            return encode_error(id, "invalid_image", "direct frames require rgba or bgra");
        }
        let direct = self.direct_graphics_available
            && key.1 == PANE_GRAPHICS_PRIMARY_LAYER_ID
            && params.format == crate::api::schema::PaneGraphicsFormat::Rgba;
        let max_bytes = if direct {
            PANE_GRAPHICS_DIRECT_FILE_MAX_BYTES
        } else {
            PANE_GRAPHICS_STREAM_MAX_BYTES
        };
        let expected_len =
            match expected_len(params.format, params.image_width, params.image_height) {
                Ok(Some(len)) if len <= max_bytes => len,
                _ => return encode_error(id, "invalid_image", "invalid direct RGBA dimensions"),
            };
        let lease = match self
            .pane_graphics_files
            .lease(std::path::Path::new(&params.path), expected_len)
        {
            Ok(lease) => lease,
            Err(err) => return encode_error(id, "invalid_frame_file", err.to_string()),
        };
        if !direct && !self.pane_graphics.can_store_inline(&key, expected_len) {
            return encode_error(
                id,
                "graphics_budget_exceeded",
                "pane graphics inline memory limit reached",
            );
        }
        let layer = if direct {
            Layer::direct(
                params.image_width,
                params.image_height,
                lease,
                params.placement,
                params.z_index,
            )
        } else {
            let mut data = match lease.copy_rgba() {
                Ok(data) => data,
                Err(err) => return encode_error(id, "invalid_frame_file", err.to_string()),
            };
            canonicalize_bgra(params.format, &mut data);
            Layer::inline(
                crate::api::schema::PaneGraphicsFormat::Rgba,
                params.image_width,
                params.image_height,
                data,
                params.placement,
                params.z_index,
            )
        };
        let Some(slot) = self.pane_graphics.slots.get_mut(&key) else {
            return encode_error(id, "stream_closed", "pane graphics stream is not active");
        };
        if !slot.stream_is_active() {
            return encode_error(id, "stream_closed", "pane graphics stream is not active");
        }
        slot.layer = Some(layer);
        slot.direct_gate = None;
        self.pane_graphics.mark_changed();
        encode_success(
            id,
            ResponseResult::PaneGraphicsFrameAck {
                sequence: params.sequence,
                revision: params.revision,
            },
        )
    }

    fn set_layer(
        &mut self,
        id: String,
        key: PaneGraphicsKey,
        params: PaneGraphicsSetParams,
    ) -> String {
        if !self.pane_graphics.can_add_slot(&key)
            || (!self.pane_graphics.slots.contains_key(&key)
                && self.pane_graphics.layer_count(key.0) >= PANE_GRAPHICS_MAX_LAYERS_PER_PANE)
        {
            return encode_error(id, "layer_limit", "pane graphics layer limit reached");
        }
        if params.image_width == 0 || params.image_height == 0 {
            return encode_error(
                id,
                "invalid_image",
                "image_width and image_height must be greater than zero",
            );
        }
        let expected = match expected_len(params.format, params.image_width, params.image_height) {
            Ok(expected) => expected,
            Err(()) => return encode_error(id, "invalid_image", "image dimensions are too large"),
        };
        let streamed = params.data.is_some();
        let mut data = match params.data {
            Some(data) => data,
            None => match decode_public_image(&params.data_base64) {
                Ok(data) => data,
                Err((code, message)) => return encode_error(id, code, message),
            },
        };
        let limit = if streamed {
            PANE_GRAPHICS_STREAM_MAX_BYTES
        } else {
            PANE_GRAPHICS_SET_MAX_BYTES
        };
        if data.is_empty() {
            return encode_error(id, "invalid_image", "image data must not be empty");
        }
        if data.len() > limit {
            return encode_error(id, "image_too_large", "image data is too large");
        }
        if expected.is_some_and(|size| size != data.len()) {
            return encode_error(
                id,
                "invalid_image",
                "image data does not match the frame contract",
            );
        }
        let format = canonicalize_bgra(params.format, &mut data);
        if !self.pane_graphics.can_store_inline(&key, data.len()) {
            return encode_error(
                id,
                "graphics_budget_exceeded",
                "pane graphics inline memory limit reached",
            );
        }
        let Some(host_image_id) = self.pane_graphics.reserve_image_id(&key) else {
            return encode_error(id, "layer_limit", "pane graphics image ids are exhausted");
        };
        let (stream_owner, stream_active) = self
            .pane_graphics
            .slots
            .get_mut(&key)
            .map(|slot| (slot.stream_owner.take(), slot.stream_active.take()))
            .unwrap_or_default();
        self.pane_graphics.slots.insert(
            key,
            Slot {
                host_image_id,
                layer: Some(Layer::inline(
                    format,
                    params.image_width,
                    params.image_height,
                    data,
                    params.placement,
                    params.z_index,
                )),
                stream_owner,
                stream_active,
                direct_gate: None,
            },
        );
        self.pane_graphics.mark_changed();
        encode_success(id, ResponseResult::Ok {})
    }

    fn reclaim_inactive_stream(&mut self, key: &PaneGraphicsKey) {
        let stale = self
            .pane_graphics
            .slots
            .get(key)
            .is_some_and(|slot| slot.stream_owner.is_some() && !slot.stream_is_active());
        if stale {
            self.pane_graphics.slots.remove(key);
            self.pane_graphics.mark_changed();
        }
    }

    pub(crate) fn sync_pane_graphics_streams(&mut self) -> bool {
        let mut changed = self.pane_graphics.retain_live_panes(&self.state);
        let before = self.pane_graphics.slots.len();
        let enabled = self.state.kitty_graphics_enabled;
        self.pane_graphics
            .slots
            .retain(|_, slot| enabled && (slot.stream_owner.is_none() || slot.stream_is_active()));
        if self.pane_graphics.slots.len() != before {
            self.pane_graphics.mark_changed();
            changed = true;
        }
        if changed {
            self.render_dirty.request_generic();
        }
        changed
    }
}

fn canonicalize_bgra(
    format: crate::api::schema::PaneGraphicsFormat,
    data: &mut [u8],
) -> crate::api::schema::PaneGraphicsFormat {
    if format != crate::api::schema::PaneGraphicsFormat::Bgra {
        return format;
    }
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    crate::api::schema::PaneGraphicsFormat::Rgba
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

fn graphics_key(pane_id: PaneId, layer_id: Option<&str>) -> Result<PaneGraphicsKey, &'static str> {
    let layer_id = layer_id.unwrap_or(PANE_GRAPHICS_PRIMARY_LAYER_ID);
    if layer_id.is_empty() || layer_id.len() > 64 {
        return Err("layer_id must contain between 1 and 64 characters");
    }
    if !layer_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err("layer_id contains unsupported characters");
    }
    Ok((pane_id, layer_id.to_owned()))
}

fn expected_len(
    format: crate::api::schema::PaneGraphicsFormat,
    width: u32,
    height: u32,
) -> Result<Option<usize>, ()> {
    let bytes = match format {
        crate::api::schema::PaneGraphicsFormat::Png => return Ok(None),
        crate::api::schema::PaneGraphicsFormat::Rgb => 3_u64,
        crate::api::schema::PaneGraphicsFormat::Rgba
        | crate::api::schema::PaneGraphicsFormat::Bgra => 4_u64,
    };
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(bytes))
        .and_then(|size| usize::try_from(size).ok())
        .map(Some)
        .ok_or(())
}

fn require_enabled(app: &App, id: &str) -> Result<(), String> {
    app.state
        .kitty_graphics_enabled
        .then_some(())
        .ok_or_else(|| {
            encode_error(
                id.to_owned(),
                "feature_disabled",
                "pane graphics require experimental.kitty_graphics",
            )
        })
}

fn pane_not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{ErrorResponse, SuccessResponse};
    use crate::app::App;

    fn endstate_frame(
        pane_id: String,
        layer_id: Option<&str>,
        owner: &str,
        value: u8,
    ) -> PaneGraphicsSetParams {
        PaneGraphicsSetParams {
            pane_id,
            layer_id: layer_id.map(str::to_owned),
            z_index: 2,
            owner: owner.to_owned(),
            format: crate::api::schema::PaneGraphicsFormat::Rgba,
            image_width: 1,
            image_height: 1,
            data: Some(vec![value; 4]),
            data_base64: String::new(),
            placement: Default::default(),
        }
    }

    fn endstate_open(app: &mut App, id: &str, params: PaneGraphicsStreamParams) -> String {
        app.handle_pane_graphics_stream_open(
            id.to_owned(),
            PaneGraphicsStreamOpenParams {
                params,
                active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            },
        )
    }

    fn endstate_error_code(response: &str) -> String {
        serde_json::from_str::<ErrorResponse>(response)
            .unwrap()
            .error
            .code
    }

    fn endstate_pane_visible(response: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(response).unwrap()["result"]["pane_visible"]
            .as_bool()
            .unwrap()
    }

    #[test]
    fn info_reports_visibility_for_terminal_surface_workspace_tab_and_zoom() {
        let (mut app, pane_id, _) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        app.state.mode = crate::app::Mode::Terminal;
        app.state.active = Some(0);
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        assert!(endstate_pane_visible(&app.handle_pane_graphics_info(
            "visible".into(),
            crate::api::schema::PaneTarget {
                pane_id: pane_id.clone(),
            },
        )));

        let hidden_workspace = crate::workspace::Workspace::test_new("hidden");
        let hidden_workspace_pane = hidden_workspace.tabs[0].root_pane;
        app.state.workspaces.push(hidden_workspace);
        let hidden_workspace_id = app.public_pane_id(1, hidden_workspace_pane).unwrap();
        assert!(!endstate_pane_visible(&app.handle_pane_graphics_info(
            "hidden-workspace".into(),
            crate::api::schema::PaneTarget {
                pane_id: hidden_workspace_id,
            },
        )));

        let inactive_tab = app.state.workspaces[0].test_add_tab(Some("inactive"));
        let inactive_pane = app.state.workspaces[0].tabs[inactive_tab].root_pane;
        let inactive_id = app.public_pane_id(0, inactive_pane).unwrap();
        assert!(!endstate_pane_visible(&app.handle_pane_graphics_info(
            "hidden-tab".into(),
            crate::api::schema::PaneTarget {
                pane_id: inactive_id,
            },
        )));

        app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].zoomed = true;
        assert!(!endstate_pane_visible(&app.handle_pane_graphics_info(
            "zoomed-away".into(),
            crate::api::schema::PaneTarget {
                pane_id: pane_id.clone(),
            },
        )));
        app.state.workspaces[0].tabs[0].zoomed = false;
        app.state.mode = crate::app::Mode::Navigate;
        assert!(endstate_pane_visible(&app.handle_pane_graphics_info(
            "navigate".into(),
            crate::api::schema::PaneTarget { pane_id },
        )));
    }

    #[test]
    fn monolithic_info_keeps_fast_transport_and_exact_pixels_disabled() {
        let (mut app, pane_id, _) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        assert!(!app.pane_graphics_files.is_initialized());
        let response = app
            .handle_pane_graphics_info("info".into(), crate::api::schema::PaneTarget { pane_id });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::PaneGraphicsInfo {
                file_frame_damage: true,
                max_layers_per_pane: 16,
                pixel_mouse: false,
                ..
            }
        ));
        assert!(app.pane_graphics.slots.is_empty());
        assert!(!app.pane_graphics_files.is_initialized());
    }

    #[test]
    fn info_advertises_rgba_direct_and_bgra_fallback_file_formats_on_demand() {
        let (mut app, pane_id, _) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        app.direct_graphics_available = true;
        let response = app
            .handle_pane_graphics_info("info".into(), crate::api::schema::PaneTarget { pane_id });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(app.pane_graphics_files.is_initialized());
        assert_eq!(
            value["result"]["file_frame_formats"],
            serde_json::json!(["rgba", "bgra"])
        );
        assert_eq!(
            value["result"]["file_frame_max_bytes"],
            PANE_GRAPHICS_STREAM_MAX_BYTES
        );
        assert_eq!(
            value["result"]["file_frame_direct_max_bytes"],
            PANE_GRAPHICS_DIRECT_FILE_MAX_BYTES
        );
        assert_eq!(value["result"]["file_frame_transport"], "direct-kitty");
    }

    #[test]
    fn layers_are_isolated_and_stream_owned() {
        let (mut app, pane_id, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        for (layer, z, value) in [(None, 0, 1), (Some("chrome"), 10, 2)] {
            let mut request = endstate_frame(pane_id.clone(), layer, "", value);
            request.z_index = z;
            let response = app.handle_pane_graphics_set(format!("set-{value}"), request);
            assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        }
        assert_eq!(
            app.pane_graphics.slots[&(pane, PANE_GRAPHICS_PRIMARY_LAYER_ID.into())]
                .layer
                .as_ref()
                .unwrap()
                .z_index,
            0
        );
        assert_eq!(
            app.pane_graphics.slots[&(pane, "chrome".into())]
                .layer
                .as_ref()
                .unwrap()
                .z_index,
            10
        );
        let response = endstate_open(
            &mut app,
            "open",
            PaneGraphicsStreamParams {
                pane_id: pane_id.clone(),
                layer_id: Some("chrome".into()),
                z_index: 10,
                owner: "stream".into(),
            },
        );
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert_eq!(
            endstate_error_code(&app.handle_pane_graphics_set(
                "conflict".into(),
                endstate_frame(pane_id, Some("chrome"), "", 3),
            )),
            "stream_conflict"
        );
    }

    #[test]
    fn validates_layer_and_frame_contracts() {
        let cases = [
            (Some("bad space"), 1, 1, vec![1; 4], "invalid_layer_id"),
            (None, 0, 1, vec![1; 4], "invalid_image"),
            (None, 1, 1, Vec::new(), "invalid_image"),
            (None, 1, 1, vec![1; 3], "invalid_image"),
        ];
        for (layer, width, height, data, expected) in cases {
            let (mut app, pane_id, _) = m832a_static_app();
            app.state.kitty_graphics_enabled = true;
            let mut params = endstate_frame(pane_id, layer, "", 1);
            params.image_width = width;
            params.image_height = height;
            params.data = Some(data);
            let response = app.handle_pane_graphics_set("invalid".into(), params);
            assert_eq!(endstate_error_code(&response), expected);
            assert!(app.pane_graphics.slots.is_empty());
        }
    }

    #[test]
    fn disabled_requests_have_no_graphics_side_effects() {
        let (mut app, pane_id, _) = m832a_static_app();
        let revision = app.pane_graphics.revision();
        let response =
            app.handle_pane_graphics_set("disabled".into(), endstate_frame(pane_id, None, "", 1));
        assert_eq!(endstate_error_code(&response), "feature_disabled");
        assert!(app.pane_graphics.slots.is_empty());
        assert_eq!(app.pane_graphics.revision(), revision);
    }

    #[test]
    fn stream_owner_controls_only_its_named_layer() {
        let (mut app, pane_id, _) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        let params = |owner: &str| PaneGraphicsStreamParams {
            pane_id: pane_id.clone(),
            layer_id: Some("chrome".into()),
            z_index: 7,
            owner: owner.into(),
        };
        assert!(serde_json::from_str::<SuccessResponse>(&endstate_open(
            &mut app,
            "open",
            params("owner"),
        ))
        .is_ok());
        assert_eq!(
            endstate_error_code(&app.handle_pane_graphics_stream_set(
                "wrong".into(),
                endstate_frame(pane_id.clone(), Some("chrome"), "other", 2),
            )),
            "stream_conflict"
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_pane_graphics_stream_set(
                "frame".into(),
                endstate_frame(pane_id.clone(), Some("chrome"), "owner", 3),
            ))
            .is_ok()
        );
        app.handle_pane_graphics_stream_close("stale".into(), params("other"));
        assert_eq!(app.pane_graphics.slots.len(), 1);
        app.handle_pane_graphics_stream_close("close".into(), params("owner"));
        assert!(app.pane_graphics.slots.is_empty());
    }

    #[test]
    fn static_set_and_clear_reclaim_inactive_stream_layers() {
        let (mut app, pane_id, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        let params = |layer: &str| PaneGraphicsStreamParams {
            pane_id: pane_id.clone(),
            layer_id: Some(layer.into()),
            z_index: 0,
            owner: format!("owner-{layer}"),
        };
        for layer in ["set", "clear"] {
            assert!(serde_json::from_str::<SuccessResponse>(&endstate_open(
                &mut app,
                layer,
                params(layer),
            ))
            .is_ok());
            app.pane_graphics.slots[&(pane, layer.into())]
                .stream_active
                .as_ref()
                .unwrap()
                .store(false, std::sync::atomic::Ordering::Release);
        }
        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_pane_graphics_set(
                "set-static".into(),
                endstate_frame(pane_id.clone(), Some("set"), "", 4),
            ))
            .is_ok()
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_pane_graphics_clear(
                "clear-static".into(),
                PaneGraphicsClearParams {
                    pane_id,
                    layer_id: Some("clear".into()),
                },
            ))
            .is_ok()
        );
        assert_eq!(app.pane_graphics.slots.len(), 1);
        assert!(app
            .pane_graphics
            .slots
            .values()
            .all(|slot| slot.stream_owner.is_none()));
    }

    #[test]
    fn layer_limit_counts_empty_and_populated_stream_slots() {
        let (mut app, pane_id, _) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        for index in 0..PANE_GRAPHICS_MAX_LAYERS_PER_PANE {
            let layer_id = format!("layer-{index}");
            let response = endstate_open(
                &mut app,
                &layer_id,
                PaneGraphicsStreamParams {
                    pane_id: pane_id.clone(),
                    layer_id: Some(layer_id.clone()),
                    z_index: index as i32,
                    owner: format!("owner-{index}"),
                },
            );
            assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        }
        assert_eq!(
            endstate_error_code(&app.handle_pane_graphics_set(
                "set-overflow".into(),
                endstate_frame(pane_id.clone(), Some("set-overflow"), "", 1),
            )),
            "layer_limit"
        );
        assert_eq!(
            endstate_error_code(&endstate_open(
                &mut app,
                "overflow",
                PaneGraphicsStreamParams {
                    pane_id,
                    layer_id: Some("overflow".into()),
                    z_index: 0,
                    owner: "overflow".into(),
                },
            )),
            "layer_limit"
        );
        assert_eq!(app.pane_graphics.slots.len(), 16);
    }

    #[test]
    fn set_and_clear_preserve_runtime_only_layer_details() {
        let (mut app, pane_id, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        let mut params = endstate_frame(pane_id.clone(), None, "", 9);
        params.placement.grid_cols = 10;
        params.placement.grid_rows = 4;
        assert!(serde_json::from_str::<SuccessResponse>(
            &app.handle_pane_graphics_set("set".into(), params)
        )
        .is_ok());
        let layer = app.pane_graphics.slots[&(pane, PANE_GRAPHICS_PRIMARY_LAYER_ID.into())]
            .layer
            .as_ref()
            .unwrap();
        assert_eq!(layer.inline_data(), Some([9; 4].as_slice()));
        assert_eq!((layer.render.grid_cols, layer.render.grid_rows), (10, 4));
        assert!(!app.state.session_dirty);
        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_pane_graphics_clear(
                "clear".into(),
                PaneGraphicsClearParams {
                    pane_id,
                    layer_id: None,
                },
            ))
            .is_ok()
        );
        assert!(app.pane_graphics.slots.is_empty());
    }

    #[test]
    fn raw_formats_require_exact_non_overflowing_lengths() {
        for (format, exact) in [
            (crate::api::schema::PaneGraphicsFormat::Rgb, 6),
            (crate::api::schema::PaneGraphicsFormat::Rgba, 8),
        ] {
            for (length, expected) in [(exact, "ok"), (exact - 1, "invalid_image")] {
                let (mut app, pane_id, _) = m832a_static_app();
                app.state.kitty_graphics_enabled = true;
                let mut params = endstate_frame(pane_id, None, "", 1);
                params.format = format;
                params.image_width = 2;
                params.data = Some(vec![1; length]);
                let response = app.handle_pane_graphics_set("length".into(), params);
                if expected == "ok" {
                    assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
                } else {
                    assert_eq!(endstate_error_code(&response), expected);
                }
            }
            let (mut app, pane_id, _) = m832a_static_app();
            app.state.kitty_graphics_enabled = true;
            let mut params = endstate_frame(pane_id, None, "", 1);
            params.format = format;
            params.image_width = u32::MAX;
            params.image_height = u32::MAX;
            assert_eq!(
                endstate_error_code(&app.handle_pane_graphics_set("overflow".into(), params)),
                "invalid_image"
            );
        }
    }

    #[test]
    fn public_frames_validate_base64_and_public_size_limit() {
        use base64::Engine as _;
        let (mut app, pane_id, _) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        let mut params = endstate_frame(pane_id.clone(), None, "", 1);
        params.format = crate::api::schema::PaneGraphicsFormat::Png;
        params.data = None;
        params.data_base64 = "not base64".into();
        assert_eq!(
            endstate_error_code(&app.handle_pane_graphics_set("base64".into(), params)),
            "invalid_image"
        );
        let mut params = endstate_frame(pane_id, None, "", 1);
        params.format = crate::api::schema::PaneGraphicsFormat::Png;
        params.data = None;
        params.data_base64 =
            base64::engine::general_purpose::STANDARD
                .encode(vec![1; PANE_GRAPHICS_SET_MAX_BYTES + 1]);
        assert_eq!(
            endstate_error_code(&app.handle_pane_graphics_set("large".into(), params)),
            "image_too_large"
        );
    }

    #[test]
    fn aliases_resolve_to_the_same_layer_identity() {
        let (mut app, pane_id, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        let alias = format!("{pane_id}:alias");
        app.state.public_pane_id_aliases.insert(alias.clone(), pane);
        let params = |target: String, owner: &str| PaneGraphicsStreamParams {
            pane_id: target,
            layer_id: None,
            z_index: 0,
            owner: owner.into(),
        };
        assert!(serde_json::from_str::<SuccessResponse>(&endstate_open(
            &mut app,
            "open",
            params(pane_id, "owner"),
        ))
        .is_ok());
        assert_eq!(
            endstate_error_code(&endstate_open(
                &mut app,
                "conflict",
                params(alias.clone(), "other"),
            )),
            "stream_conflict"
        );
        app.handle_pane_graphics_stream_close("stale".into(), params(alias.clone(), "other"));
        assert_eq!(app.pane_graphics.slots.len(), 1);
        app.handle_pane_graphics_stream_close("close".into(), params(alias, "owner"));
        assert!(app.pane_graphics.slots.is_empty());
    }

    fn endstate_direct_file(app: &App, name: &str, data: &[u8]) -> String {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = app
            .pane_graphics_files
            .source_directory()
            .unwrap()
            .join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(data).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn endstate_sparse_direct_file(app: &App, name: &str, len: usize) -> String {
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = app
            .pane_graphics_files
            .source_directory()
            .unwrap()
            .join(name);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.set_len(len as u64).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn endstate_direct_params(
        pane_id: String,
        owner: &str,
        path: String,
    ) -> crate::api::schema::PaneGraphicsDirectParams {
        crate::api::schema::PaneGraphicsDirectParams {
            pane_id,
            layer_id: None,
            z_index: 0,
            owner: owner.into(),
            image_width: 1,
            image_height: 1,
            format: crate::api::schema::PaneGraphicsFormat::Rgba,
            path,
            sequence: 1,
            revision: 1,
            placement: Default::default(),
        }
    }

    #[test]
    fn direct_file_is_leased_or_safely_copied_for_inline_fallback() {
        let (mut app, pane_id, _) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        let stream = PaneGraphicsStreamParams {
            pane_id: pane_id.clone(),
            layer_id: None,
            z_index: 0,
            owner: "owner".into(),
        };
        endstate_open(&mut app, "open", stream.clone());
        for (width, height) in [(0, 1), (1, 0)] {
            let mut params = endstate_direct_params(pane_id.clone(), "owner", "unused".into());
            (params.image_width, params.image_height) = (width, height);
            assert_eq!(
                endstate_error_code(
                    &app.handle_pane_graphics_stream_direct("invalid".into(), params)
                ),
                "invalid_image"
            );
        }
        for format in [
            crate::api::schema::PaneGraphicsFormat::Rgb,
            crate::api::schema::PaneGraphicsFormat::Png,
        ] {
            let mut params = endstate_direct_params(pane_id.clone(), "owner", "unused".into());
            params.format = format;
            assert_eq!(
                endstate_error_code(
                    &app.handle_pane_graphics_stream_direct("invalid".into(), params)
                ),
                "invalid_image"
            );
        }
        app.direct_graphics_available = true;
        let path = endstate_direct_file(&app, "direct", &[1, 2, 3, 4]);
        let response = app.handle_pane_graphics_stream_direct(
            "direct".into(),
            crate::api::schema::PaneGraphicsDirectParams {
                sequence: 7,
                revision: 8,
                ..endstate_direct_params(pane_id.clone(), "owner", path)
            },
        );
        let ack: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            ack.result,
            ResponseResult::PaneGraphicsFrameAck {
                sequence: 7,
                revision: 8
            }
        ));
        assert!(app
            .pane_graphics
            .slots
            .values()
            .next()
            .unwrap()
            .layer
            .as_ref()
            .unwrap()
            .direct_lease()
            .is_some());

        app.handle_pane_graphics_stream_close("close".into(), stream.clone());
        endstate_open(&mut app, "reopen", stream);
        app.direct_graphics_available = false;
        let path = endstate_direct_file(&app, "fallback", &[5, 6, 7, 8]);
        app.handle_pane_graphics_stream_direct(
            "fallback".into(),
            crate::api::schema::PaneGraphicsDirectParams {
                sequence: 9,
                revision: 10,
                ..endstate_direct_params(pane_id, "owner", path)
            },
        );
        assert_eq!(
            app.pane_graphics
                .slots
                .values()
                .next()
                .unwrap()
                .layer
                .as_ref()
                .unwrap()
                .inline_data(),
            Some([5, 6, 7, 8].as_slice())
        );
    }

    #[test]
    fn direct_primary_rgba_accepts_fullscreen_retina_frame() {
        let (mut app, pane_id, _) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        app.direct_graphics_available = true;
        endstate_open(
            &mut app,
            "open",
            PaneGraphicsStreamParams {
                pane_id: pane_id.clone(),
                layer_id: None,
                z_index: 0,
                owner: "owner".into(),
            },
        );
        let (image_width, image_height) = (3456, 2234);
        let len = image_width * image_height * 4;
        let path = endstate_sparse_direct_file(&app, "retina-frame", len as usize);
        let response = app.handle_pane_graphics_stream_direct(
            "frame".into(),
            crate::api::schema::PaneGraphicsDirectParams {
                image_width,
                image_height,
                ..endstate_direct_params(pane_id, "owner", path)
            },
        );
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert!(app
            .pane_graphics
            .slots
            .values()
            .next()
            .unwrap()
            .layer
            .as_ref()
            .unwrap()
            .direct_lease()
            .is_some());
    }

    #[test]
    fn bgra_and_secondary_file_frames_are_canonical_owned_rgba() {
        let (mut app, pane_id, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        app.direct_graphics_available = true;
        for (layer, format, source) in [
            (
                "browser-chrome",
                crate::api::schema::PaneGraphicsFormat::Rgba,
                [1, 2, 3, 4],
            ),
            (
                "primary",
                crate::api::schema::PaneGraphicsFormat::Bgra,
                [3, 2, 1, 4],
            ),
        ] {
            endstate_open(
                &mut app,
                &format!("open-{layer}"),
                PaneGraphicsStreamParams {
                    pane_id: pane_id.clone(),
                    layer_id: Some(layer.into()),
                    z_index: 0,
                    owner: layer.into(),
                },
            );
            let path = endstate_direct_file(&app, layer, &source);
            app.handle_pane_graphics_stream_direct(
                format!("frame-{layer}"),
                crate::api::schema::PaneGraphicsDirectParams {
                    layer_id: Some(layer.into()),
                    format,
                    ..endstate_direct_params(pane_id.clone(), layer, path)
                },
            );
            assert_eq!(
                app.pane_graphics.slots[&(pane, layer.into())]
                    .layer
                    .as_ref()
                    .unwrap()
                    .inline_data(),
                Some([1, 2, 3, 4].as_slice())
            );
        }
    }

    #[test]
    fn disabled_info_and_stream_requests_are_rejected() {
        let (mut app, pane_id, _) = m832a_static_app();
        assert_eq!(
            endstate_error_code(&app.handle_pane_graphics_info(
                "info".into(),
                crate::api::schema::PaneTarget {
                    pane_id: pane_id.clone(),
                },
            )),
            "feature_disabled"
        );
        assert_eq!(
            endstate_error_code(&endstate_open(
                &mut app,
                "open",
                PaneGraphicsStreamParams {
                    pane_id,
                    layer_id: None,
                    z_index: 0,
                    owner: "owner".into(),
                },
            )),
            "feature_disabled"
        );
        assert!(app.pane_graphics.slots.is_empty());
    }

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
                    layer_id: None,
                    z_index: 0,
                    owner: owner.into(),
                },
                active,
            }),
        };
        let response = app.handle_api_request(open("owner", active.clone()));
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert_eq!(
            m832a_primary_slot(&app, pane).stream_owner.as_deref(),
            Some("owner")
        );
        m832a_seed_layer(&mut app, pane);
        let before = m832a_graphics_snapshot(&app);
        let revision = app.pane_graphics.revision();
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
            assert_eq!(m832a_graphics_snapshot(&app), before);
            assert_eq!(app.pane_graphics.revision(), revision);
        }
        let response: ErrorResponse = serde_json::from_str(
            &app.handle_api_request(open("second", Arc::new(AtomicBool::new(true)))),
        )
        .unwrap();
        assert_eq!(response.error.code, "stream_conflict");
        assert_eq!(m832a_graphics_snapshot(&app), before);
        app.state.workspaces.swap(0, 1);
        m832a_seed_layer(&mut app, other);
        let frame = |pane_id: String, owner: &str| Request {
            id: "frame".into(),
            method: Method::PaneGraphicsStreamSet(PaneGraphicsSetParams {
                pane_id,
                layer_id: None,
                z_index: 0,
                owner: owner.into(),
                format: PaneGraphicsFormat::Rgba,
                image_width: 1,
                image_height: 1,
                data: Some(vec![5, 6, 7, 8]),
                data_base64: String::new(),
                placement: Default::default(),
            }),
        };
        let response: ErrorResponse =
            serde_json::from_str(&app.handle_api_request(frame("1:p1".into(), "owner"))).unwrap();
        assert_eq!(response.error.code, "stream_conflict");
        assert_eq!(m832a_layer_data(&app, pane), &[1, 2, 3, 4]);
        assert_eq!(m832a_layer_data(&app, other), &[1, 2, 3, 4]);
        let pane_target = app.public_pane_id(1, pane).unwrap();
        let response = app.handle_api_request(frame(pane_target.clone(), "owner"));
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert_eq!(m832a_layer_data(&app, pane), &[5, 6, 7, 8]);
        assert_eq!(m832a_layer_data(&app, other), &[1, 2, 3, 4]);
        let close = |pane_id: String, owner: &str| Request {
            id: "close".into(),
            method: Method::PaneGraphicsStreamClose(PaneGraphicsStreamParams {
                pane_id,
                layer_id: None,
                z_index: 0,
                owner: owner.into(),
            }),
        };
        let response: ErrorResponse =
            serde_json::from_str(&app.handle_api_request(close("missing".into(), "owner")))
                .unwrap();
        assert_eq!(response.error.code, "pane_not_found");
        assert!(active.load(Ordering::Acquire));
        let response = app.handle_api_request(close(pane_target.clone(), "owner"));
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert!(!active.load(Ordering::Acquire));
        assert!(!app
            .pane_graphics
            .slots
            .contains_key(&m832a_primary_key(pane)));
        assert_eq!(m832a_layer_data(&app, other), &[1, 2, 3, 4]);
        let successor = Arc::new(AtomicBool::new(true));
        let response = app.handle_api_request(Request {
            id: "successor".into(),
            method: Method::PaneGraphicsStreamOpen(PaneGraphicsStreamOpenParams {
                params: PaneGraphicsStreamParams {
                    pane_id: pane_target.clone(),
                    layer_id: None,
                    z_index: 0,
                    owner: "successor".into(),
                },
                active: successor.clone(),
            }),
        });
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        m832a_seed_layer(&mut app, pane);
        let response = app.handle_api_request(close(pane_target, "owner"));
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        assert_eq!(
            m832a_primary_slot(&app, pane).stream_owner.as_deref(),
            Some("successor")
        );
        assert!(successor.load(Ordering::Acquire));
        assert_eq!(m832a_layer_data(&app, pane), &[1, 2, 3, 4]);
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
                    layer_id: None,
                    z_index: 0,
                    owner: "disabled".into(),
                },
                active: active.clone(),
            }),
        };
        let frame = |target: &str| Request {
            id: "frame".into(),
            method: Method::PaneGraphicsStreamSet(PaneGraphicsSetParams {
                pane_id: target.into(),
                layer_id: None,
                z_index: 0,
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
            assert!(app.pane_graphics.slots.is_empty());
        }
        app.state.kitty_graphics_enabled = true;
        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_api_request(open(&target))).is_ok()
        );
        m832a_seed_layer(&mut app, pane);
        app.state.kitty_graphics_enabled = false;
        let before = m832a_graphics_snapshot(&app);
        let response: ErrorResponse =
            serde_json::from_str(&app.handle_api_request(frame(&target))).unwrap();
        assert_eq!(response.error.code, "feature_disabled");
        assert_eq!(m832a_graphics_snapshot(&app), before);
        let close = Request {
            id: "close".into(),
            method: Method::PaneGraphicsStreamClose(PaneGraphicsStreamParams {
                pane_id: target,
                layer_id: None,
                z_index: 0,
                owner: "disabled".into(),
            }),
        };
        assert!(serde_json::from_str::<SuccessResponse>(&app.handle_api_request(close)).is_ok());
        assert!(!active.load(Ordering::Acquire));
        assert!(app.pane_graphics.slots.is_empty());
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
                    layer_id: None,
                    z_index: 0,
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
                layer_id: None,
                z_index: 0,
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
            m832a_layer_data(&app, pane).len(),
            PANE_GRAPHICS_STREAM_MAX_BYTES
        );
        let before = m832a_graphics_snapshot(&app);
        let revision = app.pane_graphics.revision();
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
            assert_eq!(m832a_graphics_snapshot(&app), before);
            assert_eq!(app.pane_graphics.revision(), revision);
            assert_eq!(
                m832a_primary_slot(&app, pane).stream_owner.as_deref(),
                Some("cap")
            );
        }
    }
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

    fn m832a_primary_key(pane: crate::layout::PaneId) -> crate::app::pane_graphics::Key {
        (
            pane,
            crate::api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID.to_owned(),
        )
    }

    fn m832a_static_seed() -> crate::app::pane_graphics::Layer {
        crate::app::pane_graphics::Layer::inline(
            crate::api::schema::PaneGraphicsFormat::Rgba,
            1,
            1,
            vec![1, 2, 3, 4],
            crate::api::schema::PaneGraphicsPlacementParams::default(),
            0,
        )
    }

    fn m832a_seed_layer(app: &mut App, pane: crate::layout::PaneId) {
        let key = m832a_primary_key(pane);
        let host_image_id = app.pane_graphics.reserve_image_id(&key).unwrap();
        if let Some(slot) = app.pane_graphics.slots.get_mut(&key) {
            slot.layer = Some(m832a_static_seed());
        } else {
            app.pane_graphics.slots.insert(
                key,
                crate::app::pane_graphics::Slot::test(host_image_id, Some(m832a_static_seed())),
            );
        }
    }

    fn m832a_primary_slot(
        app: &App,
        pane: crate::layout::PaneId,
    ) -> &crate::app::pane_graphics::Slot {
        &app.pane_graphics.slots[&m832a_primary_key(pane)]
    }

    fn m832a_primary_layer(
        app: &App,
        pane: crate::layout::PaneId,
    ) -> &crate::app::pane_graphics::Layer {
        m832a_primary_slot(app, pane).layer.as_ref().unwrap()
    }

    fn m832a_layer_data(app: &App, pane: crate::layout::PaneId) -> &[u8] {
        m832a_primary_layer(app, pane).inline_data().unwrap()
    }

    type M832aGraphicsSnapshotRow = (
        u32,
        String,
        crate::api::schema::PaneGraphicsFormat,
        u32,
        u32,
        Vec<u8>,
        crate::api::schema::PaneGraphicsPlacementParams,
        i32,
        Option<String>,
    );

    fn m832a_graphics_snapshot(app: &App) -> Vec<M832aGraphicsSnapshotRow> {
        let mut entries = app.pane_graphics.slots.iter().collect::<Vec<_>>();
        entries.sort_by_key(|((pane, layer), _)| (pane.raw(), layer.as_str()));
        entries
            .into_iter()
            .map(|((pane, layer_id), slot)| {
                let layer = slot.layer.as_ref().unwrap();
                (
                    pane.raw(),
                    layer_id.clone(),
                    layer.format,
                    layer.image_width,
                    layer.image_height,
                    layer.inline_data().unwrap().to_vec(),
                    layer.render,
                    layer.z_index,
                    slot.stream_owner.clone(),
                )
            })
            .collect()
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
            m832a_seed_layer(&mut app, pane);
            app.pane_graphics.test_set_revision(17);
            let before = m832a_graphics_snapshot(&app);
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
                    m832a_graphics_snapshot(&app),
                    before,
                    "method={method} case={case}"
                );
                assert_eq!(app.pane_graphics.revision(), 17);
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
        app.pane_graphics.test_set_revision(u64::MAX);
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
            assert_eq!(m832a_layer_data(&app, pane), expected);
            assert_eq!(m832a_primary_layer(&app, pane).image_width, 1);
            assert_eq!(m832a_primary_layer(&app, pane).image_height, 1);
            assert_eq!(m832a_primary_layer(&app, pane).render.viewport_col, -1);
            assert_eq!(m832a_primary_layer(&app, pane).render.grid_rows, 4);
            assert_eq!(m832a_primary_layer(&app, pane).render.grid_cols, 3);
            assert_eq!(app.pane_graphics.revision(), revision);
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
            assert!(!app
                .pane_graphics
                .slots
                .contains_key(&m832a_primary_key(pane)));
            assert_eq!(app.pane_graphics.revision(), 2);
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
            assert_eq!(m832a_layer_data(&app, pane), bytes, "length={length}");
        }
        let rgb: crate::api::schema::SuccessResponse = serde_json::from_str(
            &app.handle_api_request(m832a_static_request("pane.graphics.set", &target,
                serde_json::json!({"format":"rgb", "image_width":1, "image_height":1, "data_base64":"AQID"}))),
        ).unwrap();
        assert!(matches!(
            rgb.result,
            crate::api::schema::ResponseResult::Ok {}
        ));
        assert_eq!(m832a_layer_data(&app, pane), &[1, 2, 3]);
        assert_eq!(
            m832a_primary_layer(&app, pane).format,
            crate::api::schema::PaneGraphicsFormat::Rgb
        );
        let before = m832a_graphics_snapshot(&app);
        let revision = app.pane_graphics.revision();
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
                m832a_graphics_snapshot(&app),
                before,
                "invalid index={index}"
            );
            assert_eq!(
                app.pane_graphics.revision(),
                revision,
                "invalid index={index}"
            );
        }
        assert!(!app.state.session_dirty);
    }

    #[test]
    fn m832a_info_uses_available_cell_size_and_preserves_layer() {
        let (mut app, target, pane) = m832a_static_app();
        app.state.kitty_graphics_enabled = true;
        m832a_seed_layer(&mut app, pane);
        let before = m832a_graphics_snapshot(&app);
        let revision = app.pane_graphics.revision();
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
                cell_height_px: 22,
                ..
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
                cell_height_px: 16,
                ..
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
        assert_eq!(m832a_graphics_snapshot(&app), before);
        assert_eq!(app.pane_graphics.revision(), revision);
        assert!(!app.state.session_dirty);
    }
}
