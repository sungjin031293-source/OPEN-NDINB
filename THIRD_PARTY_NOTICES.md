# Third-party notices

OPEN-NDINB's own source code is MIT licensed (see `LICENSE`). It talks to several third-party
components. None of the proprietary SDK runtimes are included in this repository or in the release
`.exe`; they are loaded at runtime from the user's own installation.

## Not bundled - installed by the user

| Component | How OPEN-NDINB uses it | Where to get it |
|---|---|---|
| **NDI® 6 Runtime** (Vizrt NDI AB) | Loaded dynamically (`Processing.NDI.Lib.x64.dll`) to send/receive NDI | https://ndi.link/NDIRedistV6 |
| **NVIDIA Maxine Video Effects runtime** (AI Green Screen) | Loaded dynamically (`NVVideoEffects.dll`, `NVCVImage.dll`) | https://www.nvidia.com/en-us/geforce/broadcasting/broadcast-sdk/resources/ |
| **Blackmagic Desktop Video** (DeckLink driver) | Used through its COM API, only for DeckLink input | https://www.blackmagicdesign.com/support/family/capture-and-playback |

## Header / interface files in `vendor/`

These are the public, permissively licensed interface files of each SDK, needed to compile.
Their original license headers are kept intact in each file.

- `vendor/ndi/Processing.NDI.*.h` - Copyright Vizrt NDI AB. MIT license applies to these header files only
  (as stated in the files). Use of the NDI SDK/runtime is subject to the NDI SDK license: http://ndi.link/ndisdk_license
- `vendor/maxine/*.h` - Copyright NVIDIA Corporation, MIT license (from https://github.com/NVIDIA-Maxine/Maxine-VFX-SDK).
  Use of the Maxine runtime is subject to the NVIDIA Maxine EULA.
- `vendor/decklink/*.idl`, `DeckLinkAPIVersion.h` - Copyright Blackmagic Design, permissive license as stated in each file.

## FFmpeg (bundled in the release exe, LGPL v3)

The release `OPEN-NDINB.exe` contains an **unmodified** LGPL v3 build of FFmpeg's
`avformat`, `avcodec`, `avutil`, `swscale` and `swresample` libraries
(FFmpeg 9.0.2, MSVC x64 shared LGPL build from https://github.com/System233/ffmpeg-msvc-prebuilt).
They are used to decode ProRes 4444, Resolume DXV/DXV3 and other formats Windows cannot decode.

- The libraries are stored compressed inside the exe and, on first run, written as **separate DLLs** to
  `%LOCALAPPDATA%\OPEN-NDINB\lav-<hash>\` together with the FFmpeg license text, then loaded dynamically.
- **You can replace them** with your own build of the same FFmpeg major versions: set the environment
  variable `OPEN_NDINB_FFMPEG_DLL_DIR` to a folder containing `avutil-61.dll`, `swresample-7.dll`,
  `swscale-10.dll`, `avcodec-63.dll`, `avformat-63.dll` and OPEN-NDINB will load those instead.
- FFmpeg source code: https://ffmpeg.org/download.html (release 9.0.2), build scripts:
  https://github.com/System233/ffmpeg-msvc-prebuilt
- License text: https://www.gnu.org/licenses/lgpl-3.0.html

FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project.

## Rust crates

Rust dependencies (eframe/egui, libloading, rfd, png, miniz_oxide, serde_json, windows, ...) are
MIT and/or Apache-2.0 licensed; see `Cargo.lock` and each crate's license.

## Trademarks

NDI® is a registered trademark of Vizrt NDI AB. NVIDIA, NVIDIA Broadcast and NVIDIA Maxine are
trademarks of NVIDIA Corporation. Blackmagic Design and DeckLink are trademarks of Blackmagic Design
Pty. Ltd. Resolume and DXV are trademarks of Resolume. Apple ProRes is a trademark of Apple Inc.
OPEN-NDINB is an independent project and is not affiliated with or endorsed by any of them.
