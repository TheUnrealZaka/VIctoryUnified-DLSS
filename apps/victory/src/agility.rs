//! D3D12 Agility SDK exports for Windows 10 compatibility.
//!
//! Windows 10's built-in `d3d12.dll` lacks support for
//! `D3D12_ROOT_SIGNATURE_FLAG_CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED` and
//! `SAMPLER_HEAP_DIRECTLY_INDEXED`. Exporting `D3D12SDKVersion` and
//! `D3D12SDKPath` from `victory.exe` tells `d3d12.dll` to load
//! `.\D3D12\D3D12Core.dll` next to the executable when present.

pub const SDK_VERSION: u32 = 614;

#[repr(transparent)]
pub struct SdkPathPtr(*const u8);

// SAFETY: points to a `'static` NUL-terminated string literal.
unsafe impl Sync for SdkPathPtr {}

#[unsafe(no_mangle)]
pub static D3D12SDKVersion: u32 = SDK_VERSION;

#[unsafe(no_mangle)]
pub static D3D12SDKPath: SdkPathPtr = SdkPathPtr(c".\\D3D12\\".as_ptr().cast());

/// Warns once at startup if `D3D12\D3D12Core.dll` is not next to `victory.exe`.
pub fn warn_if_missing() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    if !dir.join("D3D12").join("D3D12Core.dll").exists() {
        eprintln!(
            "warning: D3D12 Agility SDK not found at {} (Windows 10 needs scripts\\get-agility-sdk.ps1)",
            dir.join("D3D12").display()
        );
    }
}
