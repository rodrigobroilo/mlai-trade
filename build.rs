use std::{env, fs, path::PathBuf};

fn cuda_library_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for key in ["CUDA_HOME", "CUDA_PATH", "CUDAToolkit_ROOT"] {
        if let Some(value) = env::var_os(key) {
            if !value.is_empty() {
                roots.push(PathBuf::from(value));
            }
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn emit_cuda_runtime_links() {
    for root in cuda_library_roots() {
        for lib_dir in [
            root.join("lib64"),
            root.join("lib"),
            root.join("targets/x86_64-linux/lib"),
        ] {
            if lib_dir.is_dir() {
                println!("cargo:rustc-link-search=native={}", lib_dir.display());
            }
        }
    }
    println!("cargo:rustc-link-lib=dylib=cudart");
}

fn emit_libtorch_cuda_links() {
    let mut lib_dirs = Vec::new();
    if let Some(lib_dir) = env::var_os("DEP_TCH_LIBTORCH_LIB").map(PathBuf::from) {
        lib_dirs.push(lib_dir);
    }
    if let Some(libtorch) = env::var_os("LIBTORCH").map(PathBuf::from) {
        lib_dirs.push(libtorch.join("lib"));
    }
    if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
        let profile = env::var("PROFILE").unwrap_or_else(|_| "release".to_string());
        let build_root = PathBuf::from(manifest_dir)
            .join("target")
            .join(profile)
            .join("build");
        if let Ok(entries) = fs::read_dir(build_root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("torch-sys-") {
                    lib_dirs.push(entry.path().join("out/libtorch/libtorch/lib"));
                }
            }
        }
    }

    lib_dirs.sort();
    lib_dirs.dedup();
    for lib_dir in lib_dirs {
        if !lib_dir.join("libtorch_cuda.so").is_file() {
            continue;
        }
        println!("cargo:rustc-link-search=native={}", lib_dir.display());
        println!("cargo:rustc-link-arg=-Wl,--no-as-needed");
        println!("cargo:rustc-link-lib=dylib=torch_cuda");
        println!("cargo:rustc-link-lib=dylib=c10_cuda");
        break;
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(mlai_xgboost)");
    println!("cargo:rustc-check-cfg=cfg(mlai_mlx)");
    println!("cargo:rustc-check-cfg=cfg(mlai_tch)");
    println!("cargo:rustc-check-cfg=cfg(mlai_nvidia_cuda)");
    println!("cargo:rustc-check-cfg=cfg(mlai_lightgbm_cuda)");

    let target = env::var("TARGET").unwrap_or_default();
    let is_macos = target.contains("apple-darwin");
    let is_linux = target.contains("linux");
    let is_freebsd = target.contains("freebsd");
    let is_aarch64 = target.starts_with("aarch64-");

    if is_macos || is_linux {
        println!("cargo:rustc-cfg=mlai_xgboost");
    }
    if is_macos && is_aarch64 {
        println!("cargo:rustc-cfg=mlai_mlx");
    }
    if is_linux {
        println!("cargo:rustc-cfg=mlai_tch");
        if env::var_os("CARGO_FEATURE_NVIDIA_CUDA").is_some() {
            println!("cargo:rustc-cfg=mlai_nvidia_cuda");
        }
        if env::var_os("CARGO_FEATURE_LIGHTGBM_CUDA").is_some() {
            println!("cargo:rustc-cfg=mlai_lightgbm_cuda");
            emit_cuda_runtime_links();
        }
        emit_libtorch_cuda_links();
        println!("cargo:rustc-link-arg=-Wl,--enable-new-dtags");
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/lib");
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/deps");
    }
    if is_freebsd {
        println!("cargo:rustc-link-lib=c++");
    }
}
