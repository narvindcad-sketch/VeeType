use llama_cpp_2::{list_llama_ggml_backend_devices, LlamaBackendDeviceType};

pub fn initialize_backend_hardware() -> u32 {
    let devices = list_llama_ggml_backend_devices();
    for device in &devices {
        tracing::info!(
            name = %device.name,
            description = %device.description,
            backend = %device.backend,
            device_type = ?device.device_type,
            memory_total = device.memory_total,
            "Detected llama.cpp backend device"
        );
    }

    #[cfg(feature = "cuda")]
    {
        let cuda_device = devices.iter().find(|device| {
            device.backend.eq_ignore_ascii_case("CUDA")
                && device.device_type == LlamaBackendDeviceType::Gpu
        });
        if cuda_device.is_some() {
            tracing::info!("CUDA backend detected; enabling GPU layer offload");
            return 99;
        }
        tracing::warn!(
            "CUDA feature is enabled, but no CUDA GPU was detected; using CPU inference"
        );
    }

    #[cfg(not(feature = "cuda"))]
    {
        let gpu_available = devices.iter().any(|device| {
            matches!(
                device.device_type,
                LlamaBackendDeviceType::Gpu | LlamaBackendDeviceType::IntegratedGpu
            )
        });
        if gpu_available {
            tracing::info!(
                "GPU device detected but this build has no CUDA feature; using CPU inference"
            );
        } else {
            tracing::info!("No GPU backend available; using CPU inference");
        }
    }

    0
}
