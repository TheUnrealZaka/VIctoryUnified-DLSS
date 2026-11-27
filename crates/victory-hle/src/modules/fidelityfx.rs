//! Reads the title's FidelityFX upscale dispatches for DLSS frame generation.
//!
//! The title's own `amd_fidelityfx_*_xbox.dll` is left to run (Series S FSR
//! upscaling is unchanged); its exports are only wrapped so that every
//! `ffxDispatch` of an upscale also tells the DLSS hooks which textures hold
//! the depth and motion vectors, and the jitter and camera values.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// The title DLLs whose exports are wrapped.
const DLLS: [&str; 3] = [
    "amd_fidelityfx_loader_xbox.dll",
    "amd_fidelityfx_upscaler_xbox.dll",
    "amd_fidelityfx_framegeneration_xbox.dll",
];

/// Whether `file` is one of the console FidelityFX DLLs.
pub fn is_xbox_dll(file: &str) -> bool {
    DLLS.iter().any(|d| d.eq_ignore_ascii_case(file))
}

/// The loader is the DLL the title calls. It looks the provider DLLs'
/// (upscaler, frame generation) exports of the same names up itself, and
/// those must stay as they are: wrapping them sends the title's calls
/// straight to a provider, past the loader, which crashes it.
const LOADER: &str = "amd_fidelityfx_loader_xbox.dll";

// ffxReturnCode_t
const ERROR: u32 = 1;

// Description types (`ffx_api_types.h`, `ffx_upscale.h`).
const DISPATCH_UPSCALE: u64 = 0x1_0001;
const CREATE_DESC_UPSCALE: u64 = 0x1_0000;
const CREATE_FLAG_ENABLE_DEPTH_INVERTED: u32 = 1 << 2;

type Create = unsafe extern "system" fn(*mut *mut c_void, *mut u8, *const c_void) -> u32;
type Destroy = unsafe extern "system" fn(*mut *mut c_void, *const c_void) -> u32;
type Header = unsafe extern "system" fn(*mut *mut c_void, *mut u8) -> u32;

// Each `FfxApiResource` is a pointer, a 32-byte description and a state: 48 bytes.
const RESOURCE_SIZE: usize = 48;
const COMMAND_LIST: usize = 16;

// Offsets in `ffxDispatchDescUpscale` (`ffx_upscale.h`, 64-bit layout):
//   0..16  : ffxApiHeader (type u64, pNext *mut)
//  16..24  : commandList
//  24..72  : color (48)
//  72..120 : depth (48)
// 120..168 : motionVectors (48)
// 168..216 : exposure (48)
// 216..264 : reactive (48)
// 264..312 : transparencyAndComposition (48)
// 312..360 : output (48)
// 360..368 : jitterOffset (f32, f32)
// 368..376 : motionVectorScale (f32, f32)
// 376..384 : renderSize (u32, u32)
// 384..392 : upscaleSize (u32, u32)
// 392..396 : enableSharpening (bool/u32)
// 396..400 : sharpness (f32)
// 400..404 : frameTimeDelta (f32)
// 404..408 : preExposure (f32)
// 408..412 : reset (bool/u8)
// 412..416 : cameraNear (f32)
// 416..420 : cameraFar (f32)
// 420..424 : cameraFovAngleVertical (f32)

static DEPTH_INVERTED: AtomicBool = AtomicBool::new(true);

fn inspect_create_flags(head: *const u8) {
    let mut node = head;
    for _ in 0..16 {
        if node.is_null() {
            break;
        }
        // SAFETY: `ffxApiHeader` starts with (u64 type, *const u8 next).
        let (ty, next) = unsafe {
            (
                node.cast::<u64>().read_unaligned(),
                node.add(8).cast::<*const u8>().read_unaligned(),
            )
        };
        if ty == CREATE_DESC_UPSCALE {
            // `flags` u32 sits right after header (16) + maxRenderSize (8) + maxUpscaleSize (8) = 32.
            // SAFETY: inside `ffxCreateContextDescUpscale`.
            let flags = unsafe { node.add(32).cast::<u32>().read_unaligned() };
            DEPTH_INVERTED.store(flags & CREATE_FLAG_ENABLE_DEPTH_INVERTED != 0, Ordering::Relaxed);
        }
        node = next;
    }
}

fn harvest_dispatch_for_fg(desc: *const u8) {
    if desc.is_null() {
        return;
    }
    // SAFETY: `ffxApiHeader` starts with u64 type.
    let ty = unsafe { desc.cast::<u64>().read_unaligned() };
    if ty != DISPATCH_UPSCALE {
        return;
    }
    // SAFETY: reads the `ffxDispatchDescUpscale` console struct passed by the title.
    unsafe {
        let list = desc.add(COMMAND_LIST).cast::<*const c_void>().read_unaligned();
        let depth = desc.add(24 + RESOURCE_SIZE).cast::<*const c_void>().read_unaligned();
        let mvec = desc.add(24 + 2 * RESOURCE_SIZE).cast::<*const c_void>().read_unaligned();
        let output = desc.add(24 + 6 * RESOURCE_SIZE).cast::<*const c_void>().read_unaligned();
        let jx = desc.add(360).cast::<f32>().read_unaligned();
        let jy = desc.add(364).cast::<f32>().read_unaligned();
        let sx = desc.add(368).cast::<f32>().read_unaligned();
        let sy = desc.add(372).cast::<f32>().read_unaligned();
        let rw = desc.add(376).cast::<u32>().read_unaligned();
        let rh = desc.add(380).cast::<u32>().read_unaligned();
        let reset = desc.add(408).cast::<u8>().read_unaligned() != 0;
        let near = desc.add(412).cast::<f32>().read_unaligned();
        let far = desc.add(416).cast::<f32>().read_unaligned();
        let fov = desc.add(420).cast::<f32>().read_unaligned();
        victory_gpu::hooks::record_fidelityfx_dispatch(
            list,
            depth,
            mvec,
            output,
            [jx, jy],
            [sx, sy],
            [rw, rh],
            near,
            far,
            fov,
            reset,
            DEPTH_INVERTED.load(Ordering::Relaxed),
        );
    }
}

static XBOX_CREATE: AtomicUsize = AtomicUsize::new(0);
static XBOX_DESTROY: AtomicUsize = AtomicUsize::new(0);
static XBOX_CONFIGURE: AtomicUsize = AtomicUsize::new(0);
static XBOX_QUERY: AtomicUsize = AtomicUsize::new(0);
static XBOX_DISPATCH: AtomicUsize = AtomicUsize::new(0);

extern "system" fn spy_create(ctx: *mut *mut c_void, desc: *mut u8, alloc: *const c_void) -> u32 {
    inspect_create_flags(desc);
    let target = XBOX_CREATE.load(Ordering::Acquire);
    if target == 0 {
        return ERROR;
    }
    // SAFETY: the real mapped Xbox FidelityFX `ffxCreateContext`.
    let f: Create = unsafe { std::mem::transmute(target) };
    // SAFETY: forwards the caller's arguments unchanged.
    unsafe { f(ctx, desc, alloc) }
}

extern "system" fn spy_destroy(ctx: *mut *mut c_void, alloc: *const c_void) -> u32 {
    let target = XBOX_DESTROY.load(Ordering::Acquire);
    if target == 0 {
        return ERROR;
    }
    // SAFETY: the real mapped Xbox FidelityFX `ffxDestroyContext`.
    let f: Destroy = unsafe { std::mem::transmute(target) };
    // SAFETY: forwards the caller's arguments unchanged.
    unsafe { f(ctx, alloc) }
}

extern "system" fn spy_configure(ctx: *mut *mut c_void, desc: *mut u8) -> u32 {
    let target = XBOX_CONFIGURE.load(Ordering::Acquire);
    if target == 0 {
        return ERROR;
    }
    // SAFETY: the real mapped Xbox FidelityFX `ffxConfigure`.
    let f: Header = unsafe { std::mem::transmute(target) };
    // SAFETY: forwards the caller's arguments unchanged.
    unsafe { f(ctx, desc) }
}

extern "system" fn spy_query(ctx: *mut *mut c_void, desc: *mut u8) -> u32 {
    let target = XBOX_QUERY.load(Ordering::Acquire);
    if target == 0 {
        return ERROR;
    }
    // SAFETY: the real mapped Xbox FidelityFX `ffxQuery`.
    let f: Header = unsafe { std::mem::transmute(target) };
    // SAFETY: forwards the caller's arguments unchanged.
    unsafe { f(ctx, desc) }
}

extern "system" fn spy_dispatch(ctx: *mut *mut c_void, desc: *mut u8) -> u32 {
    harvest_dispatch_for_fg(desc);
    let target = XBOX_DISPATCH.load(Ordering::Acquire);
    if target == 0 {
        return ERROR;
    }
    // SAFETY: the real mapped Xbox FidelityFX `ffxDispatch`.
    let f: Header = unsafe { std::mem::transmute(target) };
    // SAFETY: forwards the caller's arguments unchanged.
    unsafe { f(ctx, desc) }
}

/// Wraps the exports of the title's own mapped `amd_fidelityfx_loader_xbox.dll`,
/// so the console FSR keeps running while the depth, motion vectors, jitter
/// and camera values of each upscale are read for DLSS frame generation.
pub fn wrap_xbox_export(dll: &str, name: &str, real_addr: usize) -> usize {
    if !dll.eq_ignore_ascii_case(LOADER) || real_addr == 0 {
        return real_addr;
    }
    let (slot, wrapper): (&AtomicUsize, usize) = match name {
        "ffxCreateContext" => (&XBOX_CREATE, spy_create as *const () as usize),
        "ffxDestroyContext" => (&XBOX_DESTROY, spy_destroy as *const () as usize),
        "ffxConfigure" => (&XBOX_CONFIGURE, spy_configure as *const () as usize),
        "ffxQuery" => (&XBOX_QUERY, spy_query as *const () as usize),
        "ffxDispatch" => (&XBOX_DISPATCH, spy_dispatch as *const () as usize),
        _ => return real_addr,
    };
    if real_addr == wrapper {
        return real_addr;
    }
    slot.store(real_addr, Ordering::Release);
    wrapper
}
