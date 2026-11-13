//! Runtime and driver detection for NDI 6, NVIDIA RTX GPU driver,
//! NVIDIA Maxine Video Effects (VFX) SDK redistributable, and Blackmagic Desktop Video.

pub const URL_NDI_INSTALLER: &str = "https://ndi.link/NDIRedistV6";
pub const URL_NDI_TOOLS: &str = "https://ndi.video/tools/";
pub const URL_NVIDIA_DRIVER: &str = "https://www.nvidia.com/Download/index.aspx";
pub const URL_MAXINE_RESOURCES: &str =
    "https://www.nvidia.com/en-us/geforce/broadcasting/broadcast-sdk/resources/";
pub const URL_MAXINE_TURING: &str =
    "https://international.download.nvidia.com/Windows/broadcast/sdk/VFX/nvidia_video_effects_sdk_installer_v0.7.6_turing.exe";
pub const URL_MAXINE_AMPERE: &str =
    "https://international.download.nvidia.com/Windows/broadcast/sdk/VFX/nvidia_video_effects_sdk_installer_v0.7.6_ampere.exe";
pub const URL_MAXINE_ADA: &str =
    "https://international.download.nvidia.com/Windows/broadcast/sdk/VFX/nvidia_video_effects_sdk_installer_v0.7.6_ada.exe";
pub const URL_MAXINE_BLACKWELL: &str =
    "https://international.download.nvidia.com/Windows/broadcast/sdk/VFX/nvidia_video_effects_sdk_installer_v0.7.6_blackwell.exe";
pub const URL_BLACKMAGIC_SUPPORT: &str =
    "https://www.blackmagicdesign.com/support/family/capture-and-playback";

#[derive(Clone, Debug)]
pub struct Item {
    pub key: &'static str,
    pub name: String,
    pub ok: bool,
    pub required: bool,
    pub detail: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuGen {
    Turing,
    Ampere,
    Ada,
    Blackwell,
    Unknown,
}

impl GpuGen {
    pub fn from_name(name: &str) -> Self {
        let u = name.to_ascii_uppercase();
        if u.contains("RTX 50")
            || u.contains("RTX50")
            || u.contains("BLACKWELL")
            || u.contains("RTX PRO 6000")
            || u.contains("RTX PRO 5000")
            || u.contains("RTX PRO 4500")
            || u.contains("RTX PRO 4000")
        {
            return GpuGen::Blackwell;
        }
        if u.contains("RTX 40")
            || u.contains("RTX40")
            || u.contains("ADA")
            || u.contains("L40")
            || u.contains("L4 ")
            || u.ends_with(" L4")
            || u.contains("L20")
            || u.contains("RTX 6000 ADA")
            || u.contains("RTX 5880")
            || u.contains("RTX 5000 ADA")
            || u.contains("RTX 4500 ADA")
            || u.contains("RTX 4000 ADA")
            || u.contains("RTX 2000 ADA")
        {
            return GpuGen::Ada;
        }
        if u.contains("RTX 30")
            || u.contains("RTX30")
            || u.contains("AMPERE")
            || u.contains("RTX A2000")
            || u.contains("RTX A3000")
            || u.contains("RTX A4000")
            || u.contains("RTX A4500")
            || u.contains("RTX A5000")
            || u.contains("RTX A5500")
            || u.contains("RTX A6000")
            || u.contains("A100")
            || u.contains("A40")
            || u.contains("A30")
            || u.contains("A16")
            || u.contains("A10")
            || u.contains("A2 ")
        {
            return GpuGen::Ampere;
        }
        if u.contains("RTX 20")
            || u.contains("RTX20")
            || u.contains("TURING")
            || u.contains("QUADRO RTX")
            || u.contains("TITAN RTX")
            || u.contains("TESLA T4")
            || u.contains(" T4")
            || u.contains("T1000")
            || u.contains("T2000")
            || u.contains("T600")
            || u.contains("T400")
        {
            return GpuGen::Turing;
        }
        GpuGen::Unknown
    }

    pub fn label(self) -> &'static str {
        match self {
            GpuGen::Turing => "Turing (RTX 20)",
            GpuGen::Ampere => "Ampere (RTX 30)",
            GpuGen::Ada => "Ada (RTX 40)",
            GpuGen::Blackwell => "Blackwell (RTX 50)",
            GpuGen::Unknown => "RTX",
        }
    }

    pub fn maxine_url(self) -> &'static str {
        match self {
            GpuGen::Turing => URL_MAXINE_TURING,
            GpuGen::Ampere => URL_MAXINE_AMPERE,
            GpuGen::Ada => URL_MAXINE_ADA,
            GpuGen::Blackwell => URL_MAXINE_BLACKWELL,
            GpuGen::Unknown => URL_MAXINE_RESOURCES,
        }
    }
}

pub fn gpu_name() -> Option<String> {
    #[cfg(windows)]
    {
        imp::gpu_name()
    }
    #[cfg(not(windows))]
    {
        Some("Simulated RTX GPU".into())
    }
}

pub fn check_all() -> Vec<Item> {
    let mut items = check_all_real();
    if let Ok(sim) = std::env::var("NDI_MATTING_SIMULATE_MISSING") {
        for it in items.iter_mut().filter(|it| sim.split(',').any(|k| k.trim() == it.key)) {
            it.ok = false;
            it.detail = "설치되어 있지 않습니다 (테스트용 누락 표시)".into();
        }
    }
    items
}

fn check_all_real() -> Vec<Item> {
    #[cfg(windows)]
    {
        imp::check_all()
    }
    #[cfg(not(windows))]
    {
        vec![
            Item {
                key: "ndi",
                name: "NDI 6 런타임".into(),
                ok: true,
                required: true,
                detail: "NDI 6 런타임 확인됨 (비-Windows 환경)".into(),
                url: URL_NDI_INSTALLER.into(),
            },
            Item {
                key: "gpu",
                name: "NVIDIA RTX 드라이버".into(),
                ok: true,
                required: true,
                detail: "NVIDIA CUDA 드라이버 확인됨 (비-Windows 환경)".into(),
                url: URL_NVIDIA_DRIVER.into(),
            },
            Item {
                key: "maxine",
                name: "NVIDIA Maxine VFX 런타임".into(),
                ok: true,
                required: true,
                detail: "Maxine Video Effects SDK 확인됨 (비-Windows 환경)".into(),
                url: URL_MAXINE_RESOURCES.into(),
            },
            Item {
                key: "decklink",
                name: "Blackmagic Desktop Video".into(),
                ok: true,
                required: false,
                detail: "DeckLink 드라이버 확인됨 (비-Windows 환경)".into(),
                url: URL_BLACKMAGIC_SUPPORT.into(),
            },
        ]
    }
}

pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        imp::open_url(url);
    }
    #[cfg(not(windows))]
    {
        let _ = url;
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::path::Path;
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE,
        KEY_READ, REG_SZ, REG_VALUE_TYPE,
    };
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide_null(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn read_reg_sz(root: HKEY, subkey: &str, value: &str) -> Option<String> {
        let sub_w = wide_null(subkey);
        let val_w = wide_null(value);
        let mut hkey = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                root,
                PCWSTR(sub_w.as_ptr()),
                Some(0),
                KEY_READ,
                &mut hkey,
            )
        };
        if status.is_err() {
            return None;
        }
        let mut ty = REG_VALUE_TYPE::default();
        let mut buf = vec![0u8; 512];
        let mut cb = buf.len() as u32;
        let q = unsafe {
            RegQueryValueExW(
                hkey,
                PCWSTR(val_w.as_ptr()),
                None,
                Some(&mut ty),
                Some(buf.as_mut_ptr()),
                Some(&mut cb),
            )
        };
        let _ = unsafe { RegCloseKey(hkey) };
        if q.is_err() || ty != REG_SZ || cb < 2 {
            return None;
        }
        let u16_len = (cb as usize) / 2;
        let slice = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u16, u16_len) };
        let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
        let s = String::from_utf16_lossy(&slice[..end]).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    fn reg_key_exists(root: HKEY, subkey: &str) -> bool {
        let sub_w = wide_null(subkey);
        let mut hkey = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                root,
                PCWSTR(sub_w.as_ptr()),
                Some(0),
                KEY_READ,
                &mut hkey,
            )
        };
        if status.is_ok() {
            let _ = unsafe { RegCloseKey(hkey) };
            true
        } else {
            false
        }
    }

    pub fn gpu_name() -> Option<String> {
        let base = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
        let mut fallback: Option<String> = None;
        for idx in 0..32 {
            let sub = format!(r"{base}\{idx:04}");
            let Some(desc) = read_reg_sz(HKEY_LOCAL_MACHINE, &sub, "DriverDesc") else {
                continue;
            };
            let upper = desc.to_ascii_uppercase();
            if upper.contains("NVIDIA") {
                return Some(desc);
            }
            if fallback.is_none()
                && !upper.contains("MICROSOFT BASIC")
                && !upper.contains("VIRTUAL")
                && !upper.contains("REMOTE")
            {
                fallback = Some(desc);
            }
        }
        fallback
    }

    fn check_ndi() -> Item {
        let mut candidates = Vec::new();
        if let Ok(dir) = std::env::var("NDI_RUNTIME_DIR_V6") {
            let trimmed = dir.trim_end_matches(['\\', '/']);
            if !trimmed.is_empty() {
                candidates.push(format!(r"{trimmed}\Processing.NDI.Lib.x64.dll"));
            }
        }
        candidates.push(r"C:\Program Files\NDI\NDI 6 Runtime\v6\Processing.NDI.Lib.x64.dll".to_string());

        let found = candidates.iter().find(|p| Path::new(p.as_str()).is_file()).cloned();
        let (ok, detail) = match found {
            Some(path) => (true, format!("설치됨 ({path})")),
            None => (
                false,
                "미설치 — NDI 영상 송수신에 필요 (Processing.NDI.Lib.x64.dll 없음)".to_string(),
            ),
        };
        Item {
            key: "ndi",
            name: "NDI 6 런타임".to_string(),
            ok,
            required: true,
            detail,
            url: URL_NDI_INSTALLER.to_string(),
        }
    }

    fn check_gpu(detected_gpu: Option<&str>) -> Item {
        let cuda_ok = unsafe { libloading::Library::new("nvcuda.dll") }.is_ok();
        let gpu_label = detected_gpu.unwrap_or("NVIDIA GPU 미감지");
        let is_rtx = detected_gpu
            .map(|n| {
                let u = n.to_ascii_uppercase();
                u.contains("RTX") || GpuGen::from_name(n) != GpuGen::Unknown
            })
            .unwrap_or(false);
        let ok = cuda_ok && is_rtx;
        let detail = if ok {
            format!("설치됨 ({gpu_label}, nvcuda.dll 정상)")
        } else if !cuda_ok {
            format!("미설치 — AI 누끼 연산용 NVIDIA 드라이버(nvcuda.dll) 필요 ({gpu_label})")
        } else {
            format!("NVIDIA RTX GPU가 필요합니다 (감지됨: {gpu_label})")
        };
        Item {
            key: "gpu",
            name: "NVIDIA RTX 드라이버".to_string(),
            ok,
            required: true,
            detail,
            url: URL_NVIDIA_DRIVER.to_string(),
        }
    }

    fn maxine_sdk_dir() -> String {
        if let Ok(p) = std::env::var("NV_VIDEO_EFFECTS_PATH") {
            let trimmed = p.trim();
            if !trimmed.is_empty() && trimmed != "USE_APP_PATH" {
                return trimmed.trim_end_matches(['\\', '/']).to_string();
            }
        }
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
        format!(r"{}\NVIDIA Corporation\NVIDIA Video Effects", pf.trim_end_matches(['\\', '/']))
    }

    fn check_maxine(detected_gpu: Option<&str>) -> Item {
        let gen = detected_gpu.map(GpuGen::from_name).unwrap_or(GpuGen::Unknown);
        let url = gen.maxine_url().to_string();
        let dir = maxine_sdk_dir();
        let vfx_dll = Path::new(&dir).join("NVVideoEffects.dll");
        let cv_dll = Path::new(&dir).join("NVCVImage.dll");
        let models_dir = Path::new(&dir).join("models");

        let has_vfx = vfx_dll.is_file();
        let has_cv = cv_dll.is_file();
        let has_models = models_dir.is_dir();
        let ok = has_vfx && has_cv && has_models;

        let gen_label = gen.label();
        let detail = if ok {
            format!("설치됨 ({dir}, {gen_label})")
        } else {
            let mut missing = Vec::new();
            if !has_vfx {
                missing.push("NVVideoEffects.dll");
            }
            if !has_cv {
                missing.push("NVCVImage.dll");
            }
            if !has_models {
                missing.push("models 폴더");
            }
            format!(
                "미설치 — AI 그린스크린 누끼에 필요 ({gen_label}용 설치 필요, 누락: {})",
                missing.join(", ")
            )
        };

        Item {
            key: "maxine",
            name: "NVIDIA Maxine VFX 런타임".to_string(),
            ok,
            required: true,
            detail,
            url,
        }
    }

    fn check_decklink() -> Item {
        let clsid_key = r"CLSID\{BA6C6F44-6DA5-4DCE-94AA-EE2D1372A676}";
        let has_clsid = reg_key_exists(HKEY_CLASSES_ROOT, clsid_key);
        let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let dll_path = format!(r"{}\System32\DeckLinkAPI64.dll", sys_root.trim_end_matches(['\\', '/']));
        let has_dll = Path::new(&dll_path).is_file();
        let ok = has_clsid || has_dll;

        let detail = if ok {
            "설치됨 (DeckLink 캡처 보드 사용 가능)".to_string()
        } else {
            "미설치 (선택 사항) — Blackmagic DeckLink 캡처 보드 입력 시에만 필요".to_string()
        };

        Item {
            key: "decklink",
            name: "Blackmagic Desktop Video".to_string(),
            ok,
            required: false,
            detail,
            url: URL_BLACKMAGIC_SUPPORT.to_string(),
        }
    }

    pub fn check_all() -> Vec<Item> {
        let gpu = gpu_name();
        let gpu_ref = gpu.as_deref();
        vec![
            check_ndi(),
            check_gpu(gpu_ref),
            check_maxine(gpu_ref),
            check_decklink(),
        ]
    }

    pub fn open_url(url: &str) {
        let op = wide_null("open");
        let target = wide_null(url);
        unsafe {
            let _ = ShellExecuteW(
                None,
                PCWSTR(op.as_ptr()),
                PCWSTR(target.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}
