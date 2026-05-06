use std::env;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(mlai_xgboost)");
    println!("cargo:rustc-check-cfg=cfg(mlai_mlx)");
    println!("cargo:rustc-check-cfg=cfg(mlai_tch)");

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
    }
    if is_freebsd {
        println!("cargo:rustc-link-lib=c++");
    }
}
