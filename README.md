# VIctoryUnified-DLSS

**cool_fox for NVIDIA RTX cards, with DLSS frame generation.**
Fork by **TheUnrealZaka** ([github.com/TheUnrealZaka/VIctoryUnified-DLSS](https://github.com/TheUnrealZaka/VIctoryUnified-DLSS)).

This is a fork of **cool_fox** (codec_d's fork of [VIctory](https://github.com/Marvisak/VIctory),
a compatibility layer that runs an Xbox Series title on Windows from files you supply). It adds
NVIDIA Streamline **DLSS frame generation** and Reflex on top of cool_fox's 8 GB NVIDIA fixes:
correct lighting and colour, every 64-lane GPU program translated exactly, and settings measured
on RTX cards.

> **This repository contains no game files, code, assets, keys or download links.** You need your
> own complete, lawfully obtained copy of the game. NVIDIA's and AMD's DLLs are not included
> either; the scripts download NVIDIA's from NVIDIA. Not affiliated with or endorsed by the
> game's developer or publisher.

## What it does

| | |
|---|---|
| **DLSS frame generation** | NVIDIA Streamline (DLSS-G 2x) plus Reflex low latency, fed with the depth, motion vectors and jitter the title hands to its own upscaler. Off by itself when a card or DLL is missing. |
| **Correct picture** | Dark rooms, banding, blotchy skin and black boxes fixed (see [PROGRESS.md](PROGRESS.md)). |
| **Series S mode** | Works on NVIDIA through AMD's PC FSR DLL (you supply it). |
| **Speed on 8 GB cards** | Game buffers in video memory, Resizable BAR, 32-bit lighting targets, a whole-program-optimised build. |

### Results

Tested on an **RTX 5060 Laptop** (driver 610.88, Windows 11, Series S mode, the prologue); the
8 GB tuning was measured earlier on an RTX 4060 8 GB.

| | |
|---|---|
| Light scenes with frame generation | the counter shows **30 fps and above** |
| Busy scenes (common room, many draws) | about **10-15 fps before generation** (median world frame 11 fps, best 10 % of the time 14 fps or more) |
| Frame generation on | about 54 % of the time; it flips on and off when a frame has no depth or motion vectors, see [docs/dlss-frame-generation.md](docs/dlss-frame-generation.md) |

Frame generation doubles what the game already manages; it does not make a slow frame fast. Please
report your own numbers (card, driver, scene, log).

## Quick start (Windows 10/11 x64)

You need an **RTX 40 or 50 series card** for frame generation (other D3D12 cards still run, with
it off), a current NVIDIA driver, *Hardware-accelerated GPU scheduling* switched on, a CPU with
AVX2, Rust (pinned in `rust-toolchain.toml`), the MSVC build tools with a Windows SDK, and your
own dump of the title.

```powershell
git clone https://github.com/TheUnrealZaka/VIctoryUnified-DLSS
cd VIctoryUnified-DLSS

# 1. Microsoft's D3D12 Agility SDK and NVIDIA's Streamline runtime (downloaded; the Streamline DLLs are signature-checked)
powershell -ExecutionPolicy Bypass -File .\scripts\get-agility-sdk.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\get-streamline.ps1

# 2. Build (the "fast" profile is tuned to your own processor, about 7 % faster)
$env:RUSTFLAGS = '-Ctarget-cpu=native'
cargo build --profile fast --locked -p victory

# 3. Play. The first run translates every shader once (a few minutes).
.\scripts\play-nvidia-8gb.cmd "C:\path\to\your\game folder"
```

The game folder is the one with `game_scarlett_master.exe`; it is only read.

Series S mode needs AMD's PC FSR upscaler, `amd_fidelityfx_upscaler_dx12.dll`, from a PC game you
own that ships it (not included). Put its path in `VICTORY_FSR_DLL`, or the DLL in a folder named
`fsr` at the repository root. Without it the picture stays black on NVIDIA.

Run `scripts\play-nvidia-8gb-x.cmd` for the same settings **without** frame generation, to compare.

### Where things go

| | |
|---|---|
| Logs | `logs\run-<time>.log` in this repository (git-ignored). The first line says which fork made it. |
| Shader/pipeline cache and saves | `%LOCALAPPDATA%\VIctoryUnified-DLSS\{cache,data}`. An existing `cool_fox-nvidia` folder is moved there once, so nothing is rebuilt. |

Every setting in the launchers can be overridden by setting the variable before starting them
(`VICTORY_DLSS_FG=off`, `VICTORY_CARD_BUFFERS_MB=...`, `VICTORY_LOG=info`, ...).

## Troubleshooting: low fps

- **About 4 fps on a laptop with two GPUs (Intel/AMD + NVIDIA).** The game was running on the
  integrated GPU. The launchers now ask Windows for the high-performance GPU for `victory.exe`
  (`VICTORY_NO_GPU_PREF=1` skips that). Check `logs\run-*.log`: the line `host adapter` must name
  your NVIDIA card, with its dedicated VRAM. If it does not, set it by hand in Windows Settings,
  System, Display, Graphics, add `target\fast\victory.exe`, Options, High performance.
- **First run is slow for a long time.** The first session builds the pipeline cache (many
  minutes of 2-10 fps); later sessions reuse it. It is kept in `%LOCALAPPDATA%\VIctoryUnified-DLSS`.
- **Cards with more than 8 GB.** The launcher's memory settings were measured on 8 GB cards. On a
  12 GB card you can try `set VICTORY_CARD_BUFFERS_MB=3072` before starting the launcher
  (not tested yet; report what you see).
- Still slow? Send the first 60 lines of the log and the Task Manager GPU graph.

## Optional

`VICTORY_ICON=<file.ico>` sets the game window's icon from a file of your own.

## What needs to be done

Help is welcome. In rough order of importance:

1. **Frame generation flicker.** Hold it on for a few frames after the last good one instead of
   toggling on every frame without depth or motion vectors.
2. **Speed: the submitting thread.** About 40 ms of each frame is CPU preparation on one thread:
   about 20 ms copying 40 MB of shader results back to title memory, 8 ms uploading 56 MB. More
   translation threads were slower (6 was best on a 20-core Intel CPU). Ideas: share the
   copy-back across threads, skip copy-backs the title never reads (carefully: stale data breaks
   game logic), profile-guided optimisation.
3. **Speed: the card.** The card is 87-92 % busy at 720p. `VICTORY_BVH_MISS=1` saved about 2 ms
   in a first test; its effect on the picture is not checked. `VICTORY_RENDER_SCALE` is ported
   but not tuned.
4. **Stretched inmate clothing ("light slabs") and an orange shard near the guard.** Skinned
   meshes are handed back before the card has finished them. `VICTORY_RELAY=1` and
   `VICTORY_CARD_STEPS=0` remove them but cost 27 and 19 ms a frame; a targeted fix is needed.
5. **Rainbow / pixelated skin** on some characters: a texture refresh that does not wait
   (`VICTORY_TEXTURE_NO_WAIT=0` fixes it at about 4 ms), and probably the 16-bit "halves" fix
   (fxceless/victory-fork #927), not ported yet.
6. **Haze and silhouettes in the common room by its sunlit windows:** the light probes keep the
   sky's light indoors without ray tracing.
7. **The 30 fps cap:** the title's own frame interval; the "willy" cool_fox build has an unlock
   (`VICTORY_FRAME_INTERVAL_MS`), not ported yet.
8. **Ray tracing** on 8 GB cards: very slow, and the title's watchdog ends slow frames
   (`VICTORY_NO_WATCHDOG=1` patches it out in memory).

## Not included, on purpose

- The instruction emulation cool_fox ported from XCL (`console_insn.rs`), which is GPL-2.0-only
  and cannot be combined with this GPL-3.0 program. VIctory's own emulation of the same
  instructions takes its place.
- Any game file, artwork, shader, icon, or third-party DLL.

## Credits

This fork stands on a lot of other people's work; every line of it is theirs unless noted.

- **TheUnrealZaka**: this fork: merging the trees, porting and wiring DLSS frame generation
  (Streamline, Reflex, PCL) into cool_fox, the download/verify scripts, launchers, logging and
  testing on RTX hardware.
- [VIctory](https://github.com/Marvisak/VIctory) by Marvisak and contributors: the loader,
  runtime services, Direct3D 12 back end and shader recompiler.
- **cool_fox** by codec_d, and the forks it is built on: avatarmanz/VIctory-7900XTX,
  fxceless/victory-fork, DexNom, Belobug, FQY55kai, myokex (see `docs/README-cool_fox.md`).
- The **NVIDIA frame generation fork** (`victory-fork-NVIDIA-FG-2x`) and its author, for the
  first Streamline implementation this port is based on.
- **kelvaroq** (stellarr-studio): the paired-lane wave64 translation.
- **Neptune** (zm0kie and contributors): the FidelityFX redirect.
- **VIctory-NVIDIA** (trustno1-coder): pipeline wait-once, `VICTORY_BVH_MISS`, the ambient cap,
  and the in-memory title patches it took from victory-perf / ScarlettPort.
- The **"willy" cool_fox build**: the R11G11B10 lighting targets.
- NVIDIA [Streamline](https://github.com/NVIDIA-RTX/Streamline) and Microsoft's D3D12 Agility SDK,
  downloaded by the scripts, never redistributed here.

## License

GPL-3.0-or-later, as VIctory. See [LICENSE](LICENSE); the one file that is not covered is named in
[docs/asset-policy.md](docs/asset-policy.md). Copyright of this fork's additions:
TheUnrealZaka, under the same licence.
