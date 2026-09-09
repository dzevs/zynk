use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as FmtWrite;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use ratatui::layout::Rect;

use crate::app::state::AppState;
use crate::app::Mode;
use crate::ghostty::{KittyImageDescriptor, KittyImageFormat, KittyImagePlacement};
use crate::layout::PaneId;
use crate::terminal::TerminalRuntimeRegistry;

const KITTY_CHUNK_BYTES: usize = 3072;
const HOST_IMAGE_ID_BASE: u32 = 10_000;
const HOST_IMAGE_ID_SPAN: u32 = 900_000;
const HOST_PLACEMENT_ID_BASE: u32 = 1;
const HOST_PLACEMENT_ID_SPAN: u32 = 900_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HostCellSize {
    pub width_px: u32,
    pub height_px: u32,
}

impl HostCellSize {
    pub(crate) fn from_terminal(area: Rect) -> Self {
        let Ok(size) = crossterm::terminal::window_size() else {
            return Self::fallback_for_area(area);
        };
        if size.columns == 0 || size.rows == 0 {
            return Self::fallback_for_area(area);
        }
        if size.width == 0 || size.height == 0 {
            return Self::fallback_for_area(area);
        }
        Self {
            width_px: (size.width as u32 / size.columns as u32).max(1),
            height_px: (size.height as u32 / size.rows as u32).max(1),
        }
        .for_area(area)
    }

    pub(crate) fn is_known(self) -> bool {
        self.width_px > 0 && self.height_px > 0
    }

    fn fallback_for_area(area: Rect) -> Self {
        Self {
            width_px: 8,
            height_px: 16,
        }
        .for_area(area)
    }

    fn for_area(self, area: Rect) -> Self {
        if area.width == 0 || area.height == 0 {
            return Self::default();
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostViewKey {
    workspace_index: usize,
    tab_index: usize,
}

#[derive(Debug)]
struct HostPlacement {
    pane_id: PaneId,
    area: Rect,
    cell_size: HostCellSize,
    placement: KittyImagePlacement,
    scrollback_offset: u32,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct ImageSignature {
    image_width: u32,
    image_height: u32,
    format_code: u32,
    data_len: usize,
    data_fingerprint: u64,
}

/// The source that OWNS a host placement slot: the pane plus the ids the client
/// itself used. Two of these can hash to one host placement id, so the owner —
/// not the hash — is what decides whether a slot may be reused.
type PlacementSource = (PaneId, u32, u32);

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct PlacementSignature {
    source: PlacementSource,
    x: u16,
    y: u16,
    cols: u32,
    rows: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    x_offset: u32,
    y_offset: u32,
    z: i32,
    scrollback_offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClippedPlacement {
    x: u16,
    y: u16,
    cols: u32,
    rows: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    x_offset: u32,
    y_offset: u32,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct HostGraphicsCache {
    images: HashMap<u32, ImageSignature>,
    placements: HashMap<(u32, u32), PlacementSignature>,
    /// Host image currently backing each (pane, source image id) pair.
    sources: HashMap<(PaneId, u32), u32>,
    view: Option<HostViewKey>,
}

static KITTY_GRAPHICS_ENABLED: AtomicBool = AtomicBool::new(false);
static LOCAL_HOST_GRAPHICS: OnceLock<Mutex<HostGraphicsCache>> = OnceLock::new();

pub(crate) fn set_enabled(enabled: bool) {
    KITTY_GRAPHICS_ENABLED.store(enabled, Ordering::Release);
}

pub(crate) fn is_enabled() -> bool {
    KITTY_GRAPHICS_ENABLED.load(Ordering::Acquire)
}

pub(crate) fn paint_local_pane_graphics(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    cell_size: HostCellSize,
) -> io::Result<()> {
    let cache = LOCAL_HOST_GRAPHICS.get_or_init(|| Mutex::new(HostGraphicsCache::default()));
    let mut bytes = Vec::new();
    if let Ok(mut cache) = cache.lock() {
        bytes = encode_local_pane_graphics(app, terminal_runtimes, cell_size, &mut cache);
    }
    if bytes.is_empty() {
        return Ok(());
    }

    let mut framed = Vec::with_capacity(bytes.len() + 8);
    framed.extend_from_slice(b"\x1b7");
    framed.extend_from_slice(&bytes);
    framed.extend_from_slice(b"\x1b8");

    let mut stdout = io::stdout().lock();
    stdout.write_all(&framed)?;
    stdout.flush()
}

pub(crate) fn encode_local_pane_graphics(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    cell_size: HostCellSize,
    cache: &mut HostGraphicsCache,
) -> Vec<u8> {
    let mode_ok = app.mode == Mode::Terminal;
    let cell_ok = cell_size.is_known();
    tracing::debug!(
        mode_ok,
        cell_ok,
        cell_width_px = cell_size.width_px,
        cell_height_px = cell_size.height_px,
        active = ?app.active,
        pane_infos_len = app.view.pane_infos.len(),
        "paint_local_pane_graphics entry"
    );
    if !mode_ok || !cell_ok {
        tracing::debug!(
            reason = if !mode_ok {
                "not terminal mode"
            } else {
                "cell size unknown"
            },
            "paint_local_pane_graphics early return"
        );
        return cache.clear_bytes();
    }

    let view_key = active_view_key(app);
    let uploaded_images = cache.images.clone();
    let placements =
        collect_visible_placements(app, terminal_runtimes, cell_size, &uploaded_images);
    tracing::debug!(
        placements_collected = placements.len(),
        "collect_visible_placements result"
    );

    let mut bytes = Vec::new();
    let view_changed = cache.update_view(view_key);
    encode_graphics_update(
        &mut bytes,
        &placements,
        view_changed,
        &mut cache.images,
        &mut cache.placements,
        &mut cache.sources,
    );
    tracing::debug!(
        placements = placements.len(),
        bytes = bytes.len(),
        cell_width_px = cell_size.width_px,
        cell_height_px = cell_size.height_px,
        "painting kitty graphics placements"
    );
    bytes
}

pub(crate) fn has_visible_pane_graphics(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    cell_size: HostCellSize,
) -> bool {
    if app.mode != Mode::Terminal || !cell_size.is_known() {
        return false;
    }

    let Some(ws_idx) = app.active else {
        return false;
    };
    if app
        .workspaces
        .get(ws_idx)
        .and_then(crate::workspace::Workspace::active_tab)
        .is_none()
    {
        return false;
    }

    for info in &app.view.pane_infos {
        let Some(runtime) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        else {
            continue;
        };
        let scrollback_offset = runtime
            .scroll_metrics()
            .map(|m| m.offset_from_bottom as u32)
            .unwrap_or(0);
        for placement in runtime.kitty_image_placements_with_data_filter(|_| false) {
            let host_placement = HostPlacement {
                pane_id: info.id,
                area: info.inner_rect,
                cell_size,
                placement,
                scrollback_offset,
            };
            if clipped_placement(&host_placement).is_some() {
                return true;
            }
        }
    }
    false
}

fn encode_graphics_update(
    bytes: &mut Vec<u8>,
    placements: &[HostPlacement],
    view_changed: bool,
    host_images: &mut HashMap<u32, ImageSignature>,
    host_placements: &mut HashMap<(u32, u32), PlacementSignature>,
    sources: &mut HashMap<(PaneId, u32), u32>,
) {
    // Prune sources that are no longer visible: a stale entry would keep its
    // old host image referenced and block the superseded-image delete.
    let current_sources: HashSet<(PaneId, u32)> = placements
        .iter()
        .map(|placement| (placement.pane_id, placement.placement.image_id))
        .collect();
    sources.retain(|source, _| current_sources.contains(source));

    let mut current_placements = HashSet::new();
    let mut superseded_images = Vec::new();
    for placement in placements {
        let clipped = clipped_placement(placement);
        tracing::debug!(
            pane_id = ?placement.pane_id,
            has_clipped = clipped.is_some(),
            grid_cols = placement.placement.render.grid_cols,
            grid_rows = placement.placement.render.grid_rows,
            viewport_col = placement.placement.render.viewport_col,
            viewport_row = placement.placement.render.viewport_row,
            area_w = placement.area.width,
            area_h = placement.area.height,
            "clipped_placement result"
        );
        let Some((clipped, format_code)) = clipped else {
            continue;
        };
        let image_signature = image_signature(placement, format_code);
        let Some(host_id) = resolve_host_image_id(placement.pane_id, image_signature, host_images)
        else {
            tracing::warn!(
                pane_id = ?placement.pane_id,
                source_image_id = placement.placement.image_id,
                "host image id namespace is full; skipping this placement"
            );
            continue;
        };
        let source = placement_source(placement.pane_id, &placement.placement);
        let Some(host_placement_id) = resolve_host_placement_id(source, host_id, host_placements)
        else {
            tracing::warn!(
                pane_id = ?placement.pane_id,
                source_image_id = placement.placement.image_id,
                source_placement_id = placement.placement.placement_id,
                host_id,
                "host placement id namespace is full; skipping this placement"
            );
            continue;
        };
        let placement_signature = placement_signature(
            source,
            clipped,
            placement.placement.z,
            placement.scrollback_offset,
        );
        let placement_key = (host_id, host_placement_id);
        current_placements.insert(placement_key);

        // `resolve_host_image_id` yields either a free id or one already
        // holding this exact signature, so the id can never be occupied by
        // *different* content here. The arm this match used to carry for that
        // case - delete the resident image, drop its placements and re-upload
        // in its slot - is what made a hash collision lose the other source's
        // placement, and it is gone with the collision.
        if host_images.get(&host_id) != Some(&image_signature) {
            if !encode_upload_image(bytes, placement, format_code, host_id) {
                continue;
            }
            host_images.insert(host_id, image_signature);
        }

        if let Some(previous) =
            sources.insert((placement.pane_id, placement.placement.image_id), host_id)
        {
            if previous != host_id {
                superseded_images.push(previous);
            }
        }

        // A different view can repaint the same cells with text or overlays and
        // leave the host-side Kitty placement state out of sync with this cache.
        // Re-emit the placement even when its geometry signature is unchanged.
        match host_placements.get_mut(&placement_key) {
            Some(existing) if !view_changed && *existing == placement_signature => {}
            Some(existing) => {
                encode_display_placement(
                    bytes,
                    clipped,
                    host_id,
                    host_placement_id,
                    placement.placement.z,
                );
                *existing = placement_signature;
            }
            None => {
                encode_display_placement(
                    bytes,
                    clipped,
                    host_id,
                    host_placement_id,
                    placement.placement.z,
                );
                host_placements.insert(placement_key, placement_signature);
            }
        }
    }

    // A source superseded early in this frame can be the very image a later
    // source in the same frame still points at: `collect_visible_placements`
    // reads the pre-update cache, so that later source carries no payload and
    // could not re-upload an image freed mid-loop. Free a superseded image only
    // once every source in the frame has been registered.
    for previous in superseded_images {
        release_unreferenced_image(
            bytes,
            sources,
            host_images,
            host_placements,
            &mut current_placements,
            previous,
        );
    }

    let mut stale_placements = Vec::new();
    for key in host_placements.keys() {
        if current_placements.contains(key) {
            continue;
        }
        stale_placements.push(*key);
    }
    for (host_id, host_placement_id) in stale_placements {
        encode_delete_placement(bytes, host_id, host_placement_id);
        host_placements.remove(&(host_id, host_placement_id));
    }

    // A source that simply disappeared from the frame supersedes nothing, so
    // the loop above never sees it and its host image would stay uploaded for
    // the rest of the process. Sweep those out here, the way upstream's
    // end-of-frame dead-source pass does: an image referenced by no source in
    // `sources` has lost its last reference. This runs after the whole
    // per-placement loop, so the `4a71ece` guarantee still holds — nothing is
    // freed while a later source in the same frame could still adopt it — and
    // after the stale-placement deletes, so a view change keeps tearing those
    // placements down explicitly instead of only implying it through `d=I`.
    let referenced: HashSet<u32> = sources.values().copied().collect();
    let mut unreferenced_images: Vec<u32> = host_images
        .keys()
        .copied()
        .filter(|host_id| !referenced.contains(host_id))
        .collect();
    unreferenced_images.sort_unstable();
    for host_id in unreferenced_images {
        release_unreferenced_image(
            bytes,
            sources,
            host_images,
            host_placements,
            &mut current_placements,
            host_id,
        );
    }
}

/// Deletes a host image no source of the current frame references any more —
/// one a source moved away from, or one whose only source disappeared.
fn release_unreferenced_image(
    bytes: &mut Vec<u8>,
    sources: &HashMap<(PaneId, u32), u32>,
    host_images: &mut HashMap<u32, ImageSignature>,
    host_placements: &mut HashMap<(u32, u32), PlacementSignature>,
    current_placements: &mut HashSet<(u32, u32)>,
    host_id: u32,
) {
    if sources.values().any(|id| *id == host_id) {
        return;
    }
    // Already freed earlier in this same frame.
    if host_images.remove(&host_id).is_none() {
        return;
    }
    encode_delete_image(bytes, host_id);
    // The `d=I` delete also removes the image's placements host-side.
    host_placements.retain(|(image_id, placement_id), _| {
        if *image_id == host_id {
            current_placements.remove(&(*image_id, *placement_id));
            false
        } else {
            true
        }
    });
}

pub(crate) fn clear_all_host_graphics() -> io::Result<()> {
    let cache = LOCAL_HOST_GRAPHICS.get_or_init(|| Mutex::new(HostGraphicsCache::default()));
    let mut bytes = Vec::new();
    if let Ok(mut cache) = cache.lock() {
        bytes = cache.clear_bytes();
    }
    if bytes.is_empty() {
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.flush()
}

impl HostGraphicsCache {
    pub(crate) fn is_empty(&self) -> bool {
        self.images.is_empty() && self.placements.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn test_mark_non_empty(&mut self) {
        self.images.insert(
            HOST_IMAGE_ID_BASE,
            ImageSignature {
                image_width: 1,
                image_height: 1,
                format_code: 32,
                data_len: 4,
                data_fingerprint: 1,
            },
        );
    }

    pub(crate) fn clear_bytes(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for id in self.images.keys().copied().collect::<Vec<_>>() {
            encode_delete_image(&mut bytes, id);
        }
        self.images.clear();
        self.placements.clear();
        self.sources.clear();
        self.view = None;
        bytes
    }

    fn update_view(&mut self, view_key: Option<HostViewKey>) -> bool {
        if self.view == view_key {
            return false;
        }
        self.view = view_key;
        true
    }
}

fn active_view_key(app: &AppState) -> Option<HostViewKey> {
    let ws_idx = app.active?;
    let ws = app.workspaces.get(ws_idx)?;
    Some(HostViewKey {
        workspace_index: ws_idx,
        tab_index: ws.active_tab_index(),
    })
}

fn collect_visible_placements(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    cell_size: HostCellSize,
    uploaded_images: &HashMap<u32, ImageSignature>,
) -> Vec<HostPlacement> {
    let ws_idx = match app.active {
        Some(idx) => idx,
        None => {
            tracing::debug!("collect_visible_placements: no active workspace");
            return Vec::new();
        }
    };
    if app
        .workspaces
        .get(ws_idx)
        .and_then(crate::workspace::Workspace::active_tab)
        .is_none()
    {
        tracing::debug!(ws_idx, "collect_visible_placements: no active tab");
        return Vec::new();
    }

    tracing::debug!(
        ws_idx,
        terminal_runtimes_len = terminal_runtimes.len(),
        pane_infos_len = app.view.pane_infos.len(),
        "collect_visible_placements: starting iteration"
    );
    let mut placements = Vec::new();
    for info in &app.view.pane_infos {
        let runtime = match app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id) {
            Some(rt) => rt,
            None => {
                tracing::debug!(pane_id = ?info.id, "collect_visible_placements: runtime not found");
                continue;
            }
        };
        for placement in runtime.kitty_image_placements_with_data_filter(|descriptor| {
            let format_code = kitty_format_code(descriptor.format);
            let signature = image_signature_from_descriptor(descriptor, format_code);
            // Probe exactly the way the encode pass will, so a signature
            // parked at a probed id still counts as already uploaded.
            resolve_host_image_id(info.id, signature, uploaded_images)
                .and_then(|host_id| uploaded_images.get(&host_id).copied())
                != Some(signature)
        }) {
            let scrollback_offset = runtime
                .scroll_metrics()
                .map(|m| m.offset_from_bottom as u32)
                .unwrap_or(0);
            placements.push(HostPlacement {
                pane_id: info.id,
                area: info.inner_rect,
                cell_size,
                placement,
                scrollback_offset,
            });
        }
    }
    tracing::debug!(
        placements_len = placements.len(),
        "collect_visible_placements: done"
    );
    placements
}

/// Resolves the host image id a signature is uploaded at.
///
/// The hash is only where the search starts. `host_image_id_for_signature`
/// truncates a 64-bit hash into a `HOST_IMAGE_ID_SPAN`-wide namespace and the
/// result is the cache key, so two distinct signatures can claim one id: the
/// second one used to evict the first one's live image and take its slot,
/// which lost the first source's placement for as long as both stayed visible.
/// Probe forward from the hashed id instead, past every slot holding
/// *different* content, and stop at the first free slot or at one already
/// holding this exact signature - so content dedup still shares a single host
/// image. `None` means the whole namespace is occupied by other content; the
/// caller skips that placement rather than overwriting a live image.
fn resolve_host_image_id(
    pane_id: PaneId,
    signature: ImageSignature,
    host_images: &HashMap<u32, ImageSignature>,
) -> Option<u32> {
    let start = host_image_id_for_signature(pane_id, signature) - HOST_IMAGE_ID_BASE;
    (0..HOST_IMAGE_ID_SPAN).find_map(|step| {
        let host_id = HOST_IMAGE_ID_BASE + (start + step) % HOST_IMAGE_ID_SPAN;
        match host_images.get(&host_id) {
            Some(existing) if *existing != signature => None,
            _ => Some(host_id),
        }
    })
}

/// Resolves the host placement id a source placement is displayed at.
///
/// One namespace below `resolve_host_image_id`, and for the same reason.
/// `host_placement_id` truncates a 64-bit hash of the source triple into a
/// `HOST_PLACEMENT_ID_SPAN`-wide namespace, and `(host_id, host_placement_id)`
/// is the placement cache key, so two DIFFERENT sources of the same host image
/// can claim one id: the second overwrote the first in `host_placements` and
/// one of two visible placements was simply lost. Probe forward from the hashed
/// id instead, past every slot under this host image held by a different
/// source, and stop at the first free slot or at the one already assigned to
/// this source.
///
/// Determinism: the probe reads only the persisted cache, so a source keeps its
/// resolved id for as long as the colliding owner stays visible - independent of
/// the order the two arrive in within a frame. When that owner disappears its
/// entry is swept at the END of the frame, so the survivor holds its probed id
/// through that frame and may re-place at the freed base id on the next one.
/// That churn is bounded at one delete plus one place, and no frame is ever left
/// without the placement.
///
/// `None` means every slot in the namespace under this host image is held by
/// other sources; the caller skips that placement rather than overwriting a live
/// one.
fn resolve_host_placement_id(
    source: PlacementSource,
    host_id: u32,
    host_placements: &HashMap<(u32, u32), PlacementSignature>,
) -> Option<u32> {
    let start = host_placement_id(source) - HOST_PLACEMENT_ID_BASE;
    (0..HOST_PLACEMENT_ID_SPAN).find_map(|step| {
        let candidate = HOST_PLACEMENT_ID_BASE + (start + step) % HOST_PLACEMENT_ID_SPAN;
        match host_placements.get(&(host_id, candidate)) {
            Some(existing) if existing.source != source => None,
            _ => Some(candidate),
        }
    })
}

fn host_image_id_for_signature(pane_id: PaneId, signature: ImageSignature) -> u32 {
    let mut hasher = DefaultHasher::new();
    pane_id.raw().hash(&mut hasher);
    signature.hash(&mut hasher);
    HOST_IMAGE_ID_BASE + ((hasher.finish() as u32) % HOST_IMAGE_ID_SPAN)
}

fn placement_source(pane_id: PaneId, placement: &KittyImagePlacement) -> PlacementSource {
    (pane_id, placement.image_id, placement.placement_id)
}

fn host_placement_id(source: PlacementSource) -> u32 {
    let (pane_id, image_id, placement_id) = source;
    let mut hasher = DefaultHasher::new();
    pane_id.raw().hash(&mut hasher);
    image_id.hash(&mut hasher);
    placement_id.hash(&mut hasher);
    HOST_PLACEMENT_ID_BASE + ((hasher.finish() as u32) % HOST_PLACEMENT_ID_SPAN)
}

fn encode_delete_image(out: &mut Vec<u8>, id: u32) {
    let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\");
}

fn encode_delete_placement(out: &mut Vec<u8>, host_id: u32, host_placement_id: u32) {
    let _ = write!(
        out,
        "\x1b_Ga=d,d=i,i={host_id},p={host_placement_id},q=2;\x1b\\"
    );
}

fn encode_upload_image(
    out: &mut Vec<u8>,
    placement: &HostPlacement,
    format_code: u32,
    host_id: u32,
) -> bool {
    if placement.placement.data.is_empty() {
        return false;
    }

    let control = format!(
        "a=t,t=d,f={format_code},s={},v={},i={host_id},q=2",
        placement.placement.image_width, placement.placement.image_height,
    );
    encode_kitty_data(out, &control, &placement.placement.data);
    true
}

fn encode_display_placement(
    out: &mut Vec<u8>,
    clipped: ClippedPlacement,
    host_id: u32,
    host_placement_id: u32,
    z: i32,
) {
    let _ = write!(out, "\x1b[{};{}H", clipped.y + 1, clipped.x + 1);
    let mut control = format!(
        "a=p,i={host_id},p={host_placement_id},c={},r={},z={z},C=1,q=2",
        clipped.cols, clipped.rows,
    );
    if clipped.source_x > 0 {
        let _ = write!(control, ",x={}", clipped.source_x);
    }
    if clipped.source_y > 0 {
        let _ = write!(control, ",y={}", clipped.source_y);
    }
    if clipped.source_width > 0 {
        let _ = write!(control, ",w={}", clipped.source_width);
    }
    if clipped.source_height > 0 {
        let _ = write!(control, ",h={}", clipped.source_height);
    }
    if clipped.x_offset > 0 {
        let _ = write!(control, ",X={}", clipped.x_offset);
    }
    if clipped.y_offset > 0 {
        let _ = write!(control, ",Y={}", clipped.y_offset);
    }

    let _ = write!(out, "\x1b_G{control};\x1b\\");
}

fn clipped_placement(placement: &HostPlacement) -> Option<(ClippedPlacement, u32)> {
    if placement.area.width == 0 || placement.area.height == 0 {
        tracing::debug!(
            area_w = placement.area.width,
            area_h = placement.area.height,
            "clipped_placement: area zero"
        );
        return None;
    }
    let render = placement.placement.render;
    if render.grid_cols == 0 || render.grid_rows == 0 {
        tracing::debug!(
            grid_cols = render.grid_cols,
            grid_rows = render.grid_rows,
            "clipped_placement: grid zero"
        );
        return None;
    }
    let format_code = kitty_format_code(placement.placement.format);

    let left_clip_cells = if render.viewport_col < 0 {
        render.viewport_col.saturating_neg() as u32
    } else {
        0
    };
    let top_clip_cells = if render.viewport_row < 0 {
        render.viewport_row.saturating_neg() as u32
    } else {
        0
    };
    let viewport_col = render.viewport_col.max(0) as u32;
    let viewport_row = render.viewport_row.max(0) as u32;
    tracing::debug!(
        viewport_col = viewport_col,
        viewport_row = viewport_row,
        area_w = placement.area.width,
        area_h = placement.area.height,
        scrollback_offset = placement.scrollback_offset,
        raw_viewport_row = render.viewport_row,
        cond1 = viewport_col >= placement.area.width as u32,
        cond2 = viewport_row >= placement.area.height as u32,
        "clipped_placement: viewport check"
    );
    if viewport_col >= placement.area.width as u32 || viewport_row >= placement.area.height as u32 {
        return None;
    }

    let visible_cols = render
        .grid_cols
        .saturating_sub(left_clip_cells)
        .min(placement.area.width as u32 - viewport_col);
    let visible_rows = render
        .grid_rows
        .saturating_sub(top_clip_cells)
        .min(placement.area.height as u32 - viewport_row);
    tracing::debug!(
        visible_cols = visible_cols,
        visible_rows = visible_rows,
        left_clip_cells = left_clip_cells,
        top_clip_cells = top_clip_cells,
        "clipped_placement: visible dims check"
    );
    if visible_cols == 0 || visible_rows == 0 {
        return None;
    }

    let source_width = if render.source_width == 0 {
        placement.placement.image_width
    } else {
        render.source_width
    };
    let source_height = if render.source_height == 0 {
        placement.placement.image_height
    } else {
        render.source_height
    };
    let pixel_width = render
        .pixel_width
        .max(
            render
                .grid_cols
                .saturating_mul(placement.cell_size.width_px),
        )
        .max(1);
    let pixel_height = render
        .pixel_height
        .max(
            render
                .grid_rows
                .saturating_mul(placement.cell_size.height_px),
        )
        .max(1);

    let crop_left_px = left_clip_cells.saturating_mul(placement.cell_size.width_px);
    let crop_top_px = top_clip_cells.saturating_mul(placement.cell_size.height_px);
    let visible_width_px = visible_cols.saturating_mul(placement.cell_size.width_px);
    let visible_height_px = visible_rows.saturating_mul(placement.cell_size.height_px);

    let source_x = render.source_x + scale_pixels(crop_left_px, source_width, pixel_width);
    let source_y = render.source_y + scale_pixels(crop_top_px, source_height, pixel_height);
    let source_width = scale_pixels(visible_width_px, source_width, pixel_width)
        .max(1)
        .min(placement.placement.image_width.saturating_sub(source_x));
    let source_height = scale_pixels(visible_height_px, source_height, pixel_height)
        .max(1)
        .min(placement.placement.image_height.saturating_sub(source_y));

    if source_width == 0 || source_height == 0 {
        tracing::debug!(
            source_width = source_width,
            source_height = source_height,
            image_width = placement.placement.image_width,
            image_height = placement.placement.image_height,
            "clipped_placement: source dims zero"
        );
        return None;
    }

    tracing::debug!("clipped_placement: success");
    Some((
        ClippedPlacement {
            x: placement.area.x + viewport_col as u16,
            y: placement.area.y + viewport_row as u16,
            cols: visible_cols,
            rows: visible_rows,
            source_x,
            source_y,
            source_width,
            source_height,
            x_offset: if left_clip_cells == 0 {
                placement.placement.x_offset
            } else {
                0
            },
            y_offset: if top_clip_cells == 0 {
                placement.placement.y_offset
            } else {
                0
            },
        },
        format_code,
    ))
}

fn scale_pixels(value: u32, source: u32, dest: u32) -> u32 {
    ((value as u64).saturating_mul(source as u64) / dest.max(1) as u64).min(u32::MAX as u64) as u32
}

fn image_signature(placement: &HostPlacement, format_code: u32) -> ImageSignature {
    ImageSignature {
        image_width: placement.placement.image_width,
        image_height: placement.placement.image_height,
        format_code,
        data_len: placement.placement.data_len,
        data_fingerprint: placement.placement.data_fingerprint,
    }
}

fn image_signature_from_descriptor(
    descriptor: KittyImageDescriptor,
    format_code: u32,
) -> ImageSignature {
    ImageSignature {
        image_width: descriptor.image_width,
        image_height: descriptor.image_height,
        format_code,
        data_len: descriptor.data_len,
        data_fingerprint: descriptor.data_fingerprint,
    }
}

fn placement_signature(
    source: PlacementSource,
    clipped: ClippedPlacement,
    z: i32,
    scrollback_offset: u32,
) -> PlacementSignature {
    PlacementSignature {
        source,
        x: clipped.x,
        y: clipped.y,
        cols: clipped.cols,
        rows: clipped.rows,
        source_x: clipped.source_x,
        source_y: clipped.source_y,
        source_width: clipped.source_width,
        source_height: clipped.source_height,
        x_offset: clipped.x_offset,
        y_offset: clipped.y_offset,
        z,
        scrollback_offset,
    }
}

fn kitty_format_code(format: KittyImageFormat) -> u32 {
    match format {
        KittyImageFormat::Rgb => 24,
        KittyImageFormat::Rgba => 32,
        KittyImageFormat::Png => 100,
    }
}

fn encode_kitty_data(out: &mut Vec<u8>, control: &str, data: &[u8]) {
    let mut chunks = data.chunks(KITTY_CHUNK_BYTES).peekable();
    let Some(first) = chunks.next() else {
        return;
    };
    let more = if chunks.peek().is_some() { 1 } else { 0 };
    let encoded = base64::engine::general_purpose::STANDARD.encode(first);
    let _ = write!(out, "\x1b_G{control},m={more};{encoded}\x1b\\");

    while let Some(chunk) = chunks.next() {
        let more = if chunks.peek().is_some() { 1 } else { 0 };
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        let _ = write!(out, "\x1b_Gm={more};{encoded}\x1b\\");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghostty::KittyPlacementRenderInfo;

    /// The id a signature hashes to, before any collision probing. Tests that
    /// need the *resolved* id read it back from the cache instead.
    fn hashed_host_image_id(placement: &HostPlacement) -> u32 {
        host_image_id_for_signature(
            placement.pane_id,
            image_signature(placement, kitty_format_code(placement.placement.format)),
        )
    }

    /// The first pair of distinct inputs that `hash` maps to one id.
    /// `DefaultHasher` is deterministic inside a build, so the search is
    /// repeatable, but its output is not stable across Rust versions - hence
    /// every collision test searches for its pair instead of hard-coding one.
    fn first_colliding_pair<T: Copy>(
        inputs: impl Iterator<Item = T>,
        hash: impl Fn(T) -> u32,
    ) -> (T, T) {
        let mut seen: HashMap<u32, T> = HashMap::new();
        for input in inputs {
            if let Some(previous) = seen.insert(hash(input), input) {
                return (previous, input);
            }
        }
        panic!("no colliding ids in the search range");
    }

    /// Two distinct image signatures for one pane whose hashed host image ids
    /// collide.
    fn colliding_data_fingerprints(pane_id: PaneId) -> (u64, u64) {
        let template = image_signature(
            &test_placement(0, 0),
            kitty_format_code(KittyImageFormat::Rgba),
        );
        first_colliding_pair(0..10_000_000u64, |data_fingerprint| {
            host_image_id_for_signature(
                pane_id,
                ImageSignature {
                    data_fingerprint,
                    ..template
                },
            )
        })
    }

    /// Two distinct source placement ids for one pane and source image whose
    /// hashed host placement ids collide.
    fn colliding_placement_ids(pane_id: PaneId, image_id: u32) -> (u32, u32) {
        first_colliding_pair(0..10_000_000u32, |placement_id| {
            host_placement_id((pane_id, image_id, placement_id))
        })
    }

    /// The `p=` host placement id of every `a=p` (display placement) command in
    /// an update, in emission order.
    fn displayed_placement_ids(update: &str) -> Vec<u32> {
        update
            .split("\x1b_G")
            .filter(|command| command.starts_with("a=p,"))
            .filter_map(|command| {
                command
                    .split(',')
                    .find_map(|field| field.strip_prefix("p="))
                    .and_then(|value| value.parse().ok())
            })
            .collect()
    }

    fn test_placement(viewport_col: i32, viewport_row: i32) -> HostPlacement {
        HostPlacement {
            pane_id: PaneId::from_raw(1),
            area: Rect::new(0, 0, 20, 10),
            cell_size: HostCellSize {
                width_px: 10,
                height_px: 10,
            },
            scrollback_offset: 0,
            placement: KittyImagePlacement {
                image_id: 7,
                placement_id: 3,
                z: 0,
                x_offset: 0,
                y_offset: 0,
                image_width: 30,
                image_height: 30,
                format: KittyImageFormat::Rgba,
                data_len: 30 * 30 * 4,
                data_fingerprint: 42,
                data: vec![255; 30 * 30 * 4],
                render: KittyPlacementRenderInfo {
                    pixel_width: 0,
                    pixel_height: 0,
                    grid_cols: 3,
                    grid_rows: 3,
                    viewport_col,
                    viewport_row,
                    source_x: 0,
                    source_y: 0,
                    source_width: 0,
                    source_height: 0,
                },
            },
        }
    }

    #[test]
    fn clipped_placement_handles_positive_viewport_without_wrapping() {
        let placement = test_placement(2, 2);
        let (clipped, _) = clipped_placement(&placement).expect("visible placement");

        assert_eq!(clipped.x, 2);
        assert_eq!(clipped.y, 2);
        assert_eq!(clipped.cols, 3);
        assert_eq!(clipped.rows, 3);
        assert_eq!(clipped.source_x, 0);
        assert_eq!(clipped.source_y, 0);
    }

    #[test]
    fn clipped_placement_crops_negative_viewport_offsets() {
        let placement = test_placement(-1, -1);
        let (clipped, _) = clipped_placement(&placement).expect("partially visible placement");

        assert_eq!(clipped.x, 0);
        assert_eq!(clipped.y, 0);
        assert_eq!(clipped.cols, 2);
        assert_eq!(clipped.rows, 2);
        assert_eq!(clipped.source_x, 10);
        assert_eq!(clipped.source_y, 10);
    }

    #[test]
    fn graphics_update_uploads_once_then_repositions_only() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let first = String::from_utf8_lossy(&bytes);
        assert!(first.contains("a=t"));
        assert!(first.contains("a=p"));

        bytes.clear();
        let same = test_placement(0, 0);
        encode_graphics_update(
            &mut bytes,
            &[same],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert!(bytes.is_empty());

        let mut z_changed = test_placement(0, 0);
        z_changed.placement.z = 1;
        encode_graphics_update(
            &mut bytes,
            &[z_changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let z_changed_bytes = String::from_utf8_lossy(&bytes);
        assert!(!z_changed_bytes.contains("a=t"));
        assert!(z_changed_bytes.contains("a=p"));

        bytes.clear();
        let moved = test_placement(0, 1);
        encode_graphics_update(
            &mut bytes,
            &[moved],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let moved_bytes = String::from_utf8_lossy(&bytes);
        assert!(!moved_bytes.contains("a=t"));
        assert!(moved_bytes.contains("a=p"));
    }

    #[test]
    fn view_change_redisplays_unchanged_visible_placement() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(placements.len(), 1);

        bytes.clear();
        let same = test_placement(0, 0);
        encode_graphics_update(
            &mut bytes,
            &[same],
            true,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let redisplay = String::from_utf8_lossy(&bytes);
        assert!(!redisplay.contains("a=t"));
        assert!(redisplay.contains("a=p"));
        assert_eq!(placements.len(), 1);
    }

    #[test]
    fn surface_reset_deletes_then_reuploads_and_redisplays_placement() {
        let mut cache = HostGraphicsCache::default();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );
        assert_eq!(cache.images.len(), 1);
        assert_eq!(cache.placements.len(), 1);

        bytes = cache.clear_bytes();
        let same = test_placement(0, 0);
        encode_graphics_update(
            &mut bytes,
            &[same],
            false,
            &mut cache.images,
            &mut cache.placements,
            &mut cache.sources,
        );

        let redisplay = String::from_utf8_lossy(&bytes);
        assert!(redisplay.contains("a=d,d=I"));
        assert!(redisplay.contains("a=t"));
        assert!(redisplay.contains("a=p"));
        assert_eq!(cache.images.len(), 1);
        assert_eq!(cache.placements.len(), 1);
    }

    #[test]
    fn scrollback_offset_change_redisplays_placement() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        bytes.clear();
        let mut scrolled = test_placement(0, 0);
        scrolled.scrollback_offset = 3;
        encode_graphics_update(
            &mut bytes,
            &[scrolled],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let redisplay = String::from_utf8_lossy(&bytes);
        assert!(!redisplay.contains("a=t"));
        assert!(redisplay.contains("a=p"));
    }

    #[test]
    fn empty_image_data_does_not_mark_image_uploaded() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let mut placement = test_placement(0, 0);
        placement.placement.data.clear();

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        assert!(bytes.is_empty());
        assert!(images.is_empty());
        assert!(placements.is_empty());
    }

    #[test]
    fn same_image_signature_reuses_host_upload_across_source_image_ids() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let first = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[first],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);

        bytes.clear();
        let mut same_image_new_source_id = test_placement(0, 0);
        same_image_new_source_id.placement.image_id = 8;
        same_image_new_source_id.placement.placement_id = 4;
        same_image_new_source_id.placement.data.clear();
        encode_graphics_update(
            &mut bytes,
            &[same_image_new_source_id],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let reused = String::from_utf8_lossy(&bytes);
        assert!(!reused.contains("a=t"));
        assert!(reused.contains("a=p"));
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);
    }

    #[test]
    fn new_source_keeps_image_needed_later_in_the_same_update() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let first = test_placement(0, 0);
        let old_host_id = hashed_host_image_id(&first);
        encode_graphics_update(
            &mut bytes,
            &[first],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        let mut new_source = test_placement(5, 5);
        new_source.placement.image_id = 8;
        new_source.placement.placement_id = 4;
        // Production collects against the pre-update cache and omits data
        // for a signature already uploaded to the host.
        assert!(images.contains_key(&hashed_host_image_id(&new_source)));
        new_source.placement.data.clear();
        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[changed, new_source],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        assert!(
            images.contains_key(&old_host_id),
            "a later visible source still needs this image"
        );
        assert_eq!(placements.len(), 2, "both placements must remain visible");
    }

    #[test]
    fn new_source_order_does_not_change_superseded_image_handling() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let first = test_placement(0, 0);
        let old_host_id = hashed_host_image_id(&first);
        encode_graphics_update(
            &mut bytes,
            &[first],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        // Mirror of `new_source_keeps_image_needed_later_in_the_same_update`:
        // the new source that still needs the old image comes first, and the
        // source that supersedes it comes second.
        let mut new_source = test_placement(5, 5);
        new_source.placement.image_id = 8;
        new_source.placement.placement_id = 4;
        new_source.placement.data.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[new_source, changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(
            !update.contains(&format!("a=d,d=I,i={old_host_id}")),
            "a live source still references the old image"
        );
        assert!(images.contains_key(&old_host_id));
        assert_eq!(placements.len(), 2, "both placements must remain visible");
    }

    #[test]
    fn replaced_image_content_deletes_superseded_host_image() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let first = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[first],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        let superseded_host_id = *images.keys().next().expect("uploaded host image");

        // Same source image id, new pixel content: the fresh content maps to
        // a fresh host image id, so the replaced one must be deleted.
        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(update.contains("a=t"), "changed content re-uploads");
        assert!(
            update.contains(&format!("a=d,d=I,i={superseded_host_id}")),
            "superseded host image is deleted"
        );
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);
    }

    #[test]
    fn shared_host_image_survives_while_another_source_references_it() {
        fn twin_placement() -> HostPlacement {
            let mut twin = test_placement(5, 5);
            twin.placement.image_id = 8;
            twin.placement.placement_id = 4;
            twin
        }

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();

        encode_graphics_update(
            &mut bytes,
            &[test_placement(0, 0), twin_placement()],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1, "same content dedups to one host image");

        // One source moves to new content while the other still shows the
        // old image: the shared host image must survive.
        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed, twin_placement()],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(!update.contains("a=d,d=I"), "shared host image survives");
        assert_eq!(images.len(), 2);
    }

    #[test]
    fn stale_source_entry_does_not_block_superseded_image_delete() {
        fn twin_placement() -> HostPlacement {
            let mut twin = test_placement(5, 5);
            twin.placement.image_id = 8;
            twin.placement.placement_id = 4;
            twin
        }

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();

        encode_graphics_update(
            &mut bytes,
            &[test_placement(0, 0), twin_placement()],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 2);
        let shared_host_id = *images.keys().next().expect("uploaded host image");

        // The twin source is gone and the survivor changed content: the
        // vanished source's stale entry must not keep the old host image
        // alive.
        bytes.clear();
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = 43;
        encode_graphics_update(
            &mut bytes,
            &[changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(
            update.contains(&format!("a=d,d=I,i={shared_host_id}")),
            "old host image is deleted once its last live source moves on"
        );
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn stale_placement_deletes_placement_and_the_now_unreferenced_image() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);
        let host_id = hashed_host_image_id(&placement);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(placements.len(), 1);

        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let delete = String::from_utf8_lossy(&bytes);
        assert!(delete.contains("a=d,d=i"), "the stale placement is deleted");
        assert!(
            delete.contains(&format!("a=d,d=I,i={host_id}")),
            "the image no source references any more is released"
        );
        assert!(placements.is_empty());
        assert!(images.is_empty());
    }

    #[test]
    fn stale_sole_source_releases_unreferenced_host_image() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);
        let host_id = hashed_host_image_id(&placement);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1);
        assert_eq!(sources.len(), 1);

        // The pane stopped drawing the image: its only source is gone, so the
        // host image it kept alive has to be released.
        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(
            update.contains(&format!("a=d,d=I,i={host_id}")),
            "the last reference went away, so the host image is deleted"
        );
        assert!(images.is_empty(), "no host image is left behind");
        assert!(placements.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn shared_host_image_survives_when_one_source_disappears() {
        fn twin_placement() -> HostPlacement {
            let mut twin = test_placement(5, 5);
            twin.placement.image_id = 8;
            twin.placement.placement_id = 4;
            twin
        }

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let survivor = test_placement(0, 0);
        let host_id = hashed_host_image_id(&survivor);

        encode_graphics_update(
            &mut bytes,
            &[survivor, twin_placement()],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.len(), 1, "same content dedups to one host image");
        assert_eq!(placements.len(), 2);

        // Only the twin stops being drawn: the shared host image still backs a
        // live source, so only the twin's placement may be deleted.
        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[test_placement(0, 0)],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(
            !update.contains("d=I"),
            "a source still references this host image"
        );
        assert!(update.contains("a=d,d=i"), "the twin placement is deleted");
        assert!(images.contains_key(&host_id));
        assert_eq!(images.len(), 1);
        assert_eq!(placements.len(), 1);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn host_image_id_collision_preserves_both_visible_placements() {
        let pane_id = PaneId::from_raw(1);
        let (first_fingerprint, second_fingerprint) = colliding_data_fingerprints(pane_id);

        let mut first = test_placement(0, 0);
        first.placement.data_fingerprint = first_fingerprint;
        let mut second = test_placement(5, 5);
        second.placement.image_id = 8;
        second.placement.placement_id = 4;
        second.placement.data_fingerprint = second_fingerprint;
        assert_eq!(
            hashed_host_image_id(&first),
            hashed_host_image_id(&second),
            "the two signatures must hash to the same host image id"
        );

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[first, second],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(
            !update.contains("d=I"),
            "a colliding id must not delete the other source's live image"
        );
        assert_eq!(
            update.matches("a=t").count(),
            2,
            "both images are uploaded to the host"
        );
        assert_eq!(images.len(), 2, "each signature gets its own host image");
        assert_eq!(placements.len(), 2, "both placements stay visible");
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn colliding_signature_dedups_only_identical_content() {
        // Probing moves an id only when the slot holds *different* content:
        // two sources showing the same image still share one host upload.
        let mut twin = test_placement(5, 5);
        twin.placement.image_id = 8;
        twin.placement.placement_id = 4;

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[test_placement(0, 0), twin],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert_eq!(
            update.matches("a=t").count(),
            1,
            "identical content is uploaded once"
        );
        assert_eq!(images.len(), 1, "identical content shares one host image");
        assert_eq!(placements.len(), 2);
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn probing_keeps_the_superseded_release_correct() {
        let pane_id = PaneId::from_raw(1);
        let (first_fingerprint, second_fingerprint) = colliding_data_fingerprints(pane_id);

        let mut first = test_placement(0, 0);
        first.placement.data_fingerprint = first_fingerprint;
        let hashed_id = hashed_host_image_id(&first);

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[first],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert_eq!(images.keys().copied().collect::<Vec<_>>(), vec![hashed_id]);

        // The one source moves to content that hashes onto the id its own old
        // content still occupies: the new image is probed to a free id and the
        // old one is released through the superseded path, not overwritten.
        let mut changed = test_placement(0, 0);
        changed.placement.data_fingerprint = second_fingerprint;
        assert_eq!(hashed_host_image_id(&changed), hashed_id);
        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[changed],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert!(update.contains("a=t"), "the new content is uploaded");
        assert!(
            update.contains(&format!("a=d,d=I,i={hashed_id}")),
            "the superseded image is released"
        );
        assert_eq!(images.len(), 1);
        let surviving = *images.keys().next().expect("one host image");
        assert_ne!(
            surviving, hashed_id,
            "the new content lives at a probed id, not on top of the old one"
        );
        assert_eq!(placements.len(), 1);
        assert_eq!(sources.len(), 1);
    }

    /// The one host image id in a cache holding exactly one image.
    fn host_id_of(images: &HashMap<u32, ImageSignature>) -> u32 {
        assert_eq!(images.len(), 1, "expected exactly one host image");
        *images.keys().next().expect("one host image")
    }

    #[test]
    fn host_placement_id_collision_preserves_both_visible_placements() {
        // Gate-3 B1 #9b: `(host_id, host_placement_id)` is the placement cache key, and
        // `host_placement_id` truncates a hash, so two distinct sources of the SAME host
        // image can claim one id. The second used to overwrite the first, and one of two
        // visible placements simply vanished (the inspector reproduced it at id 105494).
        let pane_id = PaneId::from_raw(1);
        let image_id = 7;
        let (first_placement_id, second_placement_id) = colliding_placement_ids(pane_id, image_id);
        assert_eq!(
            host_placement_id((pane_id, image_id, first_placement_id)),
            host_placement_id((pane_id, image_id, second_placement_id)),
            "the two sources must hash to the same host placement id"
        );

        let mut first = test_placement(0, 0);
        first.placement.placement_id = first_placement_id;
        let mut second = test_placement(5, 5);
        second.placement.placement_id = second_placement_id;

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        encode_graphics_update(
            &mut bytes,
            &[first, second],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let update = String::from_utf8_lossy(&bytes);
        assert_eq!(placements.len(), 2, "both placements stay in the cache");
        assert!(
            !update.contains("a=d"),
            "nothing is deleted while both sources are visible"
        );
        let placed = displayed_placement_ids(&update);
        assert_eq!(placed.len(), 2, "both placements are displayed");
        assert_ne!(
            placed[0], placed[1],
            "colliding sources must be placed at DIFFERENT host placement ids"
        );
        assert_eq!(images.len(), 1, "identical content shares one host image");
    }

    #[test]
    fn placement_probe_is_stable_while_the_collider_persists() {
        // A probed id is not re-negotiated every frame: while the colliding owner stays
        // visible, each source keeps the slot it resolved to, so an unchanged frame emits
        // nothing at all.
        let pane_id = PaneId::from_raw(1);
        let image_id = 7;
        let (first_placement_id, second_placement_id) = colliding_placement_ids(pane_id, image_id);

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        for _ in 0..2 {
            let mut first = test_placement(0, 0);
            first.placement.placement_id = first_placement_id;
            let mut second = test_placement(5, 5);
            second.placement.placement_id = second_placement_id;
            bytes.clear();
            encode_graphics_update(
                &mut bytes,
                &[first, second],
                false,
                &mut images,
                &mut placements,
                &mut sources,
            );
        }

        assert!(
            bytes.is_empty(),
            "an unchanged second frame must emit nothing: {:?}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(placements.len(), 2);
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn placement_probe_releases_the_slot_when_the_collider_disappears() {
        // When the source holding the hashed id disappears, its placement is deleted and
        // the survivor may move back to the freed base id. That churn is bounded: one
        // delete plus one place, and the survivor is never lost in between.
        let pane_id = PaneId::from_raw(1);
        let image_id = 7;
        let (first_placement_id, second_placement_id) = colliding_placement_ids(pane_id, image_id);
        let base_id = host_placement_id((pane_id, image_id, first_placement_id));

        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();

        let mut first = test_placement(0, 0);
        first.placement.placement_id = first_placement_id;
        let mut second = test_placement(5, 5);
        second.placement.placement_id = second_placement_id;
        encode_graphics_update(
            &mut bytes,
            &[first, second],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let probed_id = *displayed_placement_ids(&String::from_utf8_lossy(&bytes))
            .iter()
            .find(|id| **id != base_id)
            .expect("the second source is placed at a probed id");

        // The base-id owner disappears. Its placement is torn down; the survivor stays
        // where it is for this frame, because the freed slot is only swept at the end.
        let mut survivor = test_placement(5, 5);
        survivor.placement.placement_id = second_placement_id;
        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[survivor],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let update = String::from_utf8_lossy(&bytes);
        assert!(
            update.contains(&format!("a=d,d=i,i={},p={base_id}", host_id_of(&images))),
            "the departed source's placement is deleted: {update:?}"
        );
        assert!(
            displayed_placement_ids(&update).is_empty(),
            "the survivor is not re-placed in the same frame: {update:?}"
        );
        assert_eq!(placements.len(), 1, "only the survivor is cached");

        // Next frame the base id is free, so the survivor re-places there exactly once.
        let mut survivor = test_placement(5, 5);
        survivor.placement.placement_id = second_placement_id;
        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[survivor],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        let update = String::from_utf8_lossy(&bytes);
        assert_eq!(
            displayed_placement_ids(&update),
            vec![base_id],
            "the survivor re-places once, at the freed base id: {update:?}"
        );
        assert_eq!(
            update.matches("a=d,d=i").count(),
            1,
            "its old probed slot is released once: {update:?}"
        );
        assert!(
            update.contains(&format!("p={probed_id}")),
            "the released slot is the probed one: {update:?}"
        );
        assert_eq!(placements.len(), 1);

        // And it settles: a further identical frame emits nothing.
        let mut survivor = test_placement(5, 5);
        survivor.placement.placement_id = second_placement_id;
        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[survivor],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        assert!(
            bytes.is_empty(),
            "the survivor has settled: {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    #[test]
    fn view_change_deletes_stale_placement_immediately() {
        let mut images = HashMap::new();
        let mut placements = HashMap::new();
        let mut sources = HashMap::new();
        let mut bytes = Vec::new();
        let placement = test_placement(0, 0);

        encode_graphics_update(
            &mut bytes,
            &[placement],
            false,
            &mut images,
            &mut placements,
            &mut sources,
        );
        bytes.clear();
        encode_graphics_update(
            &mut bytes,
            &[],
            true,
            &mut images,
            &mut placements,
            &mut sources,
        );

        let delete = String::from_utf8_lossy(&bytes);
        assert!(delete.contains("a=d,d=i"));
        assert!(placements.is_empty());
    }
}
