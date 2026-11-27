@echo off
rem VIctoryUnified-DLSS (by TheUnrealZaka): cool_fox for 8 GB NVIDIA cards + DLSS frame generation. Series S mode with the settings measured on an RTX 4060 8 GB
rem (see README.md and PROGRESS.md).
rem   scripts\play-nvidia-8gb.cmd "D:\path\to\your\game"        the folder with game_scarlett_master.exe
rem   scripts\play-nvidia-8gb.cmd "D:\path\to\your\game" warm   translate every shader first, then play
rem Every setting below can be overridden by setting the variable before starting this file.
rem The game folder is only read; caches and saves go to %LOCALAPPDATA%\VIctoryUnified-DLSS,
rem logs go to the "logs" folder of this repository (VIctoryUnified-DLSS\logs).
setlocal EnableExtensions
set "root=%~dp0.."

if not "%~1"=="" set "VICTORY_GAME_DIR=%~1"
if not defined VICTORY_GAME_DIR (
  echo Usage: %~nx0 "folder of your own game dump" [warm] 1>&2
  exit /b 2
)
if not exist "%VICTORY_GAME_DIR%\game_scarlett_master.exe" (
  echo No game_scarlett_master.exe in "%VICTORY_GAME_DIR%". 1>&2
  exit /b 2
)

rem The runtime: the fast build if there is one, else the release build.
set "runner=%root%\target\fast\victory.exe"
if not exist "%runner%" set "runner=%root%\target\release\victory.exe"
if defined VICTORY_RUNNER set "runner=%VICTORY_RUNNER%"
if not exist "%runner%" (
  echo Build it first: cargo build --release --locked -p victory 1>&2
  exit /b 2
)

rem Laptops with two GPUs (Intel/AMD iGPU + NVIDIA): Windows would run victory.exe on the iGPU (about 4 fps).
rem This asks Windows for the high-performance GPU for this exe only. VICTORY_NO_GPU_PREF=1 skips it.
if not defined VICTORY_NO_GPU_PREF for %%I in ("%runner%") do reg add "HKCU\Software\Microsoft\DirectX\UserGpuPreferences" /v "%%~fI" /t REG_SZ /d "GpuPreference=2;" /f >nul 2>&1
rem One-time move of the old cool_fox-nvidia cache and saves, so nothing has to be rebuilt.
if exist "%LOCALAPPDATA%\cool_fox-nvidia\data" if not exist "%LOCALAPPDATA%\VIctoryUnified-DLSS\data" (
  mkdir "%LOCALAPPDATA%\VIctoryUnified-DLSS" 2>nul
  move "%LOCALAPPDATA%\cool_fox-nvidia\cache" "%LOCALAPPDATA%\VIctoryUnified-DLSS\cache" >nul 2>&1
  move "%LOCALAPPDATA%\cool_fox-nvidia\data" "%LOCALAPPDATA%\VIctoryUnified-DLSS\data" >nul 2>&1
)
if not defined VICTORY_CACHE_DIR set "VICTORY_CACHE_DIR=%LOCALAPPDATA%\VIctoryUnified-DLSS\cache"
if not defined VICTORY_DATA_DIR set "VICTORY_DATA_DIR=%LOCALAPPDATA%\VIctoryUnified-DLSS\data"
set "logs=%root%\logs"
if not exist "%logs%" mkdir "%logs%"

rem --- cool_fox's pareto-residency set (codec_d) ---
if not defined VICTORY_AMBIENT_AIR set "VICTORY_AMBIENT_AIR=1.5"
if not defined VICTORY_TEXTURE_NO_WAIT set "VICTORY_TEXTURE_NO_WAIT=1"
if not defined VICTORY_RELAY set "VICTORY_RELAY=2"
if not defined VICTORY_WIDE_BACK set "VICTORY_WIDE_BACK=1"
if not defined VICTORY_CARD_REHASH_RUNS set "VICTORY_CARD_REHASH_RUNS=1"
if not defined VICTORY_JOIN_COPIES set "VICTORY_JOIN_COPIES=1"
if not defined VICTORY_INDIRECT_WINDOW set "VICTORY_INDIRECT_WINDOW=1"
if not defined VICTORY_RELAY_COPY set "VICTORY_RELAY_COPY=1"
if not defined VICTORY_CARD_STEPS set "VICTORY_CARD_STEPS=2"
if not defined VICTORY_INDIRECT_BATCH set "VICTORY_INDIRECT_BATCH=2"
if not defined VICTORY_MAP_THREADS set "VICTORY_MAP_THREADS=1"
if not defined VICTORY_CARD_AWAKE set "VICTORY_CARD_AWAKE=1"
if not defined VICTORY_SHOW_AFTER set "VICTORY_SHOW_AFTER=1"
if not defined VICTORY_AMBIENT set "VICTORY_AMBIENT=0.8"
if not defined VICTORY_THIN set "VICTORY_THIN=1"
if not defined VICTORY_JOIN_PARTS set "VICTORY_JOIN_PARTS=8"
if not defined VICTORY_UPLOAD_AHEAD set "VICTORY_UPLOAD_AHEAD=1"
if not defined VICTORY_CARD_BUFFERS_MIN_KB set "VICTORY_CARD_BUFFERS_MIN_KB=64"

rem --- for 8 GB NVIDIA cards (measured on an RTX 4060 8 GB) ---
rem Off: with it on, rooms went dark and light fell off in rings (same speed without it).
if not defined VICTORY_LEVELS_ON_DEMAND set "VICTORY_LEVELS_ON_DEMAND=0"
rem 64-lane GPU programs translated exactly (no banding, blotches or black boxes).
if not defined VICTORY_WAVE64_PAIRED set "VICTORY_WAVE64_PAIRED=1"
rem 16-bit float results rounded as the console rounds them.
if not defined VICTORY_F16_NEAREST set "VICTORY_F16_NEAREST=1"
rem Game buffers in the card's memory: about twice the frame rate on 8 GB.
if not defined VICTORY_CARD_BUFFERS_MB set "VICTORY_CARD_BUFFERS_MB=1536"
rem With Resizable BAR on in the BIOS: the title's own allocations written straight to the card.
if not defined VICTORY_WINDOWS_BAR_MB set "VICTORY_WINDOWS_BAR_MB=1024"
rem The title's R9G9B9E5 lighting targets as 32-bit R11G11B10_FLOAT (-15 %).
if not defined VICTORY_E5_TARGETS set "VICTORY_E5_TARGETS=r11g11b10"
rem 6 threads translating draw calls (8 by default): fewer leave the fast cores to the submitting
rem thread (cell: 8 -> 60.2 ms, 6 -> 50.2, 12 -> 63.1, 16 -> 71.5 on an i7-14700F).
if not defined VICTORY_THIN_THREADS set "VICTORY_THIN_THREADS=6"
rem Threads kept for the copies the submitting thread waits on (foxhound: 13.7 to 18.1 fps on a
rem 12-thread CPU; 8 was slower). Only with 12 or more logical processors; override or test with
rem set VICTORY_WIDE_THREADS=<n> before starting this file.
if not defined VICTORY_WIDE_THREADS if %NUMBER_OF_PROCESSORS% GEQ 12 set "VICTORY_WIDE_THREADS=6"

rem Series S upscaling through AMD's PC FSR DLL (from a PC game you own; not included).
if not defined VICTORY_FSR_DLL if exist "%root%\fsr\amd_fidelityfx_upscaler_dx12.dll" set "VICTORY_FSR_DLL=%root%\fsr\amd_fidelityfx_upscaler_dx12.dll"
if not defined VICTORY_FSR_DLL echo WARNING: no FSR DLL (VICTORY_FSR_DLL or fsr\amd_fidelityfx_upscaler_dx12.dll): on NVIDIA the picture may stay black. 1>&2
if not defined VICTORY_LOG if not defined RUST_LOG set "VICTORY_LOG=warn"

if /i "%~2"=="warm" goto warm
if not exist "%VICTORY_CACHE_DIR%\shaders" goto warm
goto play

:warm
echo Translating every shader into the cache (a few minutes, once per build)...
"%runner%" shaders --jobs 6 "%VICTORY_GAME_DIR%"

:play
for /f "tokens=1-3 delims=/:. " %%a in ("%time: =0%") do set "stamp=%%a%%b%%c"
set "log=%logs%\run-%stamp%.log"
echo VIctoryUnified-DLSS by TheUnrealZaka ^| github.com/TheUnrealZaka/VIctoryUnified-DLSS
echo Starting. Log: %log%
"%runner%" run "%VICTORY_GAME_DIR%\game_scarlett_master.exe" --console series-s --execute 2> "%log%"
exit /b %errorlevel%
