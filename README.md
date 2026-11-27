# VIctoryUnified-DLSS

NVIDIA DLSS frame generation for an Xbox Series title running natively on Windows through
[VIctory](https://github.com/Marvisak/VIctory), on top of Yaekai's 8 GB NVIDIA fork.

By [TheUnrealZaka](https://github.com/TheUnrealZaka). Licensed GPL-3.0-or-later.

**This repository contains no game files, code or assets.** You need your own copy of the title. It is not
affiliated with or endorsed by the title's developer or publisher.

## What this adds

- NVIDIA Streamline: **DLSS frame generation**, Reflex and PCL.
- The title's own FidelityFX upscale dispatch is read (depth, motion vectors, jitter, camera) and handed to
  frame generation. The console upscaler keeps running as before.
- A launcher that picks the NVIDIA GPU on hybrid laptops.

Everything else is Yaekai's 8 GB NVIDIA fork, unchanged. Details of that fork are in
[docs/README-upstream.md](docs/README-upstream.md).

## Requirements

- Windows 10/11 x64, a CPU with AVX2, hardware-accelerated GPU scheduling on.
- An **RTX 40 or 50 series** card (frame generation needs it). 8 GB of VRAM is the target.
- [Rust](https://rustup.rs) and the Visual Studio C++ build tools with a Windows SDK.
- Your own dump of the title: a folder containing `game_scarlett_master.exe`.

Without the Streamline DLLs the game still starts, with frame generation off.

## Quick start

Run these in PowerShell, inside this folder.

```powershell
# 1. Build
cargo build --release --locked -p victory

# 2. Download the DLSS runtime DLLs into .\streamline
powershell -ExecutionPolicy Bypass -File .\scripts\get-streamline.ps1

# 3. First run only: builds the shader cache (slow, about 8-10 minutes), then starts the game
powershell -ExecutionPolicy Bypass -File .\scripts\play-3070.ps1 -Game 'C:\path\to\game_scarlett_master.exe' -Prewarm -DlssFg -CardBuffersMB 1536

# 4. Every run after that
powershell -ExecutionPolicy Bypass -File .\scripts\play-3070.ps1 -Game 'C:\path\to\game_scarlett_master.exe' -DlssFg -CardBuffersMB 1536
```

On Windows 10 also run `.\scripts\get-agility-sdk.ps1` once, then rebuild.

Notes:

- Let `-Prewarm` finish. It stops by itself, at around 25,000 shaders.
- Drop `-DlssFg` to run without frame generation (a baseline to compare against).
- On cards with **12 GB or more**, add `-MipDrop 0` for full-resolution textures.
- Logs go to `runs\3070-<date>.log`. The line `host adapter` should name your NVIDIA card.

## Settings

| Setting | Effect |
|---|---|
| `VICTORY_DLSS_FG=off` | Turns frame generation off |
| `VICTORY_SL_DIR=<folder>` | Where the Streamline DLLs are |
| `VICTORY_NO_GPU_PREF=1` | Do not set the Windows GPU preference for the exe |
| `VICTORY_LOG=info` | Log level; frame generation lines need `info` |

## Status

Built and run on one machine: an RTX 5060 Laptop (8 GB), Windows 11. Other setups are untested, so please
report what you see. Known problems, such as frame generation switching on and off while depth or motion data is
missing, are listed in [docs/dlss-frame-generation.md](docs/dlss-frame-generation.md).

## Credits

This is a tree of other people's work with a small addition on top. The history is kept.

- **TheUnrealZaka**: this fork, the DLSS frame generation port.
- **Yaekai**: the 8 GB NVIDIA fork this is built on.
- **Op-lite**, **codec-d (cool_fox)** and **avatarmanz**: the forks Yaekai's work comes from.
- **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)** and **[Marvisak/VIctory](https://github.com/Marvisak/VIctory)**:
  the loader, runtime, Direct3D 12 back end and shader recompiler.
- NVIDIA Streamline and DLSS are NVIDIA's. The DLLs are downloaded from NVIDIA by the script, not shipped here.

More forks and contributors are credited in [docs/README-upstream.md](docs/README-upstream.md).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). The one file that is not covered is named in
[docs/asset-policy.md](docs/asset-policy.md).
