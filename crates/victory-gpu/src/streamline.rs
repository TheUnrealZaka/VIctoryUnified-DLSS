//! Dynamic Streamline runtime integration (`sl.interposer.dll` + `sl.dlss_g` + `sl.reflex` + `sl.pcl`).
//!
//! Uses manual hooking (`PreferenceFlags::eUseManualHooking`) so `victory-gpu`'s
//! multi-threaded command-list recording and FidelityFX forwarding run on native
//! D3D12 objects without interposer overhead, while `IDXGISwapChain3` and the
//! swapchain's direct `ID3D12CommandQueue` use Streamline proxies when DLSS-G is active.

#![allow(non_snake_case)]
#![allow(dead_code)]

use std::ffi::{CStr, CString, c_char, c_void};
use std::mem::offset_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use victory_core::config::{DlssFgConfig, DlssFgMode, MvecUnits, SlRuntimeMode};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
};

use crate::hooks::{FrameInputs, HookedTexture};
use crate::resource::HostTexture;

// ---------------------------------------------------------------------------
// Streamline C ABI constants & structures (SDK 2.14.16)
// ---------------------------------------------------------------------------

const K_SDK_VERSION: u64 = (2u64 << 48) | (14u64 << 32) | (16u64 << 16) | 0xfedc;

const K_FEATURE_REFLEX: u32 = 3;
const K_FEATURE_PCL: u32 = 4;
const K_FEATURE_DLSS_G: u32 = 1000;
const K_FEATURE_COMMON: u32 = u32::MAX;

const PREF_DISABLE_CL_STATE_TRACKING: u64 = 1 << 0;
const PREF_DISABLE_DEBUG_TEXT: u64 = 1 << 1;
const PREF_USE_MANUAL_HOOKING: u64 = 1 << 2;
const PREF_USE_FRAME_BASED_TAGGING: u64 = 1 << 7;

const LOG_LEVEL_OFF: u32 = 0;
const LOG_LEVEL_DEFAULT: u32 = 1;
const LOG_LEVEL_VERBOSE: u32 = 2;

const ENGINE_CUSTOM: u32 = 0;
const RENDER_API_D3D12: u32 = 1;

const RESOURCE_TYPE_TEX2D: u8 = 0;
const LIFECYCLE_ONLY_VALID_NOW: u32 = 0;
const LIFECYCLE_VALID_UNTIL_PRESENT: u32 = 1;

const BUFFER_DEPTH: u32 = 0;
const BUFFER_MOTION_VECTORS: u32 = 1;
const BUFFER_HUDLESS_COLOR: u32 = 2;
const BUFFER_UI_COLOR_AND_ALPHA: u32 = 23;
const BUFFER_BACKBUFFER: u32 = 53;
const BUFFER_UI_ALPHA: u32 = 69;

const BOOL_FALSE: u8 = 0;
const BOOL_TRUE: u8 = 1;
const BOOL_INVALID: u8 = 2;

const DLSSG_MODE_OFF: u32 = 0;
const DLSSG_MODE_ON: u32 = 1;
const DLSSG_MODE_AUTO: u32 = 2;
const DLSSG_MODE_DYNAMIC: u32 = 3;

const DLSSG_FLAG_DYNAMIC_RESOLUTION: u32 = 1 << 1;
const DLSSG_FLAG_REQUEST_VRAM_ESTIMATE: u32 = 1 << 2;
const DLSSG_FLAG_RETAIN_RESOURCES_WHEN_OFF: u32 = 1 << 3;
const DLSSG_FLAG_ENABLE_FULLSCREEN_MENU_DETECTION: u32 = 1 << 4;

const DLSSG_STATUS_OK: u32 = 0;
const DLSSG_STATUS_FAIL_RESOLUTION_TOO_LOW: u32 = 1 << 0;
const DLSSG_STATUS_FAIL_REFLEX_NOT_DETECTED: u32 = 1 << 1;
const DLSSG_STATUS_FAIL_HDR_NOT_SUPPORTED: u32 = 1 << 2;
const DLSSG_STATUS_FAIL_COMMON_CONSTANTS_INVALID: u32 = 1 << 3;
const DLSSG_STATUS_FAIL_GET_CURRENT_BACKBUFFER_NOT_CALLED: u32 = 1 << 4;

const REFLEX_OFF: i32 = 0;
const REFLEX_LOW_LATENCY: i32 = 1;
const REFLEX_LOW_LATENCY_BOOST: i32 = 2;

const PCL_SIMULATION_START: u32 = 0;
const PCL_SIMULATION_END: u32 = 1;
const PCL_RENDER_SUBMIT_START: u32 = 2;
const PCL_RENDER_SUBMIT_END: u32 = 3;
const PCL_PRESENT_START: u32 = 4;
const PCL_PRESENT_END: u32 = 5;

const INVALID_FLOAT: f32 = 3.40282346638528859811704183484516925440e38_f32;

const SL_OK: i32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct StructType {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

impl StructType {
    const fn new(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Self {
        Self {
            data1: d1,
            data2: d2,
            data3: d3,
            data4: d4,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BaseStructure {
    next: *mut BaseStructure,
    struct_type: StructType,
    struct_version: usize,
}

impl BaseStructure {
    const fn new(struct_type: StructType, version: usize) -> Self {
        Self {
            next: std::ptr::null_mut(),
            struct_type,
            struct_version: version,
        }
    }
}

const TYPE_PREFERENCES: StructType = StructType::new(
    0x1ca10965,
    0xbf8e,
    0x432b,
    [0x8d, 0xa1, 0x67, 0x16, 0xd8, 0x79, 0xfb, 0x14],
);
const TYPE_VIEWPORT_HANDLE: StructType = StructType::new(
    0x171b6435,
    0x9b3c,
    0x4fc8,
    [0x99, 0x94, 0xfb, 0xe5, 0x25, 0x69, 0xaa, 0xa4],
);
const TYPE_FEATURE_REQUIREMENTS: StructType = StructType::new(
    0x66714097,
    0xac6d,
    0x4bc6,
    [0x89, 0x15, 0x1e, 0x0f, 0x55, 0xa6, 0xb6, 0x1f],
);
const TYPE_FEATURE_VERSION: StructType = StructType::new(
    0x6d5b51f0,
    0x076b,
    0x486d,
    [0x99, 0x95, 0x5a, 0x56, 0x10, 0x43, 0xf5, 0xc1],
);
const TYPE_ADAPTER_INFO: StructType = StructType::new(
    0x0677315f,
    0xa746,
    0x4492,
    [0x9f, 0x42, 0xcb, 0x61, 0x42, 0xc9, 0xc3, 0xd4],
);
const TYPE_RESOURCE: StructType = StructType::new(
    0x3a9d70cf,
    0x2418,
    0x4b72,
    [0x83, 0x91, 0x13, 0xf8, 0x72, 0x1c, 0x72, 0x61],
);
const TYPE_RESOURCE_TAG: StructType = StructType::new(
    0x4c6a5aad,
    0xb445,
    0x496c,
    [0x87, 0xff, 0x1a, 0xf3, 0x84, 0x5b, 0xe6, 0x53],
);
const TYPE_CONSTANTS: StructType = StructType::new(
    0xdcd35ad7,
    0x4e4a,
    0x4bad,
    [0xa9, 0x0c, 0xe0, 0xc4, 0x9e, 0xb2, 0x3a, 0xfe],
);
const TYPE_DLSSG_OPTIONS: StructType = StructType::new(
    0xfac5f1cb,
    0x2dfd,
    0x4f36,
    [0xa1, 0xe6, 0x3a, 0x9e, 0x86, 0x52, 0x56, 0xc5],
);
const TYPE_DLSSG_STATE: StructType = StructType::new(
    0xcc8ac8e1,
    0xa179,
    0x44f5,
    [0x97, 0xfa, 0xe7, 0x41, 0x12, 0xf9, 0xbc, 0x61],
);
const TYPE_REFLEX_OPTIONS: StructType = StructType::new(
    0xf03af81a,
    0x6d0b,
    0x4902,
    [0xa6, 0x51, 0xc4, 0x96, 0x5e, 0x21, 0x54, 0x34],
);
const TYPE_REFLEX_REPORT: StructType = StructType::new(
    0x0d569b37,
    0xa1c8,
    0x4453,
    [0xbe, 0x4d, 0x40, 0xf4, 0xde, 0x57, 0x95, 0x2b],
);
const TYPE_REFLEX_REPORT2: StructType = StructType::new(
    0x68bb0632,
    0x5e1c,
    0x402b,
    [0x89, 0x9d, 0xb4, 0x9f, 0x63, 0x3c, 0x56, 0xc2],
);
const TYPE_REFLEX_STATE: StructType = StructType::new(
    0xf0bb5985,
    0xdaf9,
    0x4728,
    [0xb2, 0xfd, 0xae, 0x80, 0xa2, 0xbd, 0x79, 0x89],
);

#[repr(C)]
struct Preferences {
    base: BaseStructure,
    show_console: bool,
    log_level: u32,
    paths_to_plugins: *const *const u16,
    num_paths_to_plugins: u32,
    path_to_logs_and_data: *const u16,
    allocate_callback: *const c_void,
    release_callback: *const c_void,
    log_message_callback: Option<unsafe extern "C" fn(u32, *const c_char)>,
    flags: u64,
    features_to_load: *const u32,
    num_features_to_load: u32,
    application_id: u32,
    engine: u32,
    engine_version: *const c_char,
    project_id: *const c_char,
    render_api: u32,
}

#[repr(C)]
struct ViewportHandle {
    base: BaseStructure,
    value: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SlVersion {
    major: u32,
    minor: u32,
    build: u32,
}

#[repr(C)]
struct FeatureRequirements {
    base: BaseStructure,
    flags: u32,
    max_num_cpu_threads: u32,
    max_num_viewports: u32,
    num_required_tags: u32,
    required_tags: *const u32,
    os_version_detected: SlVersion,
    os_version_required: SlVersion,
    driver_version_detected: SlVersion,
    driver_version_required: SlVersion,
    vk_num_compute_queues_required: u32,
    vk_num_graphics_queues_required: u32,
    vk_num_device_extensions: u32,
    vk_device_extensions: *const *const c_char,
    vk_num_instance_extensions: u32,
    vk_instance_extensions: *const *const c_char,
    vk_num_features12: u32,
    vk_features12: *const *const c_char,
    vk_num_features13: u32,
    vk_features13: *const *const c_char,
    vk_num_optical_flow_queues_required: u32,
}

impl Default for FeatureRequirements {
    fn default() -> Self {
        Self {
            base: BaseStructure::new(TYPE_FEATURE_REQUIREMENTS, 2),
            flags: 0,
            max_num_cpu_threads: 0,
            max_num_viewports: 0,
            num_required_tags: 0,
            required_tags: std::ptr::null(),
            os_version_detected: SlVersion::default(),
            os_version_required: SlVersion::default(),
            driver_version_detected: SlVersion::default(),
            driver_version_required: SlVersion::default(),
            vk_num_compute_queues_required: 0,
            vk_num_graphics_queues_required: 0,
            vk_num_device_extensions: 0,
            vk_device_extensions: std::ptr::null(),
            vk_num_instance_extensions: 0,
            vk_instance_extensions: std::ptr::null(),
            vk_num_features12: 0,
            vk_features12: std::ptr::null(),
            vk_num_features13: 0,
            vk_features13: std::ptr::null(),
            vk_num_optical_flow_queues_required: 0,
        }
    }
}

#[repr(C)]
struct FeatureVersion {
    base: BaseStructure,
    version_sl: SlVersion,
    version_ngx: SlVersion,
}

impl Default for FeatureVersion {
    fn default() -> Self {
        Self {
            base: BaseStructure::new(TYPE_FEATURE_VERSION, 1),
            version_sl: SlVersion::default(),
            version_ngx: SlVersion::default(),
        }
    }
}

#[repr(C)]
struct AdapterInfo {
    base: BaseStructure,
    device_luid: *mut u8,
    device_luid_size_in_bytes: u32,
    vk_physical_device: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Extent {
    top: u32,
    left: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
struct SlResource {
    base: BaseStructure,
    kind: u8,
    native: *mut c_void,
    memory: *mut c_void,
    view: *mut c_void,
    state: u32,
    width: u32,
    height: u32,
    native_format: u32,
    mip_levels: u32,
    array_layers: u32,
    gpu_virtual_address: u64,
    flags: u32,
    usage: u32,
    internal_flags: u16,
    reserved: u16,
}

impl SlResource {
    fn tex2d(native: *mut c_void, state: u32, width: u32, height: u32, format: u32) -> Self {
        Self {
            base: BaseStructure::new(TYPE_RESOURCE, 1),
            kind: RESOURCE_TYPE_TEX2D,
            native,
            memory: std::ptr::null_mut(),
            view: std::ptr::null_mut(),
            state,
            width,
            height,
            native_format: format,
            mip_levels: 1,
            array_layers: 1,
            gpu_virtual_address: 0,
            flags: 0,
            usage: 0,
            internal_flags: 0,
            reserved: 0,
        }
    }
}

#[repr(C)]
struct ResourceTag {
    base: BaseStructure,
    resource: *mut SlResource,
    kind: u32,
    lifecycle: u32,
    extent: Extent,
}

impl ResourceTag {
    fn new(resource: *mut SlResource, kind: u32, lifecycle: u32, extent: Extent) -> Self {
        Self {
            base: BaseStructure::new(TYPE_RESOURCE_TAG, 1),
            resource,
            kind,
            lifecycle,
            extent,
        }
    }
}

#[repr(C)]
struct Constants {
    base: BaseStructure,
    camera_view_to_clip: [[f32; 4]; 4],
    clip_to_camera_view: [[f32; 4]; 4],
    clip_to_lens_clip: [[f32; 4]; 4],
    clip_to_prev_clip: [[f32; 4]; 4],
    prev_clip_to_clip: [[f32; 4]; 4],
    jitter_offset: [f32; 2],
    mvec_scale: [f32; 2],
    camera_pinhole_offset: [f32; 2],
    camera_pos: [f32; 3],
    camera_up: [f32; 3],
    camera_right: [f32; 3],
    camera_fwd: [f32; 3],
    camera_near: f32,
    camera_far: f32,
    camera_fov: f32,
    camera_aspect_ratio: f32,
    motion_vectors_invalid_value: f32,
    depth_inverted: u8,
    camera_motion_included: u8,
    motion_vectors_3d: u8,
    reset: u8,
    orthographic_projection: u8,
    motion_vectors_dilated: u8,
    motion_vectors_jittered: u8,
    min_relative_linear_depth_object_separation: f32,
}

impl Default for Constants {
    fn default() -> Self {
        let id = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        Self {
            base: BaseStructure::new(TYPE_CONSTANTS, 2),
            camera_view_to_clip: id,
            clip_to_camera_view: id,
            clip_to_lens_clip: id,
            clip_to_prev_clip: id,
            prev_clip_to_clip: id,
            jitter_offset: [0.0, 0.0],
            mvec_scale: [1.0, 1.0],
            camera_pinhole_offset: [0.0, 0.0],
            camera_pos: [0.0, 0.0, 0.0],
            camera_up: [0.0, 1.0, 0.0],
            camera_right: [1.0, 0.0, 0.0],
            camera_fwd: [0.0, 0.0, 1.0],
            camera_near: INVALID_FLOAT,
            camera_far: INVALID_FLOAT,
            camera_fov: INVALID_FLOAT,
            camera_aspect_ratio: INVALID_FLOAT,
            motion_vectors_invalid_value: INVALID_FLOAT,
            depth_inverted: BOOL_INVALID,
            camera_motion_included: BOOL_INVALID,
            motion_vectors_3d: BOOL_FALSE,
            reset: BOOL_FALSE,
            orthographic_projection: BOOL_FALSE,
            motion_vectors_dilated: BOOL_FALSE,
            motion_vectors_jittered: BOOL_FALSE,
            min_relative_linear_depth_object_separation: 40.0,
        }
    }
}

#[repr(C)]
struct APIError {
    code: i32,
}

#[repr(C)]
struct DLSSGOptions {
    base: BaseStructure,
    mode: u32,
    num_frames_to_generate: u32,
    flags: u32,
    dynamic_res_width: u32,
    dynamic_res_height: u32,
    num_back_buffers: u32,
    mvec_depth_width: u32,
    mvec_depth_height: u32,
    color_width: u32,
    color_height: u32,
    color_buffer_format: u32,
    mvec_buffer_format: u32,
    depth_buffer_format: u32,
    hud_less_buffer_format: u32,
    ui_buffer_format: u32,
    on_error_callback: Option<unsafe extern "C" fn(*const APIError)>,
    b_reserved_15: u8,
    queue_parallelism_mode: u32,
    enable_user_interface_recomposition: u8,
    dynamic_target_frame_rate: f32,
}

impl Default for DLSSGOptions {
    fn default() -> Self {
        Self {
            base: BaseStructure::new(TYPE_DLSSG_OPTIONS, 5),
            mode: DLSSG_MODE_OFF,
            num_frames_to_generate: 1,
            flags: 0,
            dynamic_res_width: 0,
            dynamic_res_height: 0,
            num_back_buffers: 0,
            mvec_depth_width: 0,
            mvec_depth_height: 0,
            color_width: 0,
            color_height: 0,
            color_buffer_format: 0,
            mvec_buffer_format: 0,
            depth_buffer_format: 0,
            hud_less_buffer_format: 0,
            ui_buffer_format: 0,
            on_error_callback: None,
            b_reserved_15: BOOL_INVALID,
            queue_parallelism_mode: 0,
            enable_user_interface_recomposition: BOOL_FALSE,
            dynamic_target_frame_rate: 0.0,
        }
    }
}

#[repr(C)]
struct DLSSGState {
    base: BaseStructure,
    estimated_vram_usage_in_bytes: u64,
    status: u32,
    min_width_or_height: u32,
    num_frames_actually_presented: u32,
    num_frames_to_generate_max: u32,
    b_reserved_4: u8,
    b_is_vsync_support_available: u8,
    inputs_processing_completion_fence: *mut c_void,
    last_present_inputs_processing_completion_fence_value: u64,
    b_is_dynamic_mfg_supported: u8,
}

impl Default for DLSSGState {
    fn default() -> Self {
        Self {
            base: BaseStructure::new(TYPE_DLSSG_STATE, 4),
            estimated_vram_usage_in_bytes: 0,
            status: 0,
            min_width_or_height: 0,
            num_frames_actually_presented: 0,
            num_frames_to_generate_max: 0,
            b_reserved_4: BOOL_INVALID,
            b_is_vsync_support_available: BOOL_INVALID,
            inputs_processing_completion_fence: std::ptr::null_mut(),
            last_present_inputs_processing_completion_fence_value: 0,
            b_is_dynamic_mfg_supported: BOOL_INVALID,
        }
    }
}

#[repr(C)]
struct ReflexOptions {
    base: BaseStructure,
    mode: i32,
    frame_limit_us: u32,
    use_markers_to_optimize: bool,
    virtual_key: u16,
    id_thread: u32,
}

impl Default for ReflexOptions {
    fn default() -> Self {
        Self {
            base: BaseStructure::new(TYPE_REFLEX_OPTIONS, 1),
            mode: REFLEX_OFF,
            frame_limit_us: 0,
            use_markers_to_optimize: false,
            virtual_key: 0,
            id_thread: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ReflexReport {
    base: BaseStructure,
    frame_id: u64,
    input_sample_time: u64,
    sim_start_time: u64,
    sim_end_time: u64,
    render_submit_start_time: u64,
    render_submit_end_time: u64,
    present_start_time: u64,
    present_end_time: u64,
    driver_start_time: u64,
    driver_end_time: u64,
    os_render_queue_start_time: u64,
    os_render_queue_end_time: u64,
    gpu_render_start_time: u64,
    gpu_render_end_time: u64,
    gpu_active_render_time_us: u32,
    gpu_frame_time_us: u32,
}

impl Default for ReflexReport {
    fn default() -> Self {
        Self {
            base: BaseStructure::new(TYPE_REFLEX_REPORT, 1),
            frame_id: 0,
            input_sample_time: 0,
            sim_start_time: 0,
            sim_end_time: 0,
            render_submit_start_time: 0,
            render_submit_end_time: 0,
            present_start_time: 0,
            present_end_time: 0,
            driver_start_time: 0,
            driver_end_time: 0,
            os_render_queue_start_time: 0,
            os_render_queue_end_time: 0,
            gpu_render_start_time: 0,
            gpu_render_end_time: 0,
            gpu_active_render_time_us: 0,
            gpu_frame_time_us: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ReflexReport2 {
    base: BaseStructure,
    camera_constructed_time: u64,
    cross_adapter_copy_time_us: u32,
}

impl Default for ReflexReport2 {
    fn default() -> Self {
        Self {
            base: BaseStructure::new(TYPE_REFLEX_REPORT2, 1),
            camera_constructed_time: 0,
            cross_adapter_copy_time_us: 0,
        }
    }
}

#[repr(C)]
struct ReflexState {
    base: BaseStructure,
    low_latency_available: bool,
    latency_report_available: bool,
    stats_window_message: u32,
    frame_report: [ReflexReport; 64],
    flash_indicator_driver_controlled: bool,
    frame_report2: [ReflexReport2; 64],
}

impl Default for ReflexState {
    fn default() -> Self {
        Self {
            base: BaseStructure::new(TYPE_REFLEX_STATE, 2),
            low_latency_available: false,
            latency_report_available: false,
            stats_window_message: 0,
            frame_report: [ReflexReport::default(); 64],
            flash_indicator_driver_controlled: false,
            frame_report2: [ReflexReport2::default(); 64],
        }
    }
}

// Compile-time ABI checks matching `source/core/sl.api/sl.cpp` static_asserts.
const _: () = assert!(size_of::<BaseStructure>() == 32);
const _: () = assert!(offset_of!(Preferences, render_api) == 136);
const _: () = assert!(offset_of!(ResourceTag, extent) == 48);
const _: () = assert!(offset_of!(SlResource, internal_flags) == 104);
const _: () = assert!(offset_of!(SlResource, reserved) == 106);
const _: () = assert!(offset_of!(Constants, motion_vectors_jittered) == 450);
const _: () = assert!(offset_of!(AdapterInfo, vk_physical_device) == 48);
const _: () = assert!(offset_of!(FeatureVersion, version_ngx) == 44);
const _: () = assert!(offset_of!(FeatureRequirements, vk_num_optical_flow_queues_required) == 176);
const _: () = assert!(size_of::<DLSSGOptions>() == 120);
const _: () = assert!(size_of::<DLSSGState>() == 88);
const _: () = assert!(size_of::<ReflexOptions>() == 48);
const _: () = assert!(size_of::<ReflexReport>() == 152);
const _: () = assert!(size_of::<ReflexReport2>() == 48);
const _: () = assert!(size_of::<ReflexState>() == 12848);

#[repr(C)]
struct FrameToken {
    _opaque: [u8; 0],
}

// Function signatures exported by `sl.interposer.dll` and feature plugins.
type FnSlInit = unsafe extern "C" fn(*const Preferences, u64) -> i32;
type FnSlShutdown = unsafe extern "C" fn() -> i32;
type FnSlIsFeatureSupported = unsafe extern "C" fn(u32, *const AdapterInfo) -> i32;
type FnSlIsFeatureLoaded = unsafe extern "C" fn(u32, *mut bool) -> i32;
type FnSlSetFeatureLoaded = unsafe extern "C" fn(u32, bool) -> i32;
type FnSlSetTagForFrame =
    unsafe extern "C" fn(*const FrameToken, *const ViewportHandle, *const ResourceTag, u32, *mut c_void) -> i32;
type FnSlSetConstants =
    unsafe extern "C" fn(*const Constants, *const FrameToken, *const ViewportHandle) -> i32;
type FnSlGetFeatureRequirements = unsafe extern "C" fn(u32, *mut FeatureRequirements) -> i32;
type FnSlGetFeatureVersion = unsafe extern "C" fn(u32, *mut FeatureVersion) -> i32;
type FnSlUpgradeInterface = unsafe extern "C" fn(*mut *mut c_void) -> i32;
type FnSlGetNativeInterface = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;
type FnSlGetFeatureFunction = unsafe extern "C" fn(u32, *const c_char, *mut *mut c_void) -> i32;
type FnSlGetNewFrameToken = unsafe extern "C" fn(*mut *mut FrameToken, *const u32) -> i32;
type FnSlSetD3DDevice = unsafe extern "C" fn(*mut c_void) -> i32;
type FnSlFreeResources = unsafe extern "C" fn(u32, *const ViewportHandle) -> i32;

type FnDlssgGetState =
    unsafe extern "C" fn(*const ViewportHandle, *mut DLSSGState, *const DLSSGOptions) -> i32;
type FnDlssgSetOptions = unsafe extern "C" fn(*const ViewportHandle, *const DLSSGOptions) -> i32;

type FnReflexGetState = unsafe extern "C" fn(*mut ReflexState) -> i32;
type FnReflexSleep = unsafe extern "C" fn(*const FrameToken) -> i32;
type FnReflexSetOptions = unsafe extern "C" fn(*const ReflexOptions) -> i32;
type FnPclSetMarker = unsafe extern "C" fn(u32, *const FrameToken) -> i32;

struct CoreApi {
    module: usize,
    init: FnSlInit,
    shutdown: FnSlShutdown,
    is_feature_supported: FnSlIsFeatureSupported,
    is_feature_loaded: FnSlIsFeatureLoaded,
    set_feature_loaded: FnSlSetFeatureLoaded,
    set_tag_for_frame: FnSlSetTagForFrame,
    set_constants: FnSlSetConstants,
    get_feature_requirements: FnSlGetFeatureRequirements,
    get_feature_version: FnSlGetFeatureVersion,
    upgrade_interface: FnSlUpgradeInterface,
    get_native_interface: FnSlGetNativeInterface,
    get_feature_function: FnSlGetFeatureFunction,
    get_new_frame_token: FnSlGetNewFrameToken,
    set_d3d_device: FnSlSetD3DDevice,
    free_resources: FnSlFreeResources,
}

// SAFETY: loaded DLL address and static function pointers.
unsafe impl Send for CoreApi {}
unsafe impl Sync for CoreApi {}

#[derive(Clone, Copy)]
struct FeatureApi {
    dlssg_get_state: FnDlssgGetState,
    dlssg_set_options: FnDlssgSetOptions,
    reflex_get_state: Option<FnReflexGetState>,
    reflex_sleep: Option<FnReflexSleep>,
    reflex_set_options: Option<FnReflexSetOptions>,
    pcl_set_marker: Option<FnPclSetMarker>,
}

fn result_str(r: i32) -> &'static str {
    match r {
        0 => "eOk",
        1 => "eErrorIO",
        2 => "eErrorDriverOutOfDate",
        3 => "eErrorOSOutOfDate",
        4 => "eErrorOSDisabledHWS",
        5 => "eErrorDeviceNotCreated",
        6 => "eErrorNoSupportedAdapterFound",
        7 => "eErrorAdapterNotSupported",
        8 => "eErrorNoPlugins",
        9 => "eErrorVulkanAPI",
        10 => "eErrorDXGIAPI",
        11 => "eErrorD3DAPI",
        12 => "eErrorNRDAPI",
        13 => "eErrorNVAPI",
        14 => "eErrorReflexAPI",
        15 => "eErrorNGXFailed",
        16 => "eErrorJSONParsing",
        17 => "eErrorMissingProxy",
        18 => "eErrorMissingResourceState",
        19 => "eErrorInvalidIntegration",
        20 => "eErrorMissingInputParameter",
        21 => "eErrorNotInitialized",
        22 => "eErrorComputeFailed",
        23 => "eErrorInitNotCalled",
        24 => "eErrorExceptionHandler",
        25 => "eErrorInvalidParameter",
        26 => "eErrorMissingConstants",
        27 => "eErrorDuplicatedConstants",
        28 => "eErrorMissingOrInvalidAPI",
        29 => "eErrorCommonConstantsMissing",
        30 => "eErrorUnsupportedInterface",
        31 => "eErrorFeatureMissing",
        32 => "eErrorFeatureNotSupported",
        33 => "eErrorFeatureMissingHooks",
        34 => "eErrorFeatureFailedToLoad",
        35 => "eErrorFeatureWrongPriority",
        36 => "eErrorFeatureMissingDependency",
        37 => "eErrorFeatureManagerInvalidState",
        38 => "eErrorInvalidState",
        39 => "eWarnOutOfVRAM",
        _ => "Unknown",
    }
}

fn dlssg_status_str(status: u32) -> String {
    if status == DLSSG_STATUS_OK {
        return "eOk".to_owned();
    }
    let mut parts = Vec::new();
    for (bit, name) in [
        (DLSSG_STATUS_FAIL_RESOLUTION_TOO_LOW, "ResolutionTooLow"),
        (DLSSG_STATUS_FAIL_REFLEX_NOT_DETECTED, "ReflexNotDetectedAtRuntime"),
        (DLSSG_STATUS_FAIL_HDR_NOT_SUPPORTED, "HDRFormatNotSupported"),
        (DLSSG_STATUS_FAIL_COMMON_CONSTANTS_INVALID, "CommonConstantsInvalid"),
        (DLSSG_STATUS_FAIL_GET_CURRENT_BACKBUFFER_NOT_CALLED, "GetCurrentBackBufferIndexNotCalled"),
        (1 << 5, "Reserved5"),
    ] {
        if status & bit != 0 {
            parts.push(name);
        }
    }
    if parts.is_empty() {
        format!("{status:#x}")
    } else {
        parts.join("|")
    }
}

unsafe extern "C" fn sl_log_callback(kind: u32, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    // SAFETY: NUL-terminated log line from Streamline.
    let text = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    match kind {
        2 => tracing::error!(target: "streamline", "{trimmed}"),
        1 => tracing::warn!(target: "streamline", "{trimmed}"),
        _ => tracing::info!(target: "streamline", "{trimmed}"),
    }
}

static LAST_API_ERROR: AtomicI32 = AtomicI32::new(0);

unsafe extern "C" fn dlssg_api_error_callback(err: *const APIError) {
    if let Some(err) = unsafe { err.as_ref() } {
        LAST_API_ERROR.store(err.code, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Authenticode + NVIDIA RSA1 verification (`include/sl_security.h`)
// ---------------------------------------------------------------------------

mod security {
    use super::*;
    use windows_sys::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_SYSTEM32;

    /// NVIDIA's Streamline RSA public key (`include/sl_security.h`, a CNG RSA1 blob). Public key
    /// only, used to check that the Streamline DLLs are NVIDIA's. Policy exception: docs/asset-policy.md.
    const RSA_STREAMLINE_PUBLIC_KEY: [u8; 411] =
        *include_bytes!("nvidia-streamline-public-key.bin");

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    #[repr(C)]
    struct WintrustFileInfo {
        cb_struct: u32,
        pcwsz_file_path: *const u16,
        h_file: *mut c_void,
        pg_known_subject: *const Guid,
    }

    #[repr(C)]
    struct CertStrongSignPara {
        cb_size: u32,
        dw_info_choice: u32,
        psz_oid: *const c_char,
    }

    #[repr(C)]
    struct WintrustSignatureSettings {
        cb_struct: u32,
        dw_index: u32,
        dw_flags: u32,
        c_secondary_sigs: u32,
        dw_verified_sig_index: u32,
        p_crypto_policy: *const CertStrongSignPara,
    }

    #[repr(C)]
    struct WintrustData {
        cb_struct: u32,
        p_policy_callback_data: *mut c_void,
        p_sip_client_data: *mut c_void,
        dw_ui_choice: u32,
        fdw_revocation_checks: u32,
        dw_union_choice: u32,
        p_file: *const WintrustFileInfo,
        dw_state_action: u32,
        h_wvt_state_data: *mut c_void,
        pwsz_url_reference: *const u16,
        dw_prov_flags: u32,
        dw_ui_context: u32,
        p_signature_settings: *mut WintrustSignatureSettings,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CryptoApiBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    #[repr(C)]
    struct CryptAlgorithmIdentifier {
        psz_obj_id: *mut c_char,
        parameters: CryptoApiBlob,
    }

    #[repr(C)]
    struct CryptAttribute {
        psz_obj_id: *mut c_char,
        c_value: u32,
        rg_value: *mut CryptoApiBlob,
    }

    #[repr(C)]
    struct CryptAttributes {
        c_attr: u32,
        rg_attr: *mut CryptAttribute,
    }

    #[repr(C)]
    struct CmsgSignerInfo {
        dw_version: u32,
        issuer: CryptoApiBlob,
        serial_number: CryptoApiBlob,
        hash_algorithm: CryptAlgorithmIdentifier,
        hash_encryption_algorithm: CryptAlgorithmIdentifier,
        encrypted_hash: CryptoApiBlob,
        auth_attrs: CryptAttributes,
        unauth_attrs: CryptAttributes,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CryptBitBlob {
        cb_data: u32,
        pb_data: *mut u8,
        c_unused_bits: u32,
    }

    #[repr(C)]
    struct CertPublicKeyInfo {
        algorithm: CryptAlgorithmIdentifier,
        public_key: CryptBitBlob,
    }

    #[repr(C)]
    struct CertInfo {
        dw_version: u32,
        serial_number: CryptoApiBlob,
        signature_algorithm: CryptAlgorithmIdentifier,
        issuer: CryptoApiBlob,
        not_before: [u32; 2],
        not_after: [u32; 2],
        subject: CryptoApiBlob,
        subject_public_key_info: CertPublicKeyInfo,
        issuer_unique_id: CryptBitBlob,
        subject_unique_id: CryptBitBlob,
        c_extension: u32,
        rg_extension: *mut c_void,
    }

    #[repr(C)]
    struct CertContext {
        dw_cert_encoding_type: u32,
        pb_cert_encoded: *mut u8,
        cb_cert_encoded: u32,
        p_cert_info: *const CertInfo,
        h_cert_store: *mut c_void,
    }

    fn sys_proc<F: Copy>(module: HMODULE, name: &CStr) -> Option<F> {
        // SAFETY: valid system module handle and NUL-terminated ASCII name.
        let p = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
        // SAFETY: caller matches the Win32 function signature.
        Some(unsafe { std::mem::transmute_copy::<usize, F>(&(p as usize)) })
    }

    pub(super) fn verify_interposer(full_path: &Path) -> Result<(), String> {
        let abs = std::path::absolute(full_path).map_err(|e| format!("absolute path: {e}"))?;
        let text = abs.to_string_lossy();
        let clean = text.strip_prefix(r"\\?\").unwrap_or(&text);
        let wide: Vec<u16> = clean.encode_utf16().chain([0]).collect();

        let wintrust_name: Vec<u16> = "wintrust.dll\0".encode_utf16().collect();
        // SAFETY: system32-only load of Windows trust verification DLL.
        let wintrust = unsafe {
            LoadLibraryExW(wintrust_name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32)
        };
        if wintrust.is_null() {
            return Err("failed to load wintrust.dll".into());
        }
        type FnWinVerifyTrust = unsafe extern "system" fn(HWND, *mut Guid, *mut WintrustData) -> i32;
        let win_verify_trust: FnWinVerifyTrust =
            sys_proc(wintrust, c"WinVerifyTrust").ok_or("missing WinVerifyTrust")?;

        let mut action = Guid {
            data1: 0x00aac56b,
            data2: 0xcd44,
            data3: 0x11d0,
            data4: [0x8c, 0xc2, 0x00, 0xc0, 0x4f, 0xc2, 0x95, 0xee],
        };
        let file_info = WintrustFileInfo {
            cb_struct: size_of::<WintrustFileInfo>() as u32,
            pcwsz_file_path: wide.as_ptr(),
            h_file: std::ptr::null_mut(),
            pg_known_subject: std::ptr::null(),
        };
        let oid = c"1.3.6.1.4.1.311.72.1.1";
        let strong_policy = CertStrongSignPara {
            cb_size: size_of::<CertStrongSignPara>() as u32,
            dw_info_choice: 2, // CERT_STRONG_SIGN_OID_INFO_CHOICE
            psz_oid: oid.as_ptr(),
        };
        let mut sig_settings = WintrustSignatureSettings {
            cb_struct: size_of::<WintrustSignatureSettings>() as u32,
            dw_index: 0,
            dw_flags: 1 | 2, // WSS_VERIFY_SPECIFIC | WSS_GET_SECONDARY_SIG_COUNT
            c_secondary_sigs: 0,
            dw_verified_sig_index: 0,
            p_crypto_policy: &strong_policy,
        };
        let mut wvt_data = WintrustData {
            cb_struct: size_of::<WintrustData>() as u32,
            p_policy_callback_data: std::ptr::null_mut(),
            p_sip_client_data: std::ptr::null_mut(),
            dw_ui_choice: 2,          // WTD_UI_NONE
            fdw_revocation_checks: 0, // WTD_REVOKE_NONE
            dw_union_choice: 1,       // WTD_CHOICE_FILE
            p_file: &file_info,
            dw_state_action: 1,       // WTD_STATEACTION_VERIFY
            h_wvt_state_data: std::ptr::null_mut(),
            pwsz_url_reference: std::ptr::null(),
            dw_prov_flags: 0,
            dw_ui_context: 0,
            p_signature_settings: &mut sig_settings,
        };

        // SAFETY: WinVerifyTrust validates the Authenticode signature on `clean`.
        let status = unsafe { win_verify_trust(HWND(std::ptr::null_mut()), &mut action, &mut wvt_data) };
        wvt_data.dw_state_action = 2; // WTD_STATEACTION_CLOSE
        // SAFETY: Always close WinVerifyTrust state data.
        unsafe { win_verify_trust(HWND(std::ptr::null_mut()), &mut action, &mut wvt_data) };

        if status != 0 {
            return Err(format!("WinVerifyTrust failed ({status:#x})"));
        }
        if sig_settings.c_secondary_sigs != 1 {
            return Err(format!(
                "expected 1 secondary signature (NVIDIA), found {}",
                sig_settings.c_secondary_sigs
            ));
        }
        if !is_signed_by_nvidia(&wide) {
            return Err("secondary signature does not match NVIDIA Streamline public key".into());
        }
        Ok(())
    }

    fn is_signed_by_nvidia(wide_path: &[u16]) -> bool {
        use windows_sys::Win32::Foundation::LocalFree;

        let crypt32_name: Vec<u16> = "crypt32.dll\0".encode_utf16().collect();
        // SAFETY: system32-only load of crypt32.dll.
        let crypt32 = unsafe { LoadLibraryExW(crypt32_name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if crypt32.is_null() {
            return false;
        }

        type FnCryptQueryObject = unsafe extern "system" fn(
            u32,
            *const c_void,
            u32,
            u32,
            u32,
            *mut u32,
            *mut u32,
            *mut u32,
            *mut *mut c_void,
            *mut *mut c_void,
            *mut *const c_void,
        ) -> i32;
        type FnCryptMsgGetParam =
            unsafe extern "system" fn(*mut c_void, u32, u32, *mut c_void, *mut u32) -> i32;
        type FnCryptMsgOpenToDecode =
            unsafe extern "system" fn(u32, u32, u32, usize, *const c_void, *const c_void) -> *mut c_void;
        type FnCryptMsgUpdate = unsafe extern "system" fn(*mut c_void, *const u8, u32, i32) -> i32;
        type FnCryptMsgClose = unsafe extern "system" fn(*mut c_void) -> i32;
        type FnCertOpenStore =
            unsafe extern "system" fn(*const c_char, u32, usize, u32, *const c_void) -> *mut c_void;
        type FnCertCloseStore = unsafe extern "system" fn(*mut c_void, u32) -> i32;
        type FnCertFindCertificateInStore = unsafe extern "system" fn(
            *mut c_void,
            u32,
            u32,
            u32,
            *const c_void,
            *const CertContext,
        ) -> *const CertContext;
        type FnCertFreeCertificateContext = unsafe extern "system" fn(*const CertContext) -> i32;
        type FnCryptDecodeObjectEx = unsafe extern "system" fn(
            u32,
            *const c_char,
            *const u8,
            u32,
            u32,
            *const c_void,
            *mut *mut c_void,
            *mut u32,
        ) -> i32;

        let (
            Some(crypt_query_object),
            Some(crypt_msg_get_param),
            Some(crypt_msg_open_to_decode),
            Some(crypt_msg_update),
            Some(crypt_msg_close),
            Some(cert_open_store),
            Some(cert_close_store),
            Some(cert_find_cert),
            Some(cert_free_cert),
            Some(crypt_decode_object_ex),
        ) = (
            sys_proc::<FnCryptQueryObject>(crypt32, c"CryptQueryObject"),
            sys_proc::<FnCryptMsgGetParam>(crypt32, c"CryptMsgGetParam"),
            sys_proc::<FnCryptMsgOpenToDecode>(crypt32, c"CryptMsgOpenToDecode"),
            sys_proc::<FnCryptMsgUpdate>(crypt32, c"CryptMsgUpdate"),
            sys_proc::<FnCryptMsgClose>(crypt32, c"CryptMsgClose"),
            sys_proc::<FnCertOpenStore>(crypt32, c"CertOpenStore"),
            sys_proc::<FnCertCloseStore>(crypt32, c"CertCloseStore"),
            sys_proc::<FnCertFindCertificateInStore>(crypt32, c"CertFindCertificateInStore"),
            sys_proc::<FnCertFreeCertificateContext>(crypt32, c"CertFreeCertificateContext"),
            sys_proc::<FnCryptDecodeObjectEx>(crypt32, c"CryptDecodeObjectEx"),
        ) else {
            return false;
        };

        const X509_PKCS7: u32 = 0x0001_0001; // X509_ASN_ENCODING | PKCS_7_ASN_ENCODING
        let (mut enc, mut ctype, mut ftype) = (0u32, 0u32, 0u32);
        let (mut store, mut msg) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: queries embedded PKCS7 signature on `wide_path`.
        if unsafe {
            crypt_query_object(
                1, // CERT_QUERY_OBJECT_FILE
                wide_path.as_ptr().cast(),
                1 << 10, // CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED
                1 << 1,  // CERT_QUERY_FORMAT_FLAG_BINARY
                0,
                &mut enc,
                &mut ctype,
                &mut ftype,
                &mut store,
                &mut msg,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return false;
        }

        let mut signer_size = 0u32;
        // SAFETY: size query for CMSG_SIGNER_INFO_PARAM (6).
        let ok_sz = unsafe { crypt_msg_get_param(msg, 6, 0, std::ptr::null_mut(), &mut signer_size) };
        if ok_sz == 0 || signer_size == 0 {
            // SAFETY: close handles acquired above.
            unsafe {
                crypt_msg_close(msg);
                cert_close_store(store, 1);
            }
            return false;
        }
        let mut signer_buf = vec![0u8; signer_size as usize];
        // SAFETY: reads signer info into appropriately sized buffer.
        if unsafe { crypt_msg_get_param(msg, 6, 0, signer_buf.as_mut_ptr().cast(), &mut signer_size) } == 0 {
            // SAFETY: close handles acquired above.
            unsafe {
                crypt_msg_close(msg);
                cert_close_store(store, 1);
            }
            return false;
        }
        // SAFETY: `signer_buf` contains a valid CMSG_SIGNER_INFO from `CryptMsgGetParam`.
        let signer = unsafe { &*(signer_buf.as_ptr().cast::<CmsgSignerInfo>()) };
        let mut valid = false;

        // SAFETY: `unauth_attrs.rg_attr` has `c_attr` entries.
        let attrs = unsafe {
            if signer.unauth_attrs.rg_attr.is_null() {
                &[][..]
            } else {
                std::slice::from_raw_parts(signer.unauth_attrs.rg_attr, signer.unauth_attrs.c_attr as usize)
            }
        };
        for attr in attrs {
            if attr.psz_obj_id.is_null() || attr.rg_value.is_null() || attr.c_value == 0 {
                continue;
            }
            // SAFETY: NUL-terminated OID string.
            let oid = unsafe { CStr::from_ptr(attr.psz_obj_id) };
            if oid != c"1.3.6.1.4.1.311.2.4.1" {
                continue;
            }
            // SAFETY: `rg_value` points to at least one `CryptoApiBlob`.
            let blob = unsafe { *attr.rg_value };
            // SAFETY: open nested PKCS7 message for decoding.
            let msg2 = unsafe {
                crypt_msg_open_to_decode(X509_PKCS7, 0, 0, 0, std::ptr::null(), std::ptr::null())
            };
            if msg2.is_null() {
                break;
            }
            // SAFETY: decode nested signature blob and inspect its signer cert public key.
            unsafe {
                if crypt_msg_update(msg2, blob.pb_data, blob.cb_data, 1) != 0 {
                    let mut sz2 = 0u32;
                    if crypt_msg_get_param(msg2, 6, 0, std::ptr::null_mut(), &mut sz2) != 0 && sz2 > 0 {
                        let mut buf2 = vec![0u8; sz2 as usize];
                        if crypt_msg_get_param(msg2, 6, 0, buf2.as_mut_ptr().cast(), &mut sz2) != 0 {
                            let s2 = &*(buf2.as_ptr().cast::<CmsgSignerInfo>());
                            let store2 = cert_open_store(
                                5 as *const c_char, // CERT_STORE_PROV_PKCS7
                                X509_PKCS7,
                                0,
                                0,
                                (&raw const blob).cast(),
                            );
                            if !store2.is_null() {
                                let mut cert_info: CertInfo = std::mem::zeroed();
                                cert_info.issuer = s2.issuer;
                                cert_info.serial_number = s2.serial_number;
                                let ctx = cert_find_cert(
                                    store2,
                                    X509_PKCS7,
                                    0,
                                    11 << 16, // CERT_FIND_SUBJECT_CERT
                                    (&raw const cert_info).cast(),
                                    std::ptr::null(),
                                );
                                if !ctx.is_null() {
                                    let pkey = &(*(*ctx).p_cert_info).subject_public_key_info.public_key;
                                    let mut decoded: *mut c_void = std::ptr::null_mut();
                                    let mut decoded_len = 0u32;
                                    if crypt_decode_object_ex(
                                        X509_PKCS7,
                                        72 as *const c_char, // CNG_RSA_PUBLIC_KEY_BLOB
                                        pkey.pb_data,
                                        pkey.cb_data,
                                        0x8000, // CRYPT_ENCODE_ALLOC_FLAG
                                        std::ptr::null(),
                                        &mut decoded,
                                        &mut decoded_len,
                                    ) != 0
                                        && !decoded.is_null()
                                    {
                                        let key_bytes = std::slice::from_raw_parts(
                                            decoded.cast::<u8>(),
                                            decoded_len as usize,
                                        );
                                        valid = key_bytes == RSA_STREAMLINE_PUBLIC_KEY;
                                        LocalFree(decoded);
                                    }
                                    cert_free_cert(ctx);
                                }
                                cert_close_store(store2, 1);
                            }
                        }
                    }
                }
                crypt_msg_close(msg2);
            }
            break;
        }

        // SAFETY: close handles acquired by CryptQueryObject.
        unsafe {
            crypt_msg_close(msg);
            cert_close_store(store, 1);
        }
        valid
    }
}

// ---------------------------------------------------------------------------
// Dynamic loader + Session state machine
// ---------------------------------------------------------------------------

impl CoreApi {
    fn proc<F: Copy>(module: HMODULE, name: &CStr) -> Result<F, String> {
        // SAFETY: valid DLL handle and NUL-terminated symbol name.
        let addr = unsafe { GetProcAddress(module, name.as_ptr().cast()) }
            .map(|f| f as usize)
            .ok_or_else(|| format!("sl.interposer.dll missing export {}", name.to_string_lossy()))?;
        // SAFETY: caller supplies the exact exported C signature.
        Ok(unsafe { std::mem::transmute_copy::<usize, F>(&addr) })
    }

    fn load(interposer_path: &Path) -> Result<Self, String> {
        let wide: Vec<u16> = interposer_path.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: NUL-terminated absolute/full DLL path; search sibling plugins in its folder.
        let mut module = unsafe {
            LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH)
        };
        if module.is_null() {
            // SAFETY: fallback standard search in case dependencies live on PATH/System32.
            module = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), 0) };
        }
        if module.is_null() {
            return Err(format!(
                "LoadLibraryExW({}) failed: {}",
                interposer_path.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self {
            module: module as usize,
            init: Self::proc(module, c"slInit")?,
            shutdown: Self::proc(module, c"slShutdown")?,
            is_feature_supported: Self::proc(module, c"slIsFeatureSupported")?,
            is_feature_loaded: Self::proc(module, c"slIsFeatureLoaded")?,
            set_feature_loaded: Self::proc(module, c"slSetFeatureLoaded")?,
            set_tag_for_frame: Self::proc(module, c"slSetTagForFrame")?,
            set_constants: Self::proc(module, c"slSetConstants")?,
            get_feature_requirements: Self::proc(module, c"slGetFeatureRequirements")?,
            get_feature_version: Self::proc(module, c"slGetFeatureVersion")?,
            upgrade_interface: Self::proc(module, c"slUpgradeInterface")?,
            get_native_interface: Self::proc(module, c"slGetNativeInterface")?,
            get_feature_function: Self::proc(module, c"slGetFeatureFunction")?,
            get_new_frame_token: Self::proc(module, c"slGetNewFrameToken")?,
            set_d3d_device: Self::proc(module, c"slSetD3DDevice")?,
            free_resources: Self::proc(module, c"slFreeResources")?,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct AppliedDlssgOptions {
    mode: u32,
    frames: u32,
    flags: u32,
    dyn_w: u32,
    dyn_h: u32,
    ui_recomp: u8,
}

#[derive(Default)]
pub(crate) struct Telemetry {
    pub requested_mode: DlssFgMode,
    pub requested_frames: u32,
    pub effective_mode: DlssFgMode,
    pub effective_frames: u32,
    pub actual_active: bool,
    pub dlssg_supported: bool,
    pub dlssg_loaded: bool,
    pub max_frames: u32,
    pub min_dim: u32,
    pub vsync_supported: bool,
    pub dynamic_mfg_supported: bool,
    pub presented_per_app_frame: u32,
    pub last_status: u32,
    pub vram_estimate_mb: u64,
    pub deferred_reason: &'static str,
    pub runtime_dir: String,
    pub runtime_mode: SlRuntimeMode,
    pub constants_set_frames: u64,
    pub depth_tag_frames: u64,
    pub mvec_tag_frames: u64,
    pub options_apply_calls: u64,
    pub active_frames: u64,
    pub deferred_frames: u64,
}

static TELEMETRY: Mutex<Option<Telemetry>> = Mutex::new(None);
static SHUTDOWN_DONE: AtomicBool = AtomicBool::new(false);

fn update_telemetry(f: impl FnOnce(&mut Telemetry)) {
    let mut t = TELEMETRY.lock().unwrap_or_else(|e| e.into_inner());
    let entry = t.get_or_insert_with(Telemetry::default);
    f(entry);
}

pub(crate) fn status_line() -> Option<String> {
    let t = TELEMETRY.lock().unwrap_or_else(|e| e.into_inner());
    let t = t.as_ref()?;
    if t.requested_mode == DlssFgMode::Off && !t.dlssg_loaded {
        return None;
    }
    Some(format!(
        "dlss_fg={} (req={}:{}x, eff={}:{}x, active={}, max={}x, presented={}, status={}, reflex_vram={}MB, reason={}, consts={}, depth={}, mvec={}, set_opts={})",
        t.runtime_mode.as_str(),
        t.requested_mode.as_str(),
        t.requested_frames + 1,
        t.effective_mode.as_str(),
        t.effective_frames + 1,
        t.actual_active,
        t.max_frames + 1,
        t.presented_per_app_frame,
        dlssg_status_str(t.last_status),
        t.vram_estimate_mb,
        t.deferred_reason,
        t.constants_set_frames,
        t.depth_tag_frames,
        t.mvec_tag_frames,
        t.options_apply_calls,
    ))
}

pub(crate) fn overlay_badge() -> Option<String> {
    let t = TELEMETRY.lock().unwrap_or_else(|e| e.into_inner());
    let t = t.as_ref()?;
    if t.actual_active {
        let mult = t.presented_per_app_frame.max(t.effective_frames + 1);
        Some(format!("DLSS-G {}x", mult))
    } else if t.requested_mode != DlssFgMode::Off {
        Some(format!("DLSS-G deferred ({})", t.deferred_reason))
    } else {
        None
    }
}

pub(crate) struct Session {
    cfg: DlssFgConfig,
    core: CoreApi,
    features: Option<FeatureApi>,
    proxy_device: Option<ID3D12Device>,
    proxy_queue: Option<ID3D12CommandQueue>,
    dlssg_supported: bool,
    dlssg_loaded: bool,
    reflex_supported: bool,
    pcl_supported: bool,
    reflex_low_latency_available: bool,
    reflex_enabled_last: Option<bool>,
    max_frames: u32,
    min_dim: u32,
    vsync_supported: bool,
    dynamic_mfg_supported: bool,
    frame_index: u32,
    current_token: *mut FrameToken,
    sim_active: bool,
    render_submitted: bool,
    last_applied: Option<AppliedDlssgOptions>,
    tags_active: bool,
    prev_view_to_clip: [[f32; 4]; 4],
    prev_clip_to_view: [[f32; 4]; 4],
    have_prev_camera: bool,
    kept_textures: Vec<std::sync::Arc<HostTexture>>,
    kept_snapshots: Vec<ID3D12Resource>,
}

// SAFETY: Streamline API handles (`FrameToken` only touched under `Host::sl` mutex)
// and D3D12 COM pointers are free-threaded.
unsafe impl Send for Session {}

static SESSION_API: OnceLock<usize> = OnceLock::new();

pub(crate) fn shutdown() {
    if SHUTDOWN_DONE.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some(&addr) = SESSION_API.get() {
        // SAFETY: `slShutdown` function pointer stored during `init_early`.
        let shutdown_fn: FnSlShutdown = unsafe { std::mem::transmute(addr) };
        // SAFETY: called once on process teardown.
        let res = unsafe { shutdown_fn() };
        tracing::info!(result = result_str(res), "Streamline slShutdown");
    }
}

fn resolve_runtime_dir(cfg: &DlssFgConfig) -> PathBuf {
    if let Some(dir) = &cfg.runtime_dir {
        return dir.clone();
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    if cfg.runtime_mode == SlRuntimeMode::Development {
        let dev = exe_dir.join("development");
        if dev.join("sl.interposer.dll").is_file() {
            return dev;
        }
    }
    exe_dir
}

fn check_runtime_files(dir: &Path, mode: SlRuntimeMode) -> bool {
    let required = [
        "sl.interposer.dll",
        "sl.common.dll",
        "sl.dlss_g.dll",
        "sl.reflex.dll",
        "sl.pcl.dll",
    ];
    let mut all_required = true;
    for dll in required {
        let p = dir.join(dll);
        if p.is_file() {
            tracing::info!(dll, path = %p.display(), "Streamline runtime dependency present");
        } else {
            tracing::warn!(
                dll,
                expected = %p.display(),
                "Streamline runtime dependency missing; DLSS Frame Generation will fall back to Off"
            );
            all_required = false;
        }
    }
    let ngxg = dir.join("nvngx_dlssg.dll");
    if ngxg.is_file() {
        tracing::info!(path = %ngxg.display(), "DLSS-G snippet nvngx_dlssg.dll present locally");
    } else {
        tracing::info!(
            expected = %ngxg.display(),
            "nvngx_dlssg.dll not found locally (NGX driver store may supply it if installed)"
        );
    }
    if mode == SlRuntimeMode::Development {
        let cfg_json = dir.join("sl.dlss_g.json");
        tracing::warn!(
            mode = "development",
            dir = %dir.display(),
            sl_dlss_g_json = cfg_json.is_file(),
            "Streamline DEVELOPMENT runtime mode active (non-production DLLs/overlays must NOT ship)"
        );
    }
    all_required
}

impl Session {
    pub(crate) fn init_early(cfg: DlssFgConfig) -> Option<Self> {
        if cfg.mode == DlssFgMode::Off {
            return None;
        }
        let runtime_dir = resolve_runtime_dir(&cfg);
        let runtime_dir_str = runtime_dir.display().to_string();
        tracing::info!(
            mode = cfg.runtime_mode.as_str(),
            requested = cfg.mode.as_str(),
            frames = cfg.frames,
            runtime_dir = %runtime_dir_str,
            project_id = %cfg.project_id,
            application_id = cfg.application_id,
            "initializing Streamline for DLSS Frame Generation"
        );
        update_telemetry(|t| {
            t.requested_mode = cfg.mode;
            t.requested_frames = cfg.frames;
            t.effective_mode = DlssFgMode::Off;
            t.runtime_mode = cfg.runtime_mode;
            t.runtime_dir = runtime_dir_str.clone();
            t.deferred_reason = "checking dependencies";
        });

        if !check_runtime_files(&runtime_dir, cfg.runtime_mode) {
            update_telemetry(|t| t.deferred_reason = "missing Streamline DLLs");
            return None;
        }

        let interposer_path = runtime_dir.join("sl.interposer.dll");
        match cfg.runtime_mode {
            SlRuntimeMode::Production => {
                if let Err(reason) = security::verify_interposer(&interposer_path) {
                    tracing::error!(
                        path = %interposer_path.display(),
                        reason,
                        "Streamline production signature verification failed; DLSS-G disabled"
                    );
                    update_telemetry(|t| t.deferred_reason = "signature verification failed");
                    return None;
                }
                tracing::info!(
                    path = %interposer_path.display(),
                    "sl.interposer.dll NVIDIA embedded signature verified"
                );
            }
            SlRuntimeMode::Development => {
                tracing::warn!(
                    path = %interposer_path.display(),
                    "skipping Authenticode/NVIDIA signature check in Streamline development mode"
                );
            }
        }

        let core = match CoreApi::load(&interposer_path) {
            Ok(core) => core,
            Err(err) => {
                tracing::warn!(error = err, "failed to load sl.interposer.dll; DLSS-G disabled");
                update_telemetry(|t| t.deferred_reason = "sl.interposer.dll load failed");
                return None;
            }
        };

        let _ = SESSION_API.set(core.shutdown as usize);

        let plugin_dir_wide: Vec<u16> = std::path::absolute(&runtime_dir)
            .unwrap_or_else(|_| runtime_dir.clone())
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect();
        let plugin_ptrs = [plugin_dir_wide.as_ptr()];
        let log_dir_wide: Option<Vec<u16>> = cfg.log_dir.as_ref().map(|p| {
            std::path::absolute(p)
                .unwrap_or_else(|_| p.clone())
                .as_os_str()
                .encode_wide()
                .chain([0])
                .collect()
        });
        let project_id_c = CString::new(cfg.project_id.as_str()).unwrap_or_else(|_| {
            CString::new("a3f7e812-4c9b-4e6a-9d12-71b4e509a8c3").expect("valid guid")
        });
        let engine_ver_c = CString::new(env!("CARGO_PKG_VERSION")).expect("valid version");

        let is_dev = cfg.runtime_mode == SlRuntimeMode::Development;
        let mut flags =
            PREF_DISABLE_CL_STATE_TRACKING | PREF_USE_MANUAL_HOOKING | PREF_USE_FRAME_BASED_TAGGING;
        if !is_dev {
            flags |= PREF_DISABLE_DEBUG_TEXT;
        }
        let features = [K_FEATURE_DLSS_G, K_FEATURE_REFLEX, K_FEATURE_PCL];
        let pref = Preferences {
            base: BaseStructure::new(TYPE_PREFERENCES, 1),
            show_console: is_dev,
            log_level: if is_dev { LOG_LEVEL_VERBOSE } else { LOG_LEVEL_DEFAULT },
            paths_to_plugins: plugin_ptrs.as_ptr(),
            num_paths_to_plugins: plugin_ptrs.len() as u32,
            path_to_logs_and_data: log_dir_wide.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
            allocate_callback: std::ptr::null(),
            release_callback: std::ptr::null(),
            log_message_callback: Some(sl_log_callback),
            flags,
            features_to_load: features.as_ptr(),
            num_features_to_load: features.len() as u32,
            application_id: cfg.application_id,
            engine: ENGINE_CUSTOM,
            engine_version: engine_ver_c.as_ptr(),
            project_id: if cfg.application_id == 0 {
                project_id_c.as_ptr()
            } else {
                std::ptr::null()
            },
            render_api: RENDER_API_D3D12,
        };

        tracing::info!(
            setup_path = "manual_hooking",
            debug_text = is_dev,
            console = is_dev,
            log_level = if is_dev { "verbose" } else { "default" },
            log_dir = ?cfg.log_dir,
            "calling slInit"
        );
        // SAFETY: all pointers inside `pref` stay valid for the duration of `slInit`.
        let init_res = unsafe { (core.init)(&pref, K_SDK_VERSION) };
        if init_res != SL_OK {
            tracing::warn!(
                result = result_str(init_res),
                code = init_res,
                "Streamline slInit failed; falling back to native rendering"
            );
            update_telemetry(|t| t.deferred_reason = "slInit failed");
            return None;
        }

        Some(Self {
            cfg,
            core,
            features: None,
            proxy_device: None,
            proxy_queue: None,
            dlssg_supported: false,
            dlssg_loaded: false,
            reflex_supported: false,
            pcl_supported: false,
            reflex_low_latency_available: false,
            reflex_enabled_last: None,
            max_frames: 1,
            min_dim: 64,
            vsync_supported: false,
            dynamic_mfg_supported: false,
            frame_index: 1,
            current_token: std::ptr::null_mut(),
            sim_active: false,
            render_submitted: false,
            last_applied: None,
            tags_active: false,
            prev_view_to_clip: [[0.0; 4]; 4],
            prev_clip_to_view: [[0.0; 4]; 4],
            have_prev_camera: false,
            kept_textures: Vec::new(),
            kept_snapshots: Vec::new(),
        })
    }

    pub(crate) fn on_device_created(
        &mut self,
        native_device: &ID3D12Device,
    ) -> Option<(ID3D12CommandQueue, ID3D12CommandQueue)> {
        // 1. Register native D3D12 device with Streamline (initializes loaded plugins + hooks).
        // SAFETY: `native_device` is a live native ID3D12Device created before proxy wrapping.
        let set_res = unsafe { (self.core.set_d3d_device)(native_device.as_raw()) };
        if set_res != SL_OK {
            tracing::warn!(
                result = result_str(set_res),
                "slSetD3DDevice failed; disabling DLSS-G"
            );
            update_telemetry(|t| t.deferred_reason = "slSetD3DDevice failed");
            return None;
        }

        // 2. Check feature support against the device's adapter LUID.
        // SAFETY: plain query on a live ID3D12Device.
        let mut luid = unsafe { native_device.GetAdapterLuid() };
        let adapter_info = AdapterInfo {
            base: BaseStructure::new(TYPE_ADAPTER_INFO, 1),
            device_luid: (&raw mut luid).cast::<u8>(),
            device_luid_size_in_bytes: size_of_val(&luid) as u32,
            vk_physical_device: std::ptr::null_mut(),
        };

        // SAFETY: FFI queries on initialized Streamline session.
        let (dlssg_sup, reflex_sup, pcl_sup) = unsafe {
            (
                (self.core.is_feature_supported)(K_FEATURE_DLSS_G, &adapter_info),
                (self.core.is_feature_supported)(K_FEATURE_REFLEX, &adapter_info),
                (self.core.is_feature_supported)(K_FEATURE_PCL, &adapter_info),
            )
        };
        self.dlssg_supported = dlssg_sup == SL_OK;
        self.reflex_supported = reflex_sup == SL_OK;
        self.pcl_supported = pcl_sup == SL_OK;

        let mut reqs = FeatureRequirements::default();
        // SAFETY: valid FeatureRequirements out pointer.
        let req_res = unsafe { (self.core.get_feature_requirements)(K_FEATURE_DLSS_G, &mut reqs) };
        let mut ver = FeatureVersion::default();
        // SAFETY: valid FeatureVersion out pointer.
        let ver_res = unsafe { (self.core.get_feature_version)(K_FEATURE_DLSS_G, &mut ver) };
        tracing::info!(
            dlssg_supported = self.dlssg_supported,
            dlssg_result = result_str(dlssg_sup),
            reflex_result = result_str(reflex_sup),
            pcl_result = result_str(pcl_sup),
            req_result = result_str(req_res),
            os_detected = %format!("{}.{}.{}", reqs.os_version_detected.major, reqs.os_version_detected.minor, reqs.os_version_detected.build),
            os_required = %format!("{}.{}.{}", reqs.os_version_required.major, reqs.os_version_required.minor, reqs.os_version_required.build),
            driver_detected = %format!("{}.{}.{}", reqs.driver_version_detected.major, reqs.driver_version_detected.minor, reqs.driver_version_detected.build),
            driver_required = %format!("{}.{}.{}", reqs.driver_version_required.major, reqs.driver_version_required.minor, reqs.driver_version_required.build),
            ver_result = result_str(ver_res),
            sl_ver = %format!("{}.{}.{}", ver.version_sl.major, ver.version_sl.minor, ver.version_sl.build),
            ngx_ver = %format!("{}.{}.{}", ver.version_ngx.major, ver.version_ngx.minor, ver.version_ngx.build),
            "Streamline feature support check"
        );

        if !self.dlssg_supported || !self.reflex_supported {
            tracing::warn!(
                dlssg = result_str(dlssg_sup),
                reflex = result_str(reflex_sup),
                "DLSS-G or required Reflex is unsupported on this adapter/OS/driver (check HWS enabled in Windows Graphics Settings); falling back to Off"
            );
            // SAFETY: unload DLSS-G so no swapchain hooks stay attached.
            unsafe { (self.core.set_feature_loaded)(K_FEATURE_DLSS_G, false) };
            update_telemetry(|t| {
                t.dlssg_supported = false;
                t.dlssg_loaded = false;
                t.deferred_reason = if dlssg_sup == 4 {
                    "HWS disabled"
                } else {
                    "feature unsupported"
                };
            });
            return None;
        }

        // 3. Resolve feature functions AFTER `slSetD3DDevice`.
        let get_fn = |feature: u32, name: &CStr| -> Option<usize> {
            let mut ptr: *mut c_void = std::ptr::null_mut();
            // SAFETY: initialized plugin lookup.
            let res = unsafe { (self.core.get_feature_function)(feature, name.as_ptr(), &mut ptr) };
            (res == SL_OK && !ptr.is_null()).then_some(ptr as usize)
        };

        let Some(dlssg_get_state) = get_fn(K_FEATURE_DLSS_G, c"slDLSSGGetState") else {
            tracing::warn!("missing slDLSSGGetState; disabling DLSS-G");
            return None;
        };
        let Some(dlssg_set_options) = get_fn(K_FEATURE_DLSS_G, c"slDLSSGSetOptions") else {
            tracing::warn!("missing slDLSSGSetOptions; disabling DLSS-G");
            return None;
        };
        let reflex_get_state = get_fn(K_FEATURE_REFLEX, c"slReflexGetState");
        let reflex_sleep = get_fn(K_FEATURE_REFLEX, c"slReflexSleep");
        let reflex_set_options = get_fn(K_FEATURE_REFLEX, c"slReflexSetOptions");
        let pcl_set_marker = get_fn(K_FEATURE_PCL, c"slPCLSetMarker");
        tracing::info!(
            dlssg_get_state = format_args!("{dlssg_get_state:#x}"),
            dlssg_set_options = format_args!("{dlssg_set_options:#x}"),
            reflex_get_state = format_args!("{:?}", reflex_get_state.map(|p| format!("{p:#x}"))),
            reflex_sleep = format_args!("{:?}", reflex_sleep.map(|p| format!("{p:#x}"))),
            reflex_set_options = format_args!("{:?}", reflex_set_options.map(|p| format!("{p:#x}"))),
            pcl_set_marker = format_args!("{:?}", pcl_set_marker.map(|p| format!("{p:#x}"))),
            "resolved Streamline feature entry points"
        );
        let feature_api = FeatureApi {
            // SAFETY: pointers resolved from `slGetFeatureFunction`.
            dlssg_get_state: unsafe { std::mem::transmute::<usize, FnDlssgGetState>(dlssg_get_state) },
            // SAFETY: as above.
            dlssg_set_options: unsafe { std::mem::transmute::<usize, FnDlssgSetOptions>(dlssg_set_options) },
            reflex_get_state: reflex_get_state.map(|p| unsafe { std::mem::transmute(p) }),
            reflex_sleep: reflex_sleep.map(|p| unsafe { std::mem::transmute(p) }),
            reflex_set_options: reflex_set_options.map(|p| unsafe { std::mem::transmute(p) }),
            pcl_set_marker: pcl_set_marker.map(|p| unsafe { std::mem::transmute(p) }),
        };
        self.features = Some(feature_api);

        // 4. Upgrade native ID3D12Device to SL proxy and create the direct swapchain queue through it.
        let mut proxy_dev_raw = native_device.as_raw();
        // SAFETY: upgrades raw ID3D12Device pointer in-place to Streamline `D3D12Device` proxy.
        let up_res = unsafe { (self.core.upgrade_interface)(&mut proxy_dev_raw) };
        if up_res != SL_OK || proxy_dev_raw.is_null() {
            tracing::warn!(result = result_str(up_res), "slUpgradeInterface(ID3D12Device) failed; falling back to native queue");
            return None;
        }
        // SAFETY: `slUpgradeInterface` returned an owning `D3D12Device` proxy pointer (`m_refCount = 1`).
        let proxy_device = unsafe { ID3D12Device::from_raw(proxy_dev_raw) };

        let queue_desc = D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            ..Default::default()
        };
        // SAFETY: creates proxy command queue so Streamline hooks `eID3D12Device_CreateCommandQueue`.
        let proxy_queue: ID3D12CommandQueue = match unsafe { proxy_device.CreateCommandQueue(&queue_desc) } {
            Ok(q) => q,
            Err(err) => {
                tracing::warn!(%err, "proxy CreateCommandQueue failed; falling back to native queue");
                return None;
            }
        };

        let mut native_queue_raw = std::ptr::null_mut();
        // SAFETY: retrieves native ID3D12CommandQueue from `D3D12CommandQueue` proxy (adds ref).
        let nat_res = unsafe { (self.core.get_native_interface)(proxy_queue.as_raw(), &mut native_queue_raw) };
        if nat_res != SL_OK || native_queue_raw.is_null() {
            tracing::warn!(result = result_str(nat_res), "slGetNativeInterface(ID3D12CommandQueue) failed");
            return None;
        }
        // SAFETY: `slGetNativeInterface` AddRef'd the underlying native queue.
        let native_queue = unsafe { ID3D12CommandQueue::from_raw(native_queue_raw) };

        self.proxy_device = Some(proxy_device);
        self.proxy_queue = Some(proxy_queue.clone());
        self.dlssg_loaded = true;

        update_telemetry(|t| {
            t.dlssg_supported = true;
            t.dlssg_loaded = true;
            t.deferred_reason = "waiting for gameplay tags";
        });
        tracing::info!("Streamline manual hooking active (proxy device + proxy direct queue ready)");
        Some((native_queue, proxy_queue))
    }

    pub(crate) fn create_swapchain(
        &mut self,
        hwnd: HWND,
        desc: &DXGI_SWAP_CHAIN_DESC1,
    ) -> windows::core::Result<Option<IDXGISwapChain3>> {
        let Some(proxy_queue) = &self.proxy_queue else {
            return Ok(None);
        };
        let want_loaded = self.dlssg_supported && self.cfg.mode != DlssFgMode::Off;
        // SAFETY: toggles DLSS-G feature load state cleanly before swapchain creation (§18.0).
        let load_res = unsafe { (self.core.set_feature_loaded)(K_FEATURE_DLSS_G, want_loaded) };
        self.dlssg_loaded = want_loaded && load_res == SL_OK;
        if !self.dlssg_loaded {
            update_telemetry(|t| {
                t.dlssg_loaded = false;
                t.actual_active = false;
                t.deferred_reason = "disabled by user";
            });
            return Ok(None);
        }

        // SAFETY: standard DXGI factory creation, upgraded via Streamline `slUpgradeInterface`.
        unsafe {
            let native_factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let mut proxy_factory_raw = native_factory.as_raw();
            let up = (self.core.upgrade_interface)(&mut proxy_factory_raw);
            if up != SL_OK || proxy_factory_raw.is_null() {
                tracing::warn!(result = result_str(up), "slUpgradeInterface(IDXGIFactory) failed");
                return Ok(None);
            }
            let proxy_factory = IDXGIFactory2::from_raw(proxy_factory_raw);
            let swap_chain = proxy_factory
                .CreateSwapChainForHwnd(proxy_queue, hwnd, desc, None, None)?
                .cast::<IDXGISwapChain3>()?;
            self.on_swapchain_ready(desc.Width, desc.Height, desc.Format, desc.BufferCount);
            Ok(Some(swap_chain))
        }
    }

    fn on_swapchain_ready(&mut self, width: u32, height: u32, format: DXGI_FORMAT, buffers: u32) {
        self.last_applied = None;
        self.tags_active = false;
        self.have_prev_camera = false;

        let Some(api) = self.features else { return };
        let vp = ViewportHandle {
            base: BaseStructure::new(TYPE_VIEWPORT_HANDLE, 1),
            value: 0,
        };
        // Query basic state + VRAM estimate once at swapchain creation (never per frame).
        let mut vram_opts = DLSSGOptions {
            mode: DLSSG_MODE_ON,
            num_frames_to_generate: self.cfg.frames.clamp(1, 5),
            flags: DLSSG_FLAG_REQUEST_VRAM_ESTIMATE | DLSSG_FLAG_RETAIN_RESOURCES_WHEN_OFF,
            num_back_buffers: buffers,
            mvec_depth_width: self.cfg.hooks.render_width.unwrap_or(width),
            mvec_depth_height: self.cfg.hooks.render_height.unwrap_or(height),
            color_width: width,
            color_height: height,
            color_buffer_format: format.0 as u32,
            mvec_buffer_format: 34, // R16G16_FLOAT
            depth_buffer_format: 40, // D32_FLOAT
            ..Default::default()
        };
        let mut state = DLSSGState::default();
        // SAFETY: valid viewport, state, and options pointers.
        let state_res = unsafe { (api.dlssg_get_state)(&vp, &mut state, &mut vram_opts) };
        if state_res == SL_OK {
            self.max_frames = state.num_frames_to_generate_max.max(1);
            self.min_dim = state.min_width_or_height;
            self.vsync_supported = state.b_is_vsync_support_available == BOOL_TRUE;
            self.dynamic_mfg_supported = state.b_is_dynamic_mfg_supported == BOOL_TRUE;
            let vram_mb = state.estimated_vram_usage_in_bytes >> 20;
            update_telemetry(|t| {
                t.max_frames = self.max_frames;
                t.min_dim = self.min_dim;
                t.vsync_supported = self.vsync_supported;
                t.dynamic_mfg_supported = self.dynamic_mfg_supported;
                t.last_status = state.status;
                t.vram_estimate_mb = vram_mb;
            });
            tracing::info!(
                status = dlssg_status_str(state.status),
                max_generated_frames = self.max_frames,
                min_width_or_height = self.min_dim,
                vsync_supported = self.vsync_supported,
                dynamic_mfg_supported = self.dynamic_mfg_supported,
                estimated_vram_mb = vram_mb,
                "DLSSGState queried on swapchain creation"
            );
        } else {
            tracing::warn!(result = result_str(state_res), "slDLSSGGetState failed on swapchain creation");
        }

        if let Some(reflex_get_state) = api.reflex_get_state {
            let mut rstate = ReflexState::default();
            // SAFETY: valid out pointer.
            if unsafe { reflex_get_state(&mut rstate) } == SL_OK {
                self.reflex_low_latency_available = rstate.low_latency_available;
                tracing::info!(
                    low_latency_available = rstate.low_latency_available,
                    latency_report_available = rstate.latency_report_available,
                    "ReflexState queried"
                );
            }
        }
        self.apply_reflex_options(true);
        self.begin_new_frame_token();
    }

    fn apply_reflex_options(&mut self, want_low_latency: bool) {
        let Some(api) = self.features else { return };
        let Some(set_opts) = api.reflex_set_options else { return };
        if self.reflex_enabled_last == Some(want_low_latency) {
            return;
        }
        let opts = ReflexOptions {
            mode: if want_low_latency {
                REFLEX_LOW_LATENCY
            } else {
                REFLEX_OFF
            },
            ..Default::default()
        };
        // SAFETY: valid ReflexOptions pointer.
        let res = unsafe { set_opts(&opts) };
        if res == SL_OK {
            self.reflex_enabled_last = Some(want_low_latency);
            tracing::info!(low_latency = want_low_latency, "slReflexSetOptions applied");
        } else {
            tracing::warn!(result = result_str(res), "slReflexSetOptions failed");
        }
    }

    fn begin_new_frame_token(&mut self) {
        let mut token: *mut FrameToken = std::ptr::null_mut();
        // SAFETY: `frame_index` is monotonic per presented frame.
        let res = unsafe { (self.core.get_new_frame_token)(&mut token, &self.frame_index) };
        if res != SL_OK || token.is_null() {
            self.current_token = std::ptr::null_mut();
            return;
        }
        self.current_token = token;
        if let Some(api) = self.features {
            if let Some(sleep) = api.reflex_sleep {
                // SAFETY: `slReflexSleep` is always called even when Reflex mode is Off (Reflex guide §4.0/§8.0).
                let _ = unsafe { sleep(token) };
            }
            if let Some(marker) = api.pcl_set_marker {
                // SAFETY: marks start of simulation/frame preparation for `token`.
                let _ = unsafe { marker(PCL_SIMULATION_START, token) };
                self.sim_active = true;
                self.render_submitted = false;
            }
        }
    }

    pub(crate) fn on_render_submit(&mut self) {
        if self.current_token.is_null() || self.render_submitted {
            return;
        }
        if let Some(api) = self.features
            && let Some(marker) = api.pcl_set_marker
        {
            if self.sim_active {
                // SAFETY: pairs `eSimulationStart` before render submission begins.
                let _ = unsafe { marker(PCL_SIMULATION_END, self.current_token) };
                self.sim_active = false;
            }
            // SAFETY: marks start of D3D12 command submission for `current_token`.
            let _ = unsafe { marker(PCL_RENDER_SUBMIT_START, self.current_token) };
            self.render_submitted = true;
        }
    }

    pub(crate) fn before_resize(&mut self) {
        self.clear_tags_and_defer("swapchain resize", std::ptr::null_mut());
    }

    pub(crate) fn after_resize(&mut self, width: u32, height: u32, format: DXGI_FORMAT, buffers: u32) {
        self.on_swapchain_ready(width, height, format, buffers);
    }

    pub(crate) fn desired_swapchain_loaded(&self, cfg: &DlssFgConfig) -> bool {
        self.dlssg_supported && self.proxy_queue.is_some() && cfg.mode != DlssFgMode::Off
    }

    pub(crate) fn is_swapchain_loaded(&self) -> bool {
        self.dlssg_loaded
    }

    pub(crate) fn before_swapchain_recreate(&mut self) {
        self.clear_tags_and_defer("swapchain recreate", std::ptr::null_mut());
        if self.dlssg_loaded {
            let vp = ViewportHandle {
                base: BaseStructure::new(TYPE_VIEWPORT_HANDLE, 1),
                value: 0,
            };
            // SAFETY: frees DLSS-G resources explicitly when toggling off long-term (§6.4 / §18.0).
            unsafe {
                let _ = (self.core.free_resources)(K_FEATURE_DLSS_G, &vp);
                let _ = (self.core.set_feature_loaded)(K_FEATURE_DLSS_G, false);
            }
            self.dlssg_loaded = false;
            self.proxy_device = None;
        }
    }

    fn effective_mode_and_frames(&self, cfg: &DlssFgConfig) -> (DlssFgMode, u32) {
        let clamped_frames = cfg.frames.clamp(1, self.max_frames.max(1));
        let mode = match cfg.mode {
            DlssFgMode::Off => DlssFgMode::Off,
            DlssFgMode::On => DlssFgMode::On,
            DlssFgMode::Auto => DlssFgMode::Auto,
            DlssFgMode::Dynamic => {
                if self.dynamic_mfg_supported {
                    DlssFgMode::Dynamic
                } else {
                    DlssFgMode::Auto
                }
            }
        };
        (mode, clamped_frames)
    }

    fn clear_tags_and_defer(&mut self, reason: &'static str, cmd_list: *mut c_void) {
        let vp = ViewportHandle {
            base: BaseStructure::new(TYPE_VIEWPORT_HANDLE, 1),
            value: 0,
        };
        if self.tags_active && !self.current_token.is_null() {
            let null_res = |kind: u32| {
                ResourceTag::new(std::ptr::null_mut(), kind, LIFECYCLE_VALID_UNTIL_PRESENT, Extent::default())
            };
            let null_tags = [
                null_res(BUFFER_DEPTH),
                null_res(BUFFER_MOTION_VECTORS),
                null_res(BUFFER_HUDLESS_COLOR),
                null_res(BUFFER_UI_COLOR_AND_ALPHA),
            ];
            // SAFETY: clears previous frame tags with null resource pointers when inputs are unavailable.
            unsafe {
                let _ = (self.core.set_tag_for_frame)(
                    self.current_token,
                    &vp,
                    null_tags.as_ptr(),
                    null_tags.len() as u32,
                    cmd_list,
                );
            }
            self.tags_active = false;
        }
        self.apply_dlssg_options(
            DLSSG_MODE_OFF,
            1,
            DLSSG_FLAG_RETAIN_RESOURCES_WHEN_OFF,
            0,
            0,
            false,
            reason,
        );
        update_telemetry(|t| {
            t.effective_mode = DlssFgMode::Off;
            t.actual_active = false;
            t.deferred_reason = reason;
            t.deferred_frames += 1;
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_dlssg_options(
        &mut self,
        mode: u32,
        frames: u32,
        flags: u32,
        dyn_w: u32,
        dyn_h: u32,
        ui_recomp: bool,
        reason: &'static str,
    ) {
        let Some(api) = self.features else { return };
        let ui_recomp_byte = if ui_recomp { BOOL_TRUE } else { BOOL_FALSE };
        let wanted = AppliedDlssgOptions {
            mode,
            frames,
            flags,
            dyn_w,
            dyn_h,
            ui_recomp: ui_recomp_byte,
        };
        if self.last_applied == Some(wanted) {
            return;
        }
        let vp = ViewportHandle {
            base: BaseStructure::new(TYPE_VIEWPORT_HANDLE, 1),
            value: 0,
        };
        let opts = DLSSGOptions {
            mode,
            num_frames_to_generate: frames,
            flags,
            dynamic_res_width: dyn_w,
            dynamic_res_height: dyn_h,
            on_error_callback: Some(dlssg_api_error_callback),
            enable_user_interface_recomposition: ui_recomp_byte,
            dynamic_target_frame_rate: self.cfg.dynamic_target_fps,
            ..Default::default()
        };
        // SAFETY: called on presenting thread right before `Present()`.
        let res = unsafe { (api.dlssg_set_options)(&vp, &opts) };
        if res == SL_OK {
            self.last_applied = Some(wanted);
            update_telemetry(|t| t.options_apply_calls += 1);
            tracing::info!(
                mode,
                frames,
                flags = format_args!("{flags:#x}"),
                dyn_w,
                dyn_h,
                ui_recomp,
                reason,
                "slDLSSGSetOptions applied"
            );
        } else {
            tracing::warn!(
                result = result_str(res),
                mode,
                frames,
                reason,
                "slDLSSGSetOptions failed"
            );
        }
    }

    fn push_hooked_tex(
        &mut self,
        sl_resources: &mut Vec<SlResource>,
        tag_specs: &mut Vec<(u32, u32, Extent)>,
        kind: u32,
        tex: HookedTexture,
        lifecycle: u32,
    ) {
        let (raw, state) = match &tex.source {
            crate::hooks::HookSource::Live(host_tex) => {
                let st = host_tex.state.load(Ordering::Acquire);
                let raw = host_tex.texture.as_raw();
                self.kept_textures.push(host_tex.clone());
                (raw, st)
            }
            crate::hooks::HookSource::Snapshot(res, st) => {
                let raw = res.as_raw();
                self.kept_snapshots.push(res.clone());
                (raw, *st)
            }
        };
        sl_resources.push(SlResource::tex2d(raw, state, tex.width, tex.height, tex.format));
        tag_specs.push((
            kind,
            lifecycle,
            Extent {
                top: 0,
                left: 0,
                width: tex.width,
                height: tex.height,
            },
        ));
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_present(
        &mut self,
        cfg: &DlssFgConfig,
        copy_list: &ID3D12GraphicsCommandList,
        back_w: u32,
        back_h: u32,
        plane0: (&ID3D12Resource, u32),
        plane1: Option<(&ID3D12Resource, u32)>,
        inputs: FrameInputs,
    ) {
        self.cfg = cfg.clone();
        self.kept_textures.clear();
        self.kept_snapshots.clear();

        if self.current_token.is_null() {
            self.begin_new_frame_token();
        }
        let token = self.current_token;
        if token.is_null() {
            return;
        }

        if let Some(api) = self.features
            && let Some(marker) = api.pcl_set_marker
        {
            if self.sim_active {
                // SAFETY: completes simulation span if no earlier submit closed it.
                let _ = unsafe { marker(PCL_SIMULATION_END, token) };
                self.sim_active = false;
            }
            if !self.render_submitted {
                // SAFETY: marks render submit start/end around the present copy list.
                let _ = unsafe { marker(PCL_RENDER_SUBMIT_START, token) };
            }
            // SAFETY: render submission ends before Present starts.
            let _ = unsafe { marker(PCL_RENDER_SUBMIT_END, token) };
            self.render_submitted = false;
        }

        if !self.dlssg_loaded || cfg.mode == DlssFgMode::Off {
            self.clear_tags_and_defer("disabled", copy_list.as_raw());
            return;
        }
        if back_w.min(back_h) < self.min_dim.max(32) {
            self.clear_tags_and_defer("resolution below DLSSGState::minWidthOrHeight", copy_list.as_raw());
            return;
        }

        let Some(depth_tex) = inputs.depth else {
            self.clear_tags_and_defer("no Depth buffer tagged", copy_list.as_raw());
            return;
        };
        let Some(mvec_tex) = inputs.motion_vectors else {
            self.clear_tags_and_defer("no MotionVectors buffer tagged", copy_list.as_raw());
            return;
        };

        let (eff_mode, eff_frames) = self.effective_mode_and_frames(cfg);
        let sl_mode = match eff_mode {
            DlssFgMode::Off => DLSSG_MODE_OFF,
            DlssFgMode::On => DLSSG_MODE_ON,
            DlssFgMode::Auto => DLSSG_MODE_AUTO,
            DlssFgMode::Dynamic => DLSSG_MODE_DYNAMIC,
        };

        // 1. Build common constants for this exact frame token.
        let mv_w = mvec_tex.width.max(1) as f32;
        let mv_h = mvec_tex.height.max(1) as f32;
        let mvec_scale = if let Some(s) = cfg.hooks.mvec_scale {
            s
        } else if let Some(s) = inputs.camera.mvec_scale_override {
            s
        } else {
            match cfg.hooks.mvec_units {
                MvecUnits::Normalized => [1.0, 1.0],
                MvecUnits::Pixels => [1.0 / mv_w, 1.0 / mv_h],
                MvecUnits::Auto => {
                    // FidelityFX & RAGE pass pixel-space motion vectors unless overridden.
                    [1.0 / mv_w, 1.0 / mv_h]
                }
            }
        };

        #[allow(clippy::field_reassign_with_default)]
        let mut consts = Constants::default();
        consts.mvec_scale = mvec_scale;
        consts.jitter_offset = inputs.camera.jitter_offset;
        consts.depth_inverted = if inputs.camera.depth_inverted.unwrap_or(cfg.hooks.depth_inverted) {
            BOOL_TRUE
        } else {
            BOOL_FALSE
        };
        consts.camera_motion_included = if cfg.hooks.camera_motion_included {
            BOOL_TRUE
        } else {
            BOOL_FALSE
        };
        consts.motion_vectors_jittered = if inputs.camera.mvec_jittered.unwrap_or(cfg.hooks.mvec_jittered) {
            BOOL_TRUE
        } else {
            BOOL_FALSE
        };
        consts.motion_vectors_dilated = if cfg.hooks.mvec_dilated {
            BOOL_TRUE
        } else {
            BOOL_FALSE
        };
        consts.reset = if inputs.camera.reset { BOOL_TRUE } else { BOOL_FALSE };
        consts.camera_near = inputs.camera.near;
        consts.camera_far = inputs.camera.far;
        consts.camera_fov = inputs.camera.fov_rad;
        consts.camera_aspect_ratio = inputs.camera.aspect_ratio.max(back_w as f32 / back_h.max(1) as f32);
        if let Some(pos) = inputs.camera.pos {
            consts.camera_pos = pos;
        }
        if let Some(up) = inputs.camera.up {
            consts.camera_up = up;
        }
        if let Some(right) = inputs.camera.right {
            consts.camera_right = right;
        }
        if let Some(fwd) = inputs.camera.fwd {
            consts.camera_fwd = fwd;
        }

        if let Some(v2c) = inputs.camera.view_to_clip {
            consts.camera_view_to_clip = v2c;
            consts.clip_to_camera_view = invert_4x4(&v2c);
        }
        if let Some(c2pc) = inputs.camera.clip_to_prev_clip {
            consts.clip_to_prev_clip = c2pc;
            consts.prev_clip_to_clip = invert_4x4(&c2pc);
        } else if let (Some(v2c), Some(w2v), Some(prev_w2v)) = (
            inputs.camera.view_to_clip,
            inputs.camera.world_to_view,
            inputs.camera.prev_world_to_view,
        ) {
            let prev_v2c = if self.have_prev_camera {
                self.prev_view_to_clip
            } else {
                v2c
            };
            let v2w = invert_4x4(&w2v);
            let c2v = invert_4x4(&v2c);
            let v2pv = mul_4x4(&v2w, &prev_w2v);
            let c2pv = mul_4x4(&c2v, &v2pv);
            let c2pc = mul_4x4(&c2pv, &prev_v2c);
            consts.clip_to_camera_view = c2v;
            consts.clip_to_prev_clip = c2pc;
            consts.prev_clip_to_clip = invert_4x4(&c2pc);
        }
        if let Some(v2c) = inputs.camera.view_to_clip {
            self.prev_view_to_clip = v2c;
            self.prev_clip_to_view = consts.clip_to_camera_view;
            self.have_prev_camera = true;
        }

        let vp = ViewportHandle {
            base: BaseStructure::new(TYPE_VIEWPORT_HANDLE, 1),
            value: 0,
        };
        // SAFETY: valid `Constants`, `FrameToken`, and `ViewportHandle`.
        let const_res = unsafe { (self.core.set_constants)(&consts, token, &vp) };
        if const_res != SL_OK {
            tracing::warn!(result = result_str(const_res), "slSetConstants failed");
            self.clear_tags_and_defer("slSetConstants failed", copy_list.as_raw());
            return;
        }

        // 2. Prepare ResourceTags for Depth, MotionVectors, and optional HUDless / UI.
        let mut sl_resources: Vec<SlResource> = Vec::with_capacity(4);
        let mut tag_specs: Vec<(u32, u32, Extent)> = Vec::with_capacity(4);

        self.push_hooked_tex(
            &mut sl_resources,
            &mut tag_specs,
            BUFFER_DEPTH,
            depth_tex,
            LIFECYCLE_VALID_UNTIL_PRESENT,
        );
        self.push_hooked_tex(
            &mut sl_resources,
            &mut tag_specs,
            BUFFER_MOTION_VECTORS,
            mvec_tex,
            LIFECYCLE_VALID_UNTIL_PRESENT,
        );

        let mut has_hudless = false;
        let mut has_ui = false;
        if let Some(hudless) = inputs.hudless {
            if hudless.width == back_w && hudless.height == back_h {
                self.push_hooked_tex(
                    &mut sl_resources,
                    &mut tag_specs,
                    BUFFER_HUDLESS_COLOR,
                    hudless,
                    LIFECYCLE_VALID_UNTIL_PRESENT,
                );
                has_hudless = true;
            }
        } else if let Some((ui_res, ui_state)) = plane1 {
            // Two-plane PresentX (e.g. Series S 1080p game plane + 4K UI plane):
            // Only tag if dimensions match backbuffer (`ProgrammingGuideDLSS_G.md` §0.0).
            // SAFETY: live D3D12 resources.
            let (p0_desc, p1_desc) = unsafe { (plane0.0.GetDesc(), ui_res.GetDesc()) };
            if p1_desc.Width as u32 == back_w && p1_desc.Height == back_h {
                sl_resources.push(SlResource::tex2d(
                    plane0.0.as_raw(),
                    plane0.1,
                    back_w,
                    back_h,
                    p0_desc.Format.0 as u32,
                ));
                tag_specs.push((
                    BUFFER_HUDLESS_COLOR,
                    LIFECYCLE_VALID_UNTIL_PRESENT,
                    Extent {
                        top: 0,
                        left: 0,
                        width: back_w,
                        height: back_h,
                    },
                ));
                sl_resources.push(SlResource::tex2d(
                    ui_res.as_raw(),
                    ui_state,
                    back_w,
                    back_h,
                    p1_desc.Format.0 as u32,
                ));
                tag_specs.push((
                    BUFFER_UI_COLOR_AND_ALPHA,
                    LIFECYCLE_VALID_UNTIL_PRESENT,
                    Extent {
                        top: 0,
                        left: 0,
                        width: back_w,
                        height: back_h,
                    },
                ));
                has_hudless = true;
                has_ui = true;
            }
        }

        if let Some(ui) = inputs.ui
            && !has_ui
            && ui.width == back_w
            && ui.height == back_h
        {
            let kind = if ui.alpha_only {
                BUFFER_UI_ALPHA
            } else {
                BUFFER_UI_COLOR_AND_ALPHA
            };
            self.push_hooked_tex(
                &mut sl_resources,
                &mut tag_specs,
                kind,
                ui,
                LIFECYCLE_VALID_UNTIL_PRESENT,
            );
            has_ui = true;
        }

        let mut tags: Vec<ResourceTag> = Vec::with_capacity(sl_resources.len());
        for (i, (kind, lifecycle, extent)) in tag_specs.into_iter().enumerate() {
            let res_ptr = &raw mut sl_resources[i];
            tags.push(ResourceTag::new(res_ptr, kind, lifecycle, extent));
        }

        // SAFETY: `sl_resources` outlives `tags` through `slSetTagForFrame`; `copy_list` is open.
        let tag_res = unsafe {
            (self.core.set_tag_for_frame)(
                token,
                &vp,
                tags.as_ptr(),
                tags.len() as u32,
                copy_list.as_raw(),
            )
        };
        if tag_res != SL_OK {
            tracing::warn!(result = result_str(tag_res), "slSetTagForFrame failed");
            self.clear_tags_and_defer("slSetTagForFrame failed", copy_list.as_raw());
            return;
        }
        self.tags_active = true;

        // 3. Apply DLSS-G options only on real state transitions or setting changes.
        let mut dlssg_flags = 0u32;
        if cfg.retain_when_off {
            dlssg_flags |= DLSSG_FLAG_RETAIN_RESOURCES_WHEN_OFF;
        }
        if cfg.fullscreen_menu_detection && has_ui {
            dlssg_flags |= DLSSG_FLAG_ENABLE_FULLSCREEN_MENU_DETECTION;
        }
        let render_w = tag_specs_dim(&sl_resources, 0).0;
        let render_h = tag_specs_dim(&sl_resources, 0).1;
        let mut dyn_w = 0;
        let mut dyn_h = 0;
        if render_w != back_w || render_h != back_h {
            dlssg_flags |= DLSSG_FLAG_DYNAMIC_RESOLUTION;
            dyn_w = render_w;
            dyn_h = render_h;
        }
        let ui_recomp = cfg.ui_recomposition && has_hudless && has_ui;
        self.apply_reflex_options(true);
        self.apply_dlssg_options(
            sl_mode,
            eff_frames,
            dlssg_flags,
            dyn_w,
            dyn_h,
            ui_recomp,
            "active tagged gameplay frame",
        );

        update_telemetry(|t| {
            t.requested_mode = cfg.mode;
            t.requested_frames = cfg.frames;
            t.effective_mode = eff_mode;
            t.effective_frames = eff_frames;
            t.deferred_reason = "active";
            t.constants_set_frames += 1;
            t.depth_tag_frames += 1;
            t.mvec_tag_frames += 1;
            t.active_frames += 1;
        });
    }

    pub(crate) fn before_dxgi_present(&mut self) {
        if let Some(api) = self.features
            && let Some(marker) = api.pcl_set_marker
            && !self.current_token.is_null()
        {
            // SAFETY: matches Present start with the exact `FrameToken` used for constants & tags.
            let _ = unsafe { marker(PCL_PRESENT_START, self.current_token) };
        }
    }

    pub(crate) fn after_dxgi_present(&mut self) {
        if let Some(api) = self.features {
            if let Some(marker) = api.pcl_set_marker
                && !self.current_token.is_null()
            {
                // SAFETY: matches Present end with the exact `FrameToken`.
                let _ = unsafe { marker(PCL_PRESENT_END, self.current_token) };
            }
            if self.dlssg_loaded {
                let vp = ViewportHandle {
                    base: BaseStructure::new(TYPE_VIEWPORT_HANDLE, 1),
                    value: 0,
                };
                let mut state = DLSSGState::default();
                // SAFETY: lightweight per-present state poll (no VRAM estimate flag).
                if unsafe { (api.dlssg_get_state)(&vp, &mut state, std::ptr::null()) } == SL_OK {
                    let active_now =
                        state.status == DLSSG_STATUS_OK && self.tags_active && self.cfg.mode != DlssFgMode::Off;
                    if state.status != DLSSG_STATUS_OK && active_now {
                        tracing::warn!(
                            status = dlssg_status_str(state.status),
                            "DLSS-G entered invalid runtime status; disabling"
                        );
                        self.apply_dlssg_options(
                            DLSSG_MODE_OFF,
                            1,
                            DLSSG_FLAG_RETAIN_RESOURCES_WHEN_OFF,
                            0,
                            0,
                            false,
                            "invalid runtime status",
                        );
                    }
                    update_telemetry(|t| {
                        t.actual_active = active_now && state.num_frames_actually_presented > 1;
                        t.presented_per_app_frame = state.num_frames_actually_presented;
                        t.last_status = state.status;
                    });
                }
            }
        }
        let api_err = LAST_API_ERROR.swap(0, Ordering::Relaxed);
        if api_err != 0 {
            tracing::warn!(hresult = format_args!("{:#010x}", api_err as u32), "DLSS-G Present API error");
        }
        self.frame_index = self.frame_index.wrapping_add(1).max(1);
        self.begin_new_frame_token();
    }
}

fn tag_specs_dim(resources: &[SlResource], idx: usize) -> (u32, u32) {
    resources.get(idx).map_or((0, 0), |r| (r.width, r.height))
}

fn mul_4x4(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for r in 0..4 {
        for c in 0..4 {
            out[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c] + a[r][3] * b[3][c];
        }
    }
    out
}

pub(crate) fn invert_4x4(m: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let p: [f32; 16] = [
        m[0][0], m[0][1], m[0][2], m[0][3], m[1][0], m[1][1], m[1][2], m[1][3], m[2][0], m[2][1],
        m[2][2], m[2][3], m[3][0], m[3][1], m[3][2], m[3][3],
    ];
    let mut inv = [0.0f32; 16];
    inv[0] = p[5] * p[10] * p[15] - p[5] * p[11] * p[14] - p[9] * p[6] * p[15]
        + p[9] * p[7] * p[14]
        + p[13] * p[6] * p[11]
        - p[13] * p[7] * p[10];
    inv[4] = -p[4] * p[10] * p[15] + p[4] * p[11] * p[14] + p[8] * p[6] * p[15]
        - p[8] * p[7] * p[14]
        - p[12] * p[6] * p[11]
        + p[12] * p[7] * p[10];
    inv[8] = p[4] * p[9] * p[15] - p[4] * p[11] * p[13] - p[8] * p[5] * p[15]
        + p[8] * p[7] * p[13]
        + p[12] * p[5] * p[11]
        - p[12] * p[7] * p[9];
    inv[12] = -p[4] * p[9] * p[14] + p[4] * p[10] * p[13] + p[8] * p[5] * p[14]
        - p[8] * p[6] * p[13]
        - p[12] * p[5] * p[10]
        + p[12] * p[6] * p[9];
    inv[1] = -p[1] * p[10] * p[15] + p[1] * p[11] * p[14] + p[9] * p[2] * p[15]
        - p[9] * p[3] * p[14]
        - p[13] * p[2] * p[11]
        + p[13] * p[3] * p[10];
    inv[5] = p[0] * p[10] * p[15] - p[0] * p[11] * p[14] - p[8] * p[2] * p[15]
        + p[8] * p[3] * p[14]
        + p[12] * p[2] * p[11]
        - p[12] * p[3] * p[10];
    inv[9] = -p[0] * p[9] * p[15] + p[0] * p[11] * p[13] + p[8] * p[1] * p[15]
        - p[8] * p[3] * p[13]
        - p[12] * p[1] * p[11]
        + p[12] * p[3] * p[9];
    inv[13] = p[0] * p[9] * p[14] - p[0] * p[10] * p[13] - p[8] * p[1] * p[14]
        + p[8] * p[2] * p[13]
        + p[12] * p[1] * p[10]
        - p[12] * p[2] * p[9];
    inv[2] = p[1] * p[6] * p[15] - p[1] * p[7] * p[14] - p[5] * p[2] * p[15]
        + p[5] * p[3] * p[14]
        + p[13] * p[2] * p[7]
        - p[13] * p[3] * p[6];
    inv[6] = -p[0] * p[6] * p[15] + p[0] * p[7] * p[14] + p[4] * p[2] * p[15]
        - p[4] * p[3] * p[14]
        - p[12] * p[2] * p[7]
        + p[12] * p[3] * p[6];
    inv[10] = p[0] * p[5] * p[15] - p[0] * p[7] * p[13] - p[4] * p[1] * p[15]
        + p[4] * p[3] * p[13]
        + p[12] * p[1] * p[7]
        - p[12] * p[3] * p[5];
    inv[14] = -p[0] * p[5] * p[14] + p[0] * p[6] * p[13] + p[4] * p[1] * p[14]
        - p[4] * p[2] * p[13]
        - p[12] * p[1] * p[6]
        + p[12] * p[2] * p[5];
    inv[3] = -p[1] * p[6] * p[11] + p[1] * p[7] * p[10] + p[5] * p[2] * p[11]
        - p[5] * p[3] * p[10]
        - p[9] * p[2] * p[7]
        + p[9] * p[3] * p[6];
    inv[7] = p[0] * p[6] * p[11] - p[0] * p[7] * p[10] - p[4] * p[2] * p[11]
        + p[4] * p[3] * p[10]
        + p[8] * p[2] * p[7]
        - p[8] * p[3] * p[6];
    inv[11] = -p[0] * p[5] * p[11] + p[0] * p[7] * p[9] + p[4] * p[1] * p[11]
        - p[4] * p[3] * p[9]
        - p[8] * p[1] * p[7]
        + p[8] * p[3] * p[5];
    inv[15] = p[0] * p[5] * p[10] - p[0] * p[6] * p[9] - p[4] * p[1] * p[10]
        + p[4] * p[2] * p[9]
        + p[8] * p[1] * p[6]
        - p[8] * p[2] * p[5];

    let det = p[0] * inv[0] + p[1] * inv[4] + p[2] * inv[8] + p[3] * inv[12];
    if det.abs() <= 1e-12 {
        return [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
    }
    let scale = 1.0 / det;
    for v in &mut inv {
        *v *= scale;
    }
    [
        [inv[0], inv[1], inv[2], inv[3]],
        [inv[4], inv[5], inv[6], inv[7]],
        [inv[8], inv[9], inv[10], inv[11]],
        [inv[12], inv[13], inv[14], inv[15]],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_invert_round_trips() {
        let m = [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 1.5, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.1, 0.0],
        ];
        let inv = invert_4x4(&m);
        let prod = mul_4x4(&m, &inv);
        for r in 0..4 {
            for c in 0..4 {
                let want = if r == c { 1.0 } else { 0.0 };
                assert!((prod[r][c] - want).abs() < 1e-5, "({r},{c}): {}", prod[r][c]);
            }
        }
    }
}