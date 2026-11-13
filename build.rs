use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=decklink/decklink_shim.h");
    println!("cargo:rerun-if-changed=decklink/decklink_shim.cpp");
    println!("cargo:rerun-if-changed=vendor/decklink/DeckLinkAPI.idl");
    println!("cargo:rerun-if-changed=lav/lav_shim.c");
    println!("cargo:rerun-if-changed=assets/on.ico");
    println!("cargo:rerun-if-env-changed=OPEN_NDINB_FFMPEG_DIR");
    println!("cargo::rustc-check-cfg=cfg(lav_embedded)");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    #[cfg(windows)]
    build_windows();
}

#[cfg(windows)]
fn build_windows() {
    use std::path::PathBuf;
    use std::process::Command;

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let idl_dir = manifest_dir.join("vendor").join("decklink");
    let idl_path = idl_dir.join("DeckLinkAPI.idl");
    let header_out = out_dir.join("DeckLinkAPI.h");
    let iid_out = out_dir.join("DeckLinkAPI_i.c");

    let target = env::var("TARGET").unwrap_or_else(|_| "x86_64-pc-windows-msvc".to_string());
    let cl_tool = cc::windows_registry::find_tool(&target, "cl.exe")
        .or_else(|| cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "cl.exe"))
        .expect("Failed to locate MSVC cl.exe via cc::windows_registry");

    let midl_candidates = [
        PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\midl.exe"),
        PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin\x64\midl.exe"),
    ];

    let mut midl_exe = midl_candidates.iter().find(|p| p.exists()).cloned();
    if midl_exe.is_none() {
        for (_, v) in cl_tool.get_envs() {
            for dir in env::split_paths(v) {
                let cand = dir.join("midl.exe");
                if cand.exists() {
                    midl_exe = Some(cand);
                    break;
                }
            }
            if midl_exe.is_some() {
                break;
            }
        }
    }
    let midl_exe = midl_exe.unwrap_or_else(|| PathBuf::from("midl.exe"));

    let mut cmd = Command::new(&midl_exe);
    for (k, v) in cl_tool.get_envs() {
        cmd.env(k, v);
    }
    cmd.current_dir(&out_dir)
        .arg("/nologo")
        .arg("/env")
        .arg("x64")
        .arg("/I")
        .arg(&idl_dir)
        .arg("/h")
        .arg(&header_out)
        .arg("/iid")
        .arg(&iid_out)
        .arg(&idl_path);

    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("Failed to execute {:?}: {}", midl_exe, e));
    if !status.success() {
        panic!("midl failed with exit status: {}", status);
    }

    cc::Build::new()
        .cpp(true)
        .flag_if_supported("/std:c++17")
        .flag_if_supported("/EHsc")
        .flag_if_supported("/utf-8")
        .include(&out_dir)
        .include(&idl_dir)
        .include(manifest_dir.join("decklink"))
        .file(manifest_dir.join("decklink").join("decklink_shim.cpp"))
        .compile("decklink_shim");

    println!("cargo:rustc-link-lib=ole32");
    println!("cargo:rustc-link-lib=oleaut32");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(manifest_dir.join("assets").join("on.ico").to_str().unwrap());
    res.set("ProductName", "OPEN-NDINB");
    res.set("FileDescription", "OPEN-NDINB - NDI matting (alpha + original)");
    res.set("InternalName", "OPEN-NDINB");
    res.set("OriginalFilename", "OPEN-NDINB.exe");
    res.compile().expect("failed to embed icon/version resource");

    embed_lav(&manifest_dir, &out_dir);
}

#[cfg(windows)]
fn embed_lav(manifest_dir: &std::path::Path, out_dir: &std::path::Path) {
    use std::io::Write;
    let Some(ff) = env::var_os("OPEN_NDINB_FFMPEG_DIR").map(std::path::PathBuf::from) else {
        println!("cargo:warning=OPEN_NDINB_FFMPEG_DIR not set: building without the embedded ProRes/DXV decoder");
        return;
    };
    let inc = ff.join("include");
    let lib = ff.join("lib");
    let bin = ff.join("bin");
    cc::Build::new()
        .flag_if_supported("/utf-8")
        .include(&inc)
        .file(manifest_dir.join("lav").join("lav_shim.c"))
        .compile("lav_shim");
    println!("cargo:rustc-link-search=native={}", lib.display());
    for l in ["avformat", "avcodec", "avutil", "swscale", "swresample"] {
        println!("cargo:rustc-link-lib=dylib={l}");
    }
    let mut dlls: Vec<std::path::PathBuf> = std::fs::read_dir(&bin)
        .expect("ffmpeg bin dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().to_lowercase();
            n.ends_with(".dll") && ["avformat", "avcodec", "avutil", "swscale", "swresample"].iter().any(|k| n.starts_with(k))
        })
        .collect();
    dlls.sort_by_key(|p| {
        let n = p.file_name().unwrap().to_string_lossy().to_lowercase();
        ["avutil", "swresample", "swscale", "avcodec", "avformat"].iter().position(|k| n.starts_with(k)).unwrap_or(9)
    });
    assert!(dlls.len() == 5, "expected 5 ffmpeg dlls in {}", bin.display());
    let mut hasher: u64 = 0xcbf29ce484222325;
    let mut entries = String::new();
    for p in &dlls {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let raw = std::fs::read(p).expect("read dll");
        for b in raw.iter().step_by(4096) {
            hasher = (hasher ^ *b as u64).wrapping_mul(0x100000001b3);
        }
        hasher = (hasher ^ raw.len() as u64).wrapping_mul(0x100000001b3);
        let packed = miniz_oxide::deflate::compress_to_vec(&raw, 9);
        let out = out_dir.join(format!("{name}.z"));
        std::fs::write(&out, &packed).expect("write packed dll");
        entries.push_str(&format!("    ({name:?}, include_bytes!({:?}) as &[u8], {}u64, true),\n", out.display().to_string(), raw.len()));
        println!("cargo:rerun-if-changed={}", p.display());
    }
    let lic = ff.join("LICENSE.txt");
    if lic.is_file() {
        let raw = std::fs::read(&lic).unwrap();
        let packed = miniz_oxide::deflate::compress_to_vec(&raw, 9);
        let out = out_dir.join("FFMPEG-LICENSE.txt.z");
        std::fs::write(&out, &packed).unwrap();
        entries.push_str(&format!("    (\"FFMPEG-LICENSE.txt\", include_bytes!({:?}) as &[u8], {}u64, false),\n", out.display().to_string(), raw.len()));
    }
    let mut f = std::fs::File::create(out_dir.join("lav_embed.rs")).unwrap();
    writeln!(f, "pub const LAV_TAG: &str = \"lav-{hasher:016x}\";").unwrap();
    writeln!(f, "pub const LAV_NOTICE: &str = \"This program uses FFmpeg (https://ffmpeg.org) libraries licensed under the LGPL v3, unmodified, loaded as separate DLLs that can be replaced.\";").unwrap();
    writeln!(f, "pub static LAV_FILES: &[(&str, &[u8], u64, bool)] = &[\n{entries}];").unwrap();
    for d in &dlls {
        let n = d.file_name().unwrap().to_string_lossy().into_owned();
        println!("cargo:rustc-link-arg=/DELAYLOAD:{n}");
    }
    println!("cargo:rustc-link-lib=delayimp");
    println!("cargo:rustc-cfg=lav_embedded");
}
