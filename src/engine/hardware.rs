use llama_cpp_2::{list_llama_ggml_backend_devices, LlamaBackendDeviceType};

fn vulkan_gpu_device(
    devices: &[llama_cpp_2::LlamaBackendDevice],
) -> Option<&llama_cpp_2::LlamaBackendDevice> {
    devices.iter().find(|device| {
        device.backend.eq_ignore_ascii_case("Vulkan")
            && matches!(
                device.device_type,
                LlamaBackendDeviceType::Gpu | LlamaBackendDeviceType::IntegratedGpu
            )
    })
}

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
    let vulkan_device = vulkan_gpu_device(&devices);

    #[cfg(feature = "vulkan")]
    {
        if let Some(device) = vulkan_device {
            tracing::info!(
                device = %device.description,
                memory_free = device.memory_free,
                "Vulkan GPU detected; enabling GPU layer offload"
            );
            return 99;
        }
        tracing::warn!(
            "Vulkan is enabled, but no compatible Vulkan GPU was detected; using CPU inference"
        );
    }

    #[cfg(not(feature = "vulkan"))]
    {
        if let Some(device) = vulkan_device {
            tracing::info!(
                device = %device.description,
                "Vulkan GPU detected but this build uses CPU inference"
            );
            return 0;
        }
        let gpu_available = devices.iter().any(|device| {
            matches!(
                device.device_type,
                LlamaBackendDeviceType::Gpu | LlamaBackendDeviceType::IntegratedGpu
            )
        });
        if gpu_available {
            tracing::info!(
                "GPU device detected but this build has no Vulkan feature; using CPU inference"
            );
        } else {
            tracing::info!("No GPU backend available; using CPU inference");
        }
    }

    0
}

#[cfg(test)]
mod tests {
    use super::vulkan_gpu_device;
    use llama_cpp_2::{LlamaBackendDevice, LlamaBackendDeviceType};

    fn device(backend: &str, device_type: LlamaBackendDeviceType) -> LlamaBackendDevice {
        LlamaBackendDevice {
            index: 0,
            name: format!("{backend}0"),
            description: "test graphics device".into(),
            backend: backend.into(),
            memory_total: 0,
            memory_free: 0,
            device_type,
        }
    }

    #[test]
    fn recognizes_integrated_and_discrete_vulkan_devices_across_vendors() {
        for description in [
            "NVIDIA GeForce RTX",
            "AMD Radeon",
            "Intel Arc",
            "Intel Integrated Graphics",
        ] {
            for device_type in [
                LlamaBackendDeviceType::Gpu,
                LlamaBackendDeviceType::IntegratedGpu,
            ] {
                let mut vulkan_device = device("Vulkan", device_type);
                vulkan_device.description = description.into();
                assert!(
                    vulkan_gpu_device(&[vulkan_device]).is_some(),
                    "{description} ({device_type:?})"
                );
            }
        }
    }

    #[test]
    fn ignores_cpu_devices_and_gpus_from_other_backends() {
        let devices = [
            device("CPU", LlamaBackendDeviceType::Cpu),
            device("CUDA", LlamaBackendDeviceType::Gpu),
            device("OpenCL", LlamaBackendDeviceType::Gpu),
        ];
        assert!(vulkan_gpu_device(&devices).is_none());
    }
}
