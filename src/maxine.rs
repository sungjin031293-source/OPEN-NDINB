//! NVIDIA Maxine Video Effects "GreenScreen" (AI Green Screen), loaded at runtime from the installed
//! NVIDIA Video Effects redistributable. Per frame: UYVY upload -> GPU YUV->BGR -> AIGS -> 8-bit alpha download.
use libloading::Library;
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr::null_mut;
use std::sync::Arc;

const NVCV_A: i32 = 2;
const NVCV_BGR: i32 = 5;
const NVCV_YUV422: i32 = 11;
const NVCV_U8: i32 = 1;
const NVCV_CHUNKY: u32 = 0;
const NVCV_UYVY: u32 = 2;
const NVCV_CPU: u32 = 0;
const NVCV_GPU: u32 = 1;
const NVCV_709: u8 = 0x01;
const NVCV_VIDEO_RANGE: u8 = 0x00;
const NVCV_CHROMA_COSITED: u8 = 0x00;

#[repr(C)]
struct NvCVImage {
    width: u32,
    height: u32,
    pitch: i32,
    pixel_format: i32,
    component_type: i32,
    pixel_bytes: u8,
    component_bytes: u8,
    num_components: u8,
    planar: u8,
    gpu_mem: u8,
    colorspace: u8,
    reserved: [u8; 2],
    pixels: *mut c_void,
    delete_ptr: *mut c_void,
    delete_proc: Option<unsafe extern "C" fn(*mut c_void)>,
    buffer_bytes: u64,
}

impl NvCVImage {
    fn empty() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

type Status = i32;
type Handle = *mut c_void;
type Stream = *mut c_void;
type StateHandle = *mut c_void;

struct Api {
    _vfx: Library,
    _cv: Library,
    create_effect: unsafe extern "C" fn(*const c_char, *mut Handle) -> Status,
    destroy_effect: unsafe extern "C" fn(Handle),
    set_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> Status,
    set_u32: unsafe extern "C" fn(Handle, *const c_char, u32) -> Status,
    set_cuda_stream: unsafe extern "C" fn(Handle, *const c_char, Stream) -> Status,
    set_image: unsafe extern "C" fn(Handle, *const c_char, *mut NvCVImage) -> Status,
    set_state_array: unsafe extern "C" fn(Handle, *const c_char, *mut StateHandle) -> Status,
    load: unsafe extern "C" fn(Handle) -> Status,
    run: unsafe extern "C" fn(Handle, i32) -> Status,
    stream_create: unsafe extern "C" fn(*mut Stream) -> Status,
    stream_destroy: unsafe extern "C" fn(Stream) -> Status,
    allocate_state: unsafe extern "C" fn(Handle, *mut StateHandle) -> Status,
    deallocate_state: unsafe extern "C" fn(Handle, StateHandle) -> Status,
    img_init: unsafe extern "C" fn(*mut NvCVImage, u32, u32, i32, *mut c_void, i32, i32, u32, u32) -> Status,
    img_alloc: unsafe extern "C" fn(*mut NvCVImage, u32, u32, i32, i32, u32, u32, u32) -> Status,
    img_dealloc: unsafe extern "C" fn(*mut NvCVImage),
    img_transfer: unsafe extern "C" fn(*const NvCVImage, *mut NvCVImage, f32, Stream, *mut NvCVImage) -> Status,
    err_string: unsafe extern "C" fn(Status) -> *const c_char,
}

fn sdk_dir() -> String {
    if let Ok(p) = std::env::var("NV_VIDEO_EFFECTS_PATH") {
        if !p.is_empty() && p != "USE_APP_PATH" {
            return p;
        }
    }
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    format!(r"{pf}\NVIDIA Corporation\NVIDIA Video Effects")
}

#[cfg(windows)]
fn set_dll_directory(dir: &str) {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetDllDirectoryW(path: *const u16) -> i32;
    }
    let wide: Vec<u16> = dir.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { SetDllDirectoryW(wide.as_ptr()) };
}
#[cfg(not(windows))]
fn set_dll_directory(_dir: &str) {}

fn load_api() -> Result<(Api, String), String> {
    let dir = sdk_dir();
    set_dll_directory(&dir);
    let (vfx_name, cv_name) = if cfg!(windows) { ("NVVideoEffects.dll", "NVCVImage.dll") } else { ("libVideoFX.so", "libNVCVImage.so") };
    let open = |name: &str| -> Result<Library, String> {
        let full = format!("{dir}{}{name}", std::path::MAIN_SEPARATOR);
        unsafe { Library::new(&full) }.or_else(|_| unsafe { Library::new(name) }).map_err(|e| {
            format!("NVIDIA Video Effects 런타임이 없습니다 ({name}: {e}). RTX GPU에서 NVIDIA Maxine VFX 재배포 패키지를 설치하세요: https://www.nvidia.com/broadcast-sdk-resources")
        })
    };
    let cv = open(cv_name)?;
    let vfx = open(vfx_name)?;
    unsafe {
        macro_rules! sym {
            ($lib:expr, $n:literal) => {
                *$lib.get(concat!($n, "\0").as_bytes()).map_err(|e| format!("Maxine {}: {e}", $n))?
            };
        }
        let api = Api {
            create_effect: sym!(vfx, "NvVFX_CreateEffect"),
            destroy_effect: sym!(vfx, "NvVFX_DestroyEffect"),
            set_string: sym!(vfx, "NvVFX_SetString"),
            set_u32: sym!(vfx, "NvVFX_SetU32"),
            set_cuda_stream: sym!(vfx, "NvVFX_SetCudaStream"),
            set_image: sym!(vfx, "NvVFX_SetImage"),
            set_state_array: sym!(vfx, "NvVFX_SetStateObjectHandleArray"),
            load: sym!(vfx, "NvVFX_Load"),
            run: sym!(vfx, "NvVFX_Run"),
            stream_create: sym!(vfx, "NvVFX_CudaStreamCreate"),
            stream_destroy: sym!(vfx, "NvVFX_CudaStreamDestroy"),
            allocate_state: sym!(vfx, "NvVFX_AllocateState"),
            deallocate_state: sym!(vfx, "NvVFX_DeallocateState"),
            img_init: sym!(cv, "NvCVImage_Init"),
            img_alloc: sym!(cv, "NvCVImage_Alloc"),
            img_dealloc: sym!(cv, "NvCVImage_Dealloc"),
            img_transfer: sym!(cv, "NvCVImage_Transfer"),
            err_string: sym!(cv, "NvCV_GetErrorStringFromCode"),
            _vfx: vfx,
            _cv: cv,
        };
        Ok((api, dir))
    }
}

fn check(api: &Api, what: &str, s: Status) -> Result<(), String> {
    if s == 0 {
        return Ok(());
    }
    let msg = unsafe { CStr::from_ptr((api.err_string)(s)) }.to_string_lossy().into_owned();
    Err(format!("Maxine {what} 실패: {msg} ({s})"))
}

pub struct Matter {
    api: Arc<Api>,
    eff: Handle,
    stream: Stream,
    state: StateHandle,
    src_gpu: NvCVImage,
    dst_gpu: NvCVImage,
    tmp: NvCVImage,
    width: u32,
    height: u32,
    pub mode: u32,
}

unsafe impl Send for Matter {}

impl Matter {
    pub fn new(width: u32, height: u32, mode: u32) -> Result<Self, String> {
        let (api, dir) = load_api()?;
        let api = Arc::new(api);
        let mut m = Matter {
            api,
            eff: null_mut(),
            stream: null_mut(),
            state: null_mut(),
            src_gpu: NvCVImage::empty(),
            dst_gpu: NvCVImage::empty(),
            tmp: NvCVImage::empty(),
            width,
            height,
            mode,
        };
        m.init(&dir)?;
        Ok(m)
    }

    fn init(&mut self, dir: &str) -> Result<(), String> {
        let a = self.api.clone();
        let sel = |s: &str| CString::new(s).unwrap();
        unsafe {
            check(&a, "CreateEffect(GreenScreen)", (a.create_effect)(sel("GreenScreen").as_ptr(), &mut self.eff))?;
            let models = format!("{dir}{}models", std::path::MAIN_SEPARATOR);
            if std::path::Path::new(&models).is_dir() {
                check(&a, "ModelDir", (a.set_string)(self.eff, sel("ModelDir").as_ptr(), sel(&models).as_ptr()))?;
            }
            check(&a, "Mode", (a.set_u32)(self.eff, sel("Mode").as_ptr(), self.mode))?;
            check(&a, "CudaStreamCreate", (a.stream_create)(&mut self.stream))?;
            check(&a, "CudaStream", (a.set_cuda_stream)(self.eff, sel("CudaStream").as_ptr(), self.stream))?;
            check(&a, "MaxInputWidth", (a.set_u32)(self.eff, sel("MaxInputWidth").as_ptr(), self.width))?;
            check(&a, "MaxInputHeight", (a.set_u32)(self.eff, sel("MaxInputHeight").as_ptr(), self.height))?;
            check(&a, "MaxNumberStreams", (a.set_u32)(self.eff, sel("MaxNumberStreams").as_ptr(), 1))?;
            check(&a, "Load", (a.load)(self.eff))?;
            check(&a, "AllocateState", (a.allocate_state)(self.eff, &mut self.state))?;
            check(&a, "Alloc src", (a.img_alloc)(&mut self.src_gpu, self.width, self.height, NVCV_BGR, NVCV_U8, NVCV_CHUNKY, NVCV_GPU, 1))?;
            check(&a, "Alloc dst", (a.img_alloc)(&mut self.dst_gpu, self.width, self.height, NVCV_A, NVCV_U8, NVCV_CHUNKY, NVCV_GPU, 1))?;
            check(&a, "SetImage src", (a.set_image)(self.eff, sel("SrcImage0").as_ptr(), &mut self.src_gpu))?;
            check(&a, "SetImage dst", (a.set_image)(self.eff, sel("DstImage0").as_ptr(), &mut self.dst_gpu))?;
        }
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn matte(&mut self, uyvy: &[u8], stride: usize, alpha: &mut Vec<u8>) -> Result<(), String> {
        let a = self.api.clone();
        let (w, h) = (self.width, self.height);
        alpha.resize((w * h) as usize, 0);
        let mut src_cpu = NvCVImage::empty();
        let mut dst_cpu = NvCVImage::empty();
        let state_sel = CString::new("State").unwrap();
        unsafe {
            check(&a, 
                "Init src",
                (a.img_init)(&mut src_cpu, w, h, stride as i32, uyvy.as_ptr() as *mut c_void, NVCV_YUV422, NVCV_U8, NVCV_UYVY, NVCV_CPU),
            )?;
            src_cpu.colorspace = NVCV_709 | NVCV_VIDEO_RANGE | NVCV_CHROMA_COSITED;
            check(&a, "Init dst", (a.img_init)(&mut dst_cpu, w, h, w as i32, alpha.as_mut_ptr() as *mut c_void, NVCV_A, NVCV_U8, NVCV_CHUNKY, NVCV_CPU))?;
            check(&a, "Upload", (a.img_transfer)(&src_cpu, &mut self.src_gpu, 1.0, self.stream, &mut self.tmp))?;
            let mut states = [self.state];
            check(&a, "State", (a.set_state_array)(self.eff, state_sel.as_ptr(), states.as_mut_ptr()))?;
            check(&a, "Run", (a.run)(self.eff, 0))?;
            check(&a, "Download", (a.img_transfer)(&self.dst_gpu, &mut dst_cpu, 1.0, self.stream, &mut self.tmp))?;
        }
        Ok(())
    }
}

impl Drop for Matter {
    fn drop(&mut self) {
        let a = self.api.clone();
        unsafe {
            if !self.state.is_null() {
                (a.deallocate_state)(self.eff, self.state);
            }
            if !self.eff.is_null() {
                (a.destroy_effect)(self.eff);
            }
            (a.img_dealloc)(&mut self.src_gpu);
            (a.img_dealloc)(&mut self.dst_gpu);
            (a.img_dealloc)(&mut self.tmp);
            if !self.stream.is_null() {
                (a.stream_destroy)(self.stream);
            }
        }
    }
}
