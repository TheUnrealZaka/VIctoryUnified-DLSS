use std::path::PathBuf;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/EXPORT:D3D12SDKVersion,DATA");
        println!("cargo:rustc-link-arg=/EXPORT:D3D12SDKPath,DATA");
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let src_d3d12 = repo_root.join("D3D12");
    println!("cargo:rerun-if-changed={}", src_d3d12.display());

    if !src_d3d12.is_dir() {
        return;
    }

    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out -> go up 3 levels to target/<profile>.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let Some(target_profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    let dst_d3d12 = target_profile_dir.join("D3D12");
    let _ = std::fs::create_dir_all(&dst_d3d12);
    for dll in ["D3D12Core.dll", "d3d12SDKLayers.dll"] {
        let src = src_d3d12.join(dll);
        if src.is_file() {
            let _ = std::fs::copy(&src, dst_d3d12.join(dll));
        }
    }
}
