//! Two-tier hybrid frame-generation hook system + FidelityFX input receiver + probe logger.
//!
//! Tier 1 (`RenderHookProfile`): Rust trait registry for title-specific pass hooks.
//! Tier 2 (`[dlss_fg.hooks]`): TOML / environment-variable matching rules plus a safe
//! reversed-Z Depth & Motion Vector heuristic and `--dlss-fg-probe` pass logger.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use victory_core::config::{
    CameraCbvRule, DlssFgMode, HookBinding, HookOccurrence, HookRole, HookRule,
};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};

use crate::host::{HostList, host, transition};
use crate::object::This;
use crate::resource::{HostTexture, host_copy};

/// Source of a captured texture for Streamline tagging.
#[derive(Clone)]
pub(crate) enum HookSource {
    /// Live host texture kept alive through Present.
    Live(Arc<HostTexture>),
    /// Immutable GPU copy captured via `CopyResource` at hook trigger time.
    Snapshot(ID3D12Resource, u32),
}

#[derive(Clone)]
pub(crate) struct HookedTexture {
    pub(crate) source: HookSource,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: u32,
    pub(crate) alpha_only: bool,
}

#[derive(Clone)]
pub struct CameraData {
    pub view_to_clip: Option<[[f32; 4]; 4]>,
    pub world_to_view: Option<[[f32; 4]; 4]>,
    pub prev_world_to_view: Option<[[f32; 4]; 4]>,
    pub clip_to_prev_clip: Option<[[f32; 4]; 4]>,
    pub jitter_offset: [f32; 2],
    pub pos: Option<[f32; 3]>,
    pub up: Option<[f32; 3]>,
    pub right: Option<[f32; 3]>,
    pub fwd: Option<[f32; 3]>,
    pub near: f32,
    pub far: f32,
    pub fov_rad: f32,
    pub aspect_ratio: f32,
    pub reset: bool,
    pub depth_inverted: Option<bool>,
    pub mvec_jittered: Option<bool>,
    pub mvec_scale_override: Option<[f32; 2]>,
}

impl Default for CameraData {
    fn default() -> Self {
        const INVALID: f32 = 3.40282346638528859811704183484516925440e38_f32;
        Self {
            view_to_clip: None,
            world_to_view: None,
            prev_world_to_view: None,
            clip_to_prev_clip: None,
            jitter_offset: [0.0, 0.0],
            pos: None,
            up: None,
            right: None,
            fwd: None,
            near: INVALID,
            far: INVALID,
            fov_rad: INVALID,
            aspect_ratio: INVALID,
            reset: false,
            depth_inverted: None,
            mvec_jittered: None,
            mvec_scale_override: None,
        }
    }
}

#[derive(Default, Clone)]
pub(crate) struct FrameInputs {
    pub(crate) depth: Option<HookedTexture>,
    pub(crate) motion_vectors: Option<HookedTexture>,
    pub(crate) hudless: Option<HookedTexture>,
    pub(crate) ui: Option<HookedTexture>,
    pub(crate) camera: CameraData,
    pub(crate) from_fidelityfx: bool,
}

#[derive(Clone)]
pub struct TargetInfo {
    pub title_resource: This,
    pub binding: HookBinding,
    pub slot: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

#[derive(Clone)]
pub struct BoundResourceInfo {
    pub stage: &'static str,
    pub binding: HookBinding,
    pub slot: u32,
    pub table_dword: u32,
    pub title_resource: This,
    pub cbv_address: u64,
    pub cbv_size: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

pub struct PassContext<'a> {
    pub compute: bool,
    pub shader_hashes: &'a [(&'static str, &'a str)],
    pub targets: &'a [TargetInfo],
    pub bound: &'a [BoundResourceInfo],
    pub(crate) list: &'a HostList,
}

/// Sink passed to Tier 1 `RenderHookProfile` callbacks.
pub struct FrameHookSink<'a> {
    list: &'a HostList,
    frame: &'a mut FrameInputs,
}

impl FrameHookSink<'_> {
    pub fn set_depth(&mut self, title_res: This, snapshot: bool) {
        if let Some(tex) = capture_title_texture(self.list, HookRole::Depth, title_res, snapshot, false) {
            self.frame.depth = Some(tex);
        }
    }

    pub fn set_motion_vectors(&mut self, title_res: This, snapshot: bool) {
        if let Some(tex) =
            capture_title_texture(self.list, HookRole::MotionVectors, title_res, snapshot, false)
        {
            self.frame.motion_vectors = Some(tex);
        }
    }

    pub fn set_hudless(&mut self, title_res: This, snapshot: bool) {
        if let Some(tex) =
            capture_title_texture(self.list, HookRole::Hudless, title_res, snapshot, false)
        {
            self.frame.hudless = Some(tex);
        }
    }

    pub fn set_ui(&mut self, title_res: This, snapshot: bool, alpha_only: bool) {
        if let Some(tex) =
            capture_title_texture(self.list, HookRole::Ui, title_res, snapshot, alpha_only)
        {
            self.frame.ui = Some(tex);
        }
    }

    pub fn camera_mut(&mut self) -> &mut CameraData {
        &mut self.frame.camera
    }
}

/// Tier 1 programmatic hook trait for title-specific pass recognition.
pub trait RenderHookProfile: Send + Sync {
    fn name(&self) -> &'static str;
    fn on_pass(&self, ctx: &PassContext<'_>, sink: &mut FrameHookSink<'_>);
}

static PROFILES: Mutex<Vec<Arc<dyn RenderHookProfile>>> = Mutex::new(Vec::new());
static HAS_PROFILES: AtomicBool = AtomicBool::new(false);

pub fn register_profile(profile: Arc<dyn RenderHookProfile>) {
    tracing::info!(profile = profile.name(), "registered DLSS-G RenderHookProfile");
    PROFILES.lock().unwrap_or_else(|e| e.into_inner()).push(profile);
    HAS_PROFILES.store(true, Ordering::Release);
}

pub(crate) fn active() -> bool {
    crate::dlss_fg::wanted() || HAS_PROFILES.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Reusable GPU snapshot pool (used for HUDless pre-UI targets and DENY_SRV Depth)
// ---------------------------------------------------------------------------

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct SnapshotKey {
    role: u8,
    width: u32,
    height: u32,
    format: i32,
}

static SNAPSHOTS: Mutex<Option<HashMap<SnapshotKey, ID3D12Resource>>> = Mutex::new(None);

fn snapshot_format(format: i32) -> i32 {
    // Ensure depth/sRGB formats copy cleanly into SRV-readable formats.
    match format {
        40 => 39, // D32_FLOAT -> R32_TYPELESS
        20 => 19, // D32_FLOAT_S8X24_UINT -> R32G8X24_TYPELESS
        45 => 44, // D24_UNORM_S8_UINT -> R24G8_TYPELESS
        55 => 53, // D16_UNORM -> R16_TYPELESS
        other => other,
    }
}

fn snapshot_copy(
    list: &HostList,
    role: HookRole,
    src_title: This,
    src_host: &Arc<HostTexture>,
) -> Option<(ID3D12Resource, u32, u32, u32)> {
    let host = host()?;
    // SAFETY: live host resource.
    let desc = unsafe { src_host.texture.GetDesc() };
    if desc.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D || desc.SampleDesc.Count > 1 {
        return None;
    }
    let w = desc.Width as u32;
    let h = desc.Height;
    let copy_fmt = snapshot_format(desc.Format.0);
    let key = SnapshotKey {
        role: role as u8,
        width: w,
        height: h,
        format: copy_fmt,
    };

    let dst = {
        let mut map = SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
        let map = map.get_or_insert_with(HashMap::new);
        match map.get(&key) {
            Some(existing) => existing.clone(),
            None => {
                let heap = D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                };
                let dst_desc = D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: u64::from(w),
                    Height: h,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT(copy_fmt),
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Flags: D3D12_RESOURCE_FLAG_NONE,
                    ..Default::default()
                };
                let mut res: Option<ID3D12Resource> = None;
                // SAFETY: plain committed texture creation for DLSS-G snapshot.
                let hr = unsafe {
                    host.device.CreateCommittedResource(
                        &heap,
                        D3D12_HEAP_FLAG_NONE,
                        &dst_desc,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                        None,
                        &mut res,
                    )
                };
                if hr.is_err() || res.is_none() {
                    return None;
                }
                let res = res?;
                crate::host::name(&res, || format!("dlssg snapshot {role:?} {w}x{h} f{copy_fmt}"));
                map.insert(key, res.clone());
                res
            }
        }
    };

    const COPY_SOURCE: u32 = 0x800;
    let prev = src_host.state.load(Ordering::Acquire);
    crate::list::ensure_state(list, src_title, COPY_SOURCE);
    // SAFETY: `list` is open; `dst` rests in NON_PIXEL_SHADER_RESOURCE between copies.
    unsafe {
        list.list.ResourceBarrier(&[transition(
            &dst,
            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            D3D12_RESOURCE_STATE_COPY_DEST,
        )]);
        list.list.CopyResource(&dst, &src_host.texture);
        list.list.ResourceBarrier(&[transition(
            &dst,
            D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
        )]);
    }
    crate::list::ensure_state(list, src_title, prev);
    list.keep(&dst);
    Some((dst, w, h, desc.Format.0 as u32))
}

fn capture_title_texture(
    list: &HostList,
    role: HookRole,
    title_res: This,
    force_snapshot: bool,
    alpha_only: bool,
) -> Option<HookedTexture> {
    let copy = host_copy(title_res)?;
    // SAFETY: live texture resource.
    let desc = unsafe { copy.texture.GetDesc() };
    if desc.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D || desc.SampleDesc.Count > 1 {
        return None;
    }
    let deny_srv = desc.Flags.0 & D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE.0 != 0;
    if force_snapshot || deny_srv {
        let (snap, w, h, fmt) = snapshot_copy(list, role, title_res, &copy)?;
        return Some(HookedTexture {
            source: HookSource::Snapshot(snap, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE.0 as u32),
            width: w,
            height: h,
            format: fmt,
            alpha_only,
        });
    }
    Some(HookedTexture {
        width: desc.Width as u32,
        height: desc.Height,
        format: desc.Format.0 as u32,
        source: HookSource::Live(copy),
        alpha_only,
    })
}

// ---------------------------------------------------------------------------
// Frame state, heuristic tracking, FidelityFX intake, and probe diagnostics
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FrameBuilder {
    inputs: FrameInputs,
    rule_hits: HashMap<usize, u32>,
    depth_score: u64,
    mvec_score: u64,
    probe_entries: HashMap<(u32, u32, u32, &'static str), (u32, String)>,
    probe_frames: u64,
}

static CURRENT_FRAME: Mutex<Option<FrameBuilder>> = Mutex::new(None);

fn with_frame<R>(f: impl FnOnce(&mut FrameBuilder) -> R) -> R {
    let mut guard = CURRENT_FRAME.lock().unwrap_or_else(|e| e.into_inner());
    let builder = guard.get_or_insert_with(FrameBuilder::default);
    f(builder)
}

/// Takes the captured inputs for the frame about to be presented, resetting per-frame trackers.
pub(crate) fn take_frame_inputs() -> FrameInputs {
    let cfg = crate::dlss_fg::config();
    with_frame(|b| {
        if cfg.hooks.probe {
            b.probe_frames += 1;
            if !b.probe_entries.is_empty() && (b.probe_frames == 1 || b.probe_frames.is_multiple_of(120)) {
                let mut rows: Vec<_> = b
                    .probe_entries
                    .iter()
                    .map(|(&(w, h, fmt, role), &(hits, ref shaders))| (hits, w, h, fmt, role, shaders.clone()))
                    .collect();
                rows.sort_by(|a, b| b.0.cmp(&a.0));
                rows.truncate(24);
                tracing::info!(
                    frame = b.probe_frames,
                    depth_found = b.inputs.depth.is_some(),
                    mvec_found = b.inputs.motion_vectors.is_some(),
                    hudless_found = b.inputs.hudless.is_some(),
                    from_ffx = b.inputs.from_fidelityfx,
                    "dlss_fg probe candidates (top {}):",
                    rows.len()
                );
                for (hits, w, h, fmt, role, shaders) in rows {
                    tracing::info!(role, hits, width = w, height = h, format = fmt, shaders = %shaders, "  probe");
                }
            }
            b.probe_entries.clear();
        }
        let inputs = std::mem::take(&mut b.inputs);
        b.rule_hits.clear();
        b.depth_score = 0;
        b.mvec_score = 0;
        inputs
    })
}

/// Called from `fidelityfx.rs` when `ffxCreateContext` / `ffxDispatch` executes on Series S.
pub fn record_fidelityfx_dispatch(
    list: *const std::ffi::c_void,
    depth_title: *const std::ffi::c_void,
    mvec_title: *const std::ffi::c_void,
    hudless_title: *const std::ffi::c_void,
    jitter: [f32; 2],
    mvec_scale: [f32; 2],
    render_size: [u32; 2],
    camera_near: f32,
    camera_far: f32,
    camera_fov_rad: f32,
    reset: bool,
    depth_inverted: bool,
) {
    if !active() {
        return;
    }
    let _ = crate::list::with_host(list.cast(), |host_list, _| {
        let depth = (!depth_title.is_null())
            .then(|| capture_title_texture(host_list, HookRole::Depth, depth_title.cast(), false, false))
            .flatten();
        let mvec = (!mvec_title.is_null())
            .then(|| capture_title_texture(host_list, HookRole::MotionVectors, mvec_title.cast(), false, false))
            .flatten();
        // FSR output may be reused or UI-composited on the same target; snapshot if available.
        let hudless = (!hudless_title.is_null())
            .then(|| capture_title_texture(host_list, HookRole::Hudless, hudless_title.cast(), true, false))
            .flatten();

        with_frame(|b| {
            b.inputs.from_fidelityfx = true;
            if depth.is_some() {
                b.inputs.depth = depth;
            }
            if mvec.is_some() {
                b.inputs.motion_vectors = mvec;
            }
            if hudless.is_some() {
                b.inputs.hudless = hudless;
            }
            b.inputs.camera.jitter_offset = jitter;
            if mvec_scale[0] != 0.0 && mvec_scale[1] != 0.0 && render_size[0] > 0 && render_size[1] > 0 {
                // FidelityFX motionVectorScale multiplies MV values to convert into pixel space.
                // Streamline `mvecScale` normalizes MV values into [-1, 1] UV space.
                b.inputs.camera.mvec_scale_override = Some([
                    mvec_scale[0] / render_size[0] as f32,
                    mvec_scale[1] / render_size[1] as f32,
                ]);
            }
            if camera_near.is_finite() && camera_near > 0.0 {
                b.inputs.camera.near = camera_near;
            }
            if camera_far.is_finite() && camera_far > 0.0 {
                b.inputs.camera.far = camera_far;
            }
            if camera_fov_rad.is_finite() && camera_fov_rad > 0.0 {
                b.inputs.camera.fov_rad = camera_fov_rad;
            }
            b.inputs.camera.reset |= reset;
            b.inputs.camera.depth_inverted = Some(depth_inverted);
        });
    });
}

fn parse_format_spec(spec: &str) -> Option<u32> {
    let trimmed = spec.trim();
    if let Ok(n) = trimmed.parse::<u32>() {
        return Some(n);
    }
    match trimmed.to_ascii_uppercase().as_str() {
        "R16G16B16A16_FLOAT" => Some(10),
        "R32G32_FLOAT" => Some(16),
        "R10G10B10A2_UNORM" => Some(24),
        "R11G11B10_FLOAT" => Some(26),
        "R8G8B8A8_UNORM" => Some(28),
        "R8G8B8A8_UNORM_SRGB" => Some(29),
        "R16G16_TYPELESS" => Some(33),
        "R16G16_FLOAT" => Some(34),
        "R32_TYPELESS" => Some(39),
        "D32_FLOAT" => Some(40),
        "R32_FLOAT" => Some(41),
        "R24G8_TYPELESS" => Some(44),
        "D24_UNORM_S8_UINT" => Some(45),
        "R32G8X24_TYPELESS" => Some(19),
        "D32_FLOAT_S8X24_UINT" => Some(20),
        "D16_UNORM" => Some(55),
        "R8_UNORM" => Some(61),
        "B8G8R8A8_UNORM" => Some(87),
        "B8G8R8A8_UNORM_SRGB" => Some(91),
        _ => None,
    }
}

fn read_mat4(buf: &[u8], offset: usize, row_major: bool) -> Option<[[f32; 4]; 4]> {
    let slice = buf.get(offset..offset + 64)?;
    let mut vals = [0.0f32; 16];
    for i in 0..16 {
        vals[i] = f32::from_le_bytes(slice[i * 4..i * 4 + 4].try_into().ok()?);
        if !vals[i].is_finite() {
            return None;
        }
    }
    let m = [
        [vals[0], vals[1], vals[2], vals[3]],
        [vals[4], vals[5], vals[6], vals[7]],
        [vals[8], vals[9], vals[10], vals[11]],
        [vals[12], vals[13], vals[14], vals[15]],
    ];
    if row_major {
        Some(m)
    } else {
        Some([
            [m[0][0], m[1][0], m[2][0], m[3][0]],
            [m[0][1], m[1][1], m[2][1], m[3][1]],
            [m[0][2], m[1][2], m[2][2], m[3][2]],
            [m[0][3], m[1][3], m[2][3], m[3][3]],
        ])
    }
}

fn apply_camera_cbv(rule: &CameraCbvRule, address: u64, size: u32, cam: &mut CameraData) {
    let len = (size as usize).clamp(64, 4096);
    let mut bytes = vec![0u8; len];
    if !crate::host::read_into(address as *const u8, &mut bytes) {
        return;
    }
    if let Some(off) = rule.proj_offset
        && let Some(m) = read_mat4(&bytes, off as usize, rule.row_major)
    {
        cam.view_to_clip = Some(m);
    }
    if let Some(off) = rule.view_offset
        && let Some(m) = read_mat4(&bytes, off as usize, rule.row_major)
    {
        cam.world_to_view = Some(m);
    }
    if let Some(off) = rule.view_proj_offset
        && let Some(vp) = read_mat4(&bytes, off as usize, rule.row_major)
        && cam.view_to_clip.is_none()
    {
        cam.view_to_clip = Some(vp);
    }
    if let Some(off) = rule.prev_view_proj_offset
        && let Some(pvp) = read_mat4(&bytes, off as usize, rule.row_major)
        && let Some(vp) = cam.view_to_clip
    {
        // clipToPrevClip = clipToWorld * prevWorldToPrevClip = vp^-1 * prev_vp
        let inv_vp = crate::streamline::invert_4x4(&vp);
        let mut c2pc = [[0.0f32; 4]; 4];
        for r in 0..4 {
            for c in 0..4 {
                c2pc[r][c] = inv_vp[r][0] * pvp[0][c]
                    + inv_vp[r][1] * pvp[1][c]
                    + inv_vp[r][2] * pvp[2][c]
                    + inv_vp[r][3] * pvp[3][c];
            }
        }
        cam.clip_to_prev_clip = Some(c2pc);
    }
    if let Some(off) = rule.jitter_offset
        && let Some(slice) = bytes.get(off as usize..off as usize + 8)
    {
        let x = f32::from_le_bytes(slice[0..4].try_into().unwrap_or([0; 4]));
        let y = f32::from_le_bytes(slice[4..8].try_into().unwrap_or([0; 4]));
        if x.is_finite() && y.is_finite() && x.abs() <= 1.0 && y.abs() <= 1.0 {
            cam.jitter_offset = [x, y];
        }
    }
    if let Some(off) = rule.camera_pos_offset
        && let Some(slice) = bytes.get(off as usize..off as usize + 12)
    {
        let x = f32::from_le_bytes(slice[0..4].try_into().unwrap_or([0; 4]));
        let y = f32::from_le_bytes(slice[4..8].try_into().unwrap_or([0; 4]));
        let z = f32::from_le_bytes(slice[8..12].try_into().unwrap_or([0; 4]));
        if x.is_finite() && y.is_finite() && z.is_finite() {
            cam.pos = Some([x, y, z]);
        }
    }
}

fn rule_matches_shader(rule: &HookRule, ctx: &PassContext<'_>) -> bool {
    let stage_ok = match &rule.stage {
        Some(st) => ctx.shader_hashes.iter().any(|(s, _)| s.eq_ignore_ascii_case(st)),
        None => true,
    };
    if !stage_ok {
        return false;
    }
    match &rule.shader_hash {
        Some(h) => ctx
            .shader_hashes
            .iter()
            .any(|(_, hash)| hash.eq_ignore_ascii_case(h.trim())),
        None => true,
    }
}

fn rule_matches_dim_fmt(rule: &HookRule, width: u32, height: u32, format: u32) -> bool {
    if let Some(w) = rule.width
        && width != w
    {
        return false;
    }
    if let Some(h) = rule.height
        && height != h
    {
        return false;
    }
    if let Some(fmt_spec) = &rule.format
        && parse_format_spec(fmt_spec) != Some(format)
    {
        return false;
    }
    true
}

fn should_take_occurrence(occ: HookOccurrence, hit_index: u32) -> bool {
    match occ {
        HookOccurrence::First => hit_index == 0,
        HookOccurrence::Last => true,
        HookOccurrence::Index(target) => hit_index == target,
    }
}

fn is_depth_format(fmt: u32) -> bool {
    matches!(fmt, 19 | 20 | 39 | 40 | 44 | 45 | 53 | 55)
}

fn is_mvec_format(fmt: u32) -> bool {
    // RG16_FLOAT (34), RG16_TYPELESS (33), RG32_FLOAT (16), RG32_TYPELESS (15)
    matches!(fmt, 34 | 33 | 16 | 15)
}

/// Evaluates Tier 1 profiles, Tier 2 explicit rules, heuristic matchers, and probe logging.
pub(crate) fn on_pass(ctx: &PassContext<'_>) {
    let cfg = crate::dlss_fg::config();
    if cfg.mode == DlssFgMode::Off && !cfg.hooks.probe && !HAS_PROFILES.load(Ordering::Relaxed) {
        return;
    }

    // Tier 1: programmatic profiles.
    if HAS_PROFILES.load(Ordering::Relaxed) {
        let profiles = PROFILES.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !profiles.is_empty() {
            with_frame(|b| {
                let mut sink = FrameHookSink {
                    list: ctx.list,
                    frame: &mut b.inputs,
                };
                for p in &profiles {
                    p.on_pass(ctx, &mut sink);
                }
            });
        }
    }

    let mut matched_depth_rule = false;
    let mut matched_mvec_rule = false;
    for r in &cfg.hooks.rules {
        match r.role {
            HookRole::Depth => matched_depth_rule = true,
            HookRole::MotionVectors => matched_mvec_rule = true,
            _ => {}
        }
    }

    // Tier 2A: explicit config rules.
    if !cfg.hooks.rules.is_empty() {
        for (idx, rule) in cfg.hooks.rules.iter().enumerate() {
            if !rule_matches_shader(rule, ctx) {
                continue;
            }
            if rule.role == HookRole::Camera {
                for bnd in ctx.bound {
                    if !matches!(rule.binding, HookBinding::Any | HookBinding::Cbv) {
                        continue;
                    }
                    if bnd.cbv_address == 0 {
                        continue;
                    }
                    if let Some(slot) = rule.slot
                        && bnd.slot != slot
                    {
                        continue;
                    }
                    if let Some(dw) = rule.table_dword
                        && bnd.table_dword != dw
                    {
                        continue;
                    }
                    let hit = with_frame(|b| {
                        let e = b.rule_hits.entry(idx).or_insert(0);
                        let cur = *e;
                        *e += 1;
                        if should_take_occurrence(rule.occurrence, cur) {
                            if let Some(cam_rule) = &rule.camera {
                                apply_camera_cbv(cam_rule, bnd.cbv_address, bnd.cbv_size, &mut b.inputs.camera);
                            }
                            true
                        } else {
                            false
                        }
                    });
                    if hit {
                        break;
                    }
                }
                continue;
            }

            // Target or bound texture match.
            let target_hit = ctx.targets.iter().find(|t| {
                (rule.binding == HookBinding::Any || rule.binding == t.binding)
                    && rule.slot.is_none_or(|s| t.slot == s)
                    && rule_matches_dim_fmt(rule, t.width, t.height, t.format)
            });
            let bound_hit = if target_hit.is_none() {
                ctx.bound.iter().find(|b| {
                    !b.title_resource.is_null()
                        && (rule.binding == HookBinding::Any || rule.binding == b.binding)
                        && rule.slot.is_none_or(|s| b.slot == s)
                        && rule.table_dword.is_none_or(|dw| b.table_dword == dw)
                        && rule_matches_dim_fmt(rule, b.width, b.height, b.format)
                })
            } else {
                None
            };

            let title_res = target_hit
                .map(|t| t.title_resource)
                .or_else(|| bound_hit.map(|b| b.title_resource));
            let Some(title_res) = title_res else {
                continue;
            };

            let take = with_frame(|b| {
                let e = b.rule_hits.entry(idx).or_insert(0);
                let cur = *e;
                *e += 1;
                should_take_occurrence(rule.occurrence, cur)
            });
            if !take {
                continue;
            }
            let snap = rule.snapshot.unwrap_or(matches!(rule.role, HookRole::Hudless));
            if let Some(tex) = capture_title_texture(ctx.list, rule.role, title_res, snap, false) {
                with_frame(|b| match rule.role {
                    HookRole::Depth => b.inputs.depth = Some(tex),
                    HookRole::MotionVectors => b.inputs.motion_vectors = Some(tex),
                    HookRole::Hudless => b.inputs.hudless = Some(tex),
                    HookRole::Ui => b.inputs.ui = Some(tex),
                    HookRole::Camera => {}
                });
            }
        }
    }

    // Tier 2B: RAGE / Series X heuristic (when FidelityFX didn't supply inputs and no explicit rule covers the role).
    if cfg.hooks.heuristic {
        let rw = cfg.hooks.render_width;
        let rh = cfg.hooks.render_height;
        let prefer_render = |w: u32, h: u32| -> u64 {
            let area = u64::from(w) * u64::from(h);
            match (rw, rh) {
                (Some(ew), Some(eh)) if w == ew && h == eh => area.saturating_mul(8),
                _ => area,
            }
        };

        // Candidate Depth: DepthTarget >= 640x360 with depth format.
        if !matched_depth_rule {
            if let Some(dt) = ctx
                .targets
                .iter()
                .find(|t| t.binding == HookBinding::DepthTarget && t.width >= 640 && t.height >= 360 && is_depth_format(t.format))
            {
                let score = prefer_render(dt.width, dt.height);
                let should_capture = with_frame(|b| {
                    !b.inputs.from_fidelityfx && score >= b.depth_score
                });
                if should_capture
                    && let Some(tex) = capture_title_texture(ctx.list, HookRole::Depth, dt.title_resource, false, false)
                {
                    with_frame(|b| {
                        if !b.inputs.from_fidelityfx && score >= b.depth_score {
                            b.depth_score = score;
                            b.inputs.depth = Some(tex);
                        }
                    });
                }
            }
        }

        // Candidate MotionVectors: RenderTarget or UAV >= 640x360 in RG16_FLOAT / RG32_FLOAT.
        if !matched_mvec_rule {
            let mv_target = ctx
                .targets
                .iter()
                .find(|t| t.binding == HookBinding::RenderTarget && t.width >= 640 && t.height >= 360 && is_mvec_format(t.format))
                .map(|t| (t.title_resource, t.width, t.height));
            let mv_uav = if mv_target.is_none() {
                ctx.bound
                    .iter()
                    .find(|b| b.binding == HookBinding::Uav && !b.title_resource.is_null() && b.width >= 640 && b.height >= 360 && is_mvec_format(b.format))
                    .map(|b| (b.title_resource, b.width, b.height))
            } else {
                None
            };
            if let Some((mv_res, w, h)) = mv_target.or(mv_uav) {
                let score = prefer_render(w, h);
                let should_capture = with_frame(|b| {
                    if b.inputs.from_fidelityfx {
                        return false;
                    }
                    let dim_matches_depth = b
                        .inputs
                        .depth
                        .as_ref()
                        .is_none_or(|d| d.width == w && d.height == h);
                    dim_matches_depth && score >= b.mvec_score
                });
                if should_capture
                    && let Some(tex) = capture_title_texture(ctx.list, HookRole::MotionVectors, mv_res, false, false)
                {
                    with_frame(|b| {
                        if !b.inputs.from_fidelityfx && score >= b.mvec_score {
                            b.mvec_score = score;
                            b.inputs.motion_vectors = Some(tex);
                        }
                    });
                }
            }
        }
    }

    // Probe mode: record candidate passes/formats/shaders for offline rule authoring.
    if cfg.hooks.probe {
        let shaders_label = ctx
            .shader_hashes
            .iter()
            .map(|(st, h)| format!("{st}:{h}"))
            .collect::<Vec<_>>()
            .join("+");
        with_frame(|b| {
            for t in ctx.targets {
                if t.width >= 640 && t.height >= 360 {
                    let role = if t.binding == HookBinding::DepthTarget {
                        "depth_target"
                    } else if is_mvec_format(t.format) {
                        "mvec_rt_candidate"
                    } else {
                        "render_target"
                    };
                    let entry = b
                        .probe_entries
                        .entry((t.width, t.height, t.format, role))
                        .or_insert_with(|| (0, shaders_label.clone()));
                    entry.0 += 1;
                }
            }
            for r in ctx.bound {
                if r.binding == HookBinding::Uav && r.width >= 640 && r.height >= 360 && is_mvec_format(r.format) {
                    let entry = b
                        .probe_entries
                        .entry((r.width, r.height, r.format, "mvec_uav_candidate"))
                        .or_insert_with(|| (0, shaders_label.clone()));
                    entry.0 += 1;
                }
            }
        });
    }
}