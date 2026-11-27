# DLSS frame generation (NVIDIA Streamline)

Ported from the `victory-fork-NVIDIA-FG-2x` fork (commit "First implementation of streamline"
frame generation (NVIDIA)") and adapted for this tree by TheUnrealZaka. Tested on an RTX 5060 Laptop
(driver 610.88, Windows 11, Series S mode): Streamline reports `eOk` for DLSS-G, Reflex and PCL, and
frame generation runs in gameplay. Known issue: it switches on and off many times a minute
whenever the frame has no depth or motion-vector buffer tagged (see "Known issues" below).

## What it needs

* An RTX 40 or 50 series card, a current driver, and Windows' hardware-accelerated GPU
  scheduling switched on.
* The NVIDIA Streamline runtime DLLs from the Streamline SDK (production build):
  `sl.interposer.dll`, `sl.common.dll`, `sl.dlss_g.dll`, `sl.reflex.dll`, `sl.pcl.dll`, and
  `nvngx_dlssg.dll`. Put them beside `victory.exe`, or in a folder named by `VICTORY_SL_DIR` /
  `[dlss_fg] runtime_dir` (`Play-VIctory.cmd` also looks in a `streamline` folder beside it).
  Missing DLLs, an unsupported card or a failed Streamline start only log a warning: the game
  runs as before with frame generation off.

## Switching it on

* `Play-VIctory.cmd` and `scripts/play-nvidia-8gb.cmd` set `VICTORY_DLSS_FG=on` unless you set it.
* Otherwise: `victory run <exe> --execute --dlss-fg on` (also `--dlss-fg-frames 1..5`,
  `--dlss-fg-runtime prod|dev`, `--dlss-fg-probe`), `scripts/play.ps1 -DlssFg`, the `[dlss_fg]`
  table of `victory.toml` (see `victory.example.toml`), or the `VICTORY_DLSS_FG*` variables
  (`VICTORY_DLSS_FG`, `_FRAMES`, `_RUNTIME`, `_PROBE`, `_MVEC_UNITS`, `_MVEC_SCALE`,
  `_RENDER_SIZE`, `_DEPTH_INVERTED`, `_HEURISTIC`, `_UI_RECOMP`, `VICTORY_SL_DIR`).

## Where its inputs come from

* Series S mode (`--console series-s`): the title's FidelityFX upscale dispatch is read (through
  the PC FSR redirect `VICTORY_FSR_DLL`, or a pass-through spy on the title's own Xbox DLL) for
  depth, motion vectors, jitter, camera planes and the HUD-less colour.
* Otherwise a heuristic picks a reversed-Z depth target and an RG16 motion-vector target from
  the draws; `--dlss-fg-probe` logs the candidates, and `[[dlss_fg.hooks.rules]]` pins them.
* The window caption and the log show the frame generation status once a second.

## Known issues

* **On/off flicker.** In a 33-minute run (RTX 5060 Laptop) `slDLSSGSetOptions` was called 6,283
  times: 4,155 to switch it on ("active tagged gameplay frame"), 1,387 to switch it off ("no
  MotionVectors buffer tagged") and 741 to switch it off ("no Depth buffer tagged"). It was on
  about 54 % of the time. Frames where the title does not run its upscale pass have no depth or
  motion vectors, so Streamline is told to stand down. Smoothing this (keep it on for a few
  frames after the last good one) is the next thing to try.
* **It doubles what the game manages; it does not fix a slow frame.** In heavy scenes the
  median world frame was about 11 fps before generation (best 10 % of windows 14 fps or more).
  Light scenes reach 30 fps and above on the counter.
* Needs hardware-accelerated GPU scheduling, an RTX 40/50 card and a current driver.
