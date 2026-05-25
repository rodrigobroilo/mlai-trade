// Runtime accelerator capability reporting.
//
// Function map:
// - accelerator_status_json(): reports MLX/tch availability for status output.
// - accelerator_status_lines(): formats accelerator state for human status.

use serde_json::{json, Value};
use std::process::Command;

// Returns NVIDIA GPU visibility from the host driver tooling.
fn nvidia_status_json() -> Value {
    #[cfg(target_os = "linux")]
    {
        match Command::new("nvidia-smi")
            .args([
                "--query-gpu=name,driver_version,memory.total",
                "--format=csv,noheader,nounits",
            ])
            .output()
        {
            Ok(output) if output.status.success() => {
                let gpus = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| {
                        let parts = line.split(',').map(str::trim).collect::<Vec<_>>();
                        if parts.len() >= 3 {
                            Some(json!({
                                "name": parts[0],
                                "driver_version": parts[1],
                                "memory_total_mib": parts[2].parse::<u64>().ok(),
                            }))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                json!({
                    "available": !gpus.is_empty(),
                    "compatible": true,
                    "gpus": gpus,
                    "message": if gpus.is_empty() {
                        "not available; nvidia-smi returned no GPUs"
                    } else {
                        "available; NVIDIA GPU is visible to the process"
                    },
                })
            }
            Ok(output) => json!({
                "available": false,
                "compatible": true,
                "gpus": [],
                "message": format!(
                    "not available; nvidia-smi failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            }),
            Err(err) => json!({
                "available": false,
                "compatible": true,
                "gpus": [],
                "message": format!("not available; nvidia-smi could not run: {err}"),
            }),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        json!({
            "available": false,
            "compatible": false,
            "gpus": [],
            "message": "not compatible; NVIDIA CUDA detection is only enabled on Linux",
        })
    }
}

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
        let cuda_available = crate::lstm::tch_cuda_available();
        return json!({
            "backend": "tch",
            "available": cuda_available,
            "compatible": true,
            "compiled": true,
            "cuda_available": cuda_available,
            "implemented": true,
            "cpu_cap_applies": !cuda_available,
            "message": if cuda_available {
                "available; tch/CUDA LSTM can use the NVIDIA CUDA backend"
            } else {
                "not available; linked libtorch does not expose CUDA or CUDA is not visible to this process"
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
    let nvidia = nvidia_status_json();
    let nvidia_available = nvidia
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    json!({
        "nvidia": nvidia,
        "xgboost_cuda": {
            "compiled": cfg!(mlai_nvidia_cuda),
            "available": cfg!(mlai_nvidia_cuda) && nvidia_available,
            "message": if cfg!(mlai_nvidia_cuda) && nvidia_available {
                "compiled; XGBoost auto/cuda can use the NVIDIA CUDA backend"
            } else if cfg!(mlai_nvidia_cuda) {
                "compiled, but NVIDIA CUDA is not visible to this process; XGBoost auto falls back to CPU"
            } else {
                "not compiled; package on a compatible CUDA Linux host to enable XGBoost CUDA"
            },
        },
        "lightgbm_cuda": {
            "compiled": cfg!(mlai_lightgbm_cuda),
            "available": cfg!(mlai_lightgbm_cuda) && nvidia_available,
            "message": if cfg!(mlai_lightgbm_cuda) && nvidia_available {
                "compiled; LightGBM auto/cuda can use the NVIDIA CUDA backend"
            } else if cfg!(mlai_lightgbm_cuda) {
                "compiled, but NVIDIA CUDA is not visible to this process; LightGBM auto falls back to CPU"
            } else {
                "not compiled; package on a compatible CUDA Linux host to enable LightGBM CUDA"
            },
        },
        "mlx": mlx_status_json(),
        "tch": tch_status_json(),
        "cpu_cap_applies": true,
        "gpu_npu_paths_uncapped_when_available": true,
    })
}

// Builds one human-readable accelerator status line per backend.
pub fn accelerator_status_lines() -> Vec<String> {
    let status = accelerator_status_json();
    ["nvidia", "xgboost_cuda", "lightgbm_cuda", "mlx", "tch"]
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
