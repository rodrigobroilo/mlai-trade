// Runtime accelerator capability reporting.
//
// Function map:
// - accelerator_status_json(): reports MLX/tch availability for status output.
// - accelerator_status_lines(): formats accelerator state for human status.

use serde_json::{json, Value};

// Returns MLX availability for Apple Silicon unified-memory acceleration.
fn mlx_status_json() -> Value {
    #[cfg(mlai_mlx)]
    {
        return json!({
            "backend": "mlx",
            "available": true,
            "compatible": true,
            "compiled": true,
            "cpu_cap_applies": false,
            "message": "available; Apple Silicon MLX GPU/NPU/unified-memory path is uncapped",
        });
    }
    #[cfg(all(not(mlai_mlx), target_os = "macos", target_arch = "aarch64"))]
    {
        return json!({
            "backend": "mlx",
            "available": false,
            "compatible": true,
            "compiled": false,
            "cpu_cap_applies": true,
            "message": "not available; Apple Silicon MLX is mandatory on this target, but the build did not enable the platform cfg",
        });
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        json!({
            "backend": "mlx",
            "available": false,
            "compatible": false,
            "compiled": cfg!(mlai_mlx),
            "cpu_cap_applies": true,
            "message": "not compatible; MLX requires Apple Silicon macOS",
        })
    }
}

// Returns tch/CUDA availability for Linux NVIDIA acceleration.
fn tch_status_json() -> Value {
    #[cfg(mlai_tch)]
    {
        let cuda_available = tch::Cuda::is_available();
        return json!({
            "backend": "tch",
            "available": cuda_available,
            "compatible": true,
            "compiled": true,
            "cuda_available": cuda_available,
            "cpu_cap_applies": !cuda_available,
            "message": if cuda_available {
                "available; Linux tch/CUDA path is uncapped"
            } else {
                "not available; Linux tch/libtorch is linked, but CUDA is not visible at runtime"
            },
        });
    }
    #[cfg(all(not(mlai_tch), target_os = "linux"))]
    {
        return json!({
            "backend": "tch",
            "available": false,
            "compatible": true,
            "compiled": false,
            "cuda_available": false,
            "cpu_cap_applies": true,
            "message": "not available; Linux tch is mandatory on this target, but the build did not enable the platform cfg",
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        json!({
            "backend": "tch",
            "available": false,
            "compatible": false,
            "compiled": cfg!(mlai_tch),
            "cuda_available": false,
            "cpu_cap_applies": true,
            "message": "not compatible; tch/CUDA requires Linux with NVIDIA CUDA",
        })
    }
}

// Returns accelerator availability as JSON for API and daemon status.
pub fn accelerator_status_json() -> Value {
    json!({
        "mlx": mlx_status_json(),
        "tch": tch_status_json(),
        "cpu_cap_applies": true,
        "gpu_npu_paths_uncapped_when_available": true,
    })
}

// Builds one human-readable accelerator status line per backend.
pub fn accelerator_status_lines() -> Vec<String> {
    let status = accelerator_status_json();
    ["mlx", "tch"]
        .into_iter()
        .map(|name| {
            let item = &status[name];
            format!(
                "{}: {}",
                name.to_ascii_uppercase(),
                item.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("not available")
            )
        })
        .collect()
}
