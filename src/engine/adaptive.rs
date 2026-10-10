//! Load-aware inference routing. Decisions are applied between requests only.

use std::collections::HashMap;
use std::mem::size_of;
use std::ptr::null;
#[cfg(feature = "vulkan")]
use std::sync::Once;
use std::time::{Duration, Instant};

use sysinfo::{System, MINIMUM_CPU_UPDATE_INTERVAL};
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W,
    PDH_FMT_DOUBLE, PDH_MORE_DATA,
};

// Windows SDK Pdh.h; absent from windows-sys 0.52's generated constants.
const PDH_FMT_NOCAP100: u32 = 0x00008000;
const MEMORY_RESERVE: u64 = 512 * 1024 * 1024;
const SWITCH_DWELL: Duration = Duration::from_secs(30);
const GPU_RETRY_DELAY: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputeBackend {
    Cpu,
    Gpu,
}

#[derive(Clone, Copy, Debug, Default)]
struct LoadSnapshot {
    cpu_percent: f32,
    gpu_percent: Option<f32>,
    gpu_free_bytes: Option<u64>,
}

struct GpuUsageCounter {
    query: isize,
    counter: isize,
}

impl GpuUsageCounter {
    fn new() -> Option<Self> {
        let mut query = 0;
        let mut counter = 0;
        let path: Vec<u16> = "\\GPU Engine(*)\\Utilization Percentage"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        unsafe {
            if PdhOpenQueryW(null(), 0, &mut query) != 0 {
                return None;
            }
            if PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) != 0 {
                PdhCloseQuery(query);
                return None;
            }
            // Utilization is a rate: collect a baseline before the first request.
            PdhCollectQueryData(query);
        }
        Some(Self { query, counter })
    }

    fn sample(&self) -> Option<f32> {
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let mut bytes = 0;
            let mut count = 0;
            let format = PDH_FMT_DOUBLE | PDH_FMT_NOCAP100;
            let status = PdhGetFormattedCounterArrayW(
                self.counter,
                format,
                &mut bytes,
                &mut count,
                std::ptr::null_mut(),
            );
            if status != PDH_MORE_DATA || bytes == 0 {
                return None;
            }
            // u64 storage provides the alignment required by PDH's structs.
            let mut buffer = vec![0_u64; (bytes as usize).div_ceil(size_of::<u64>())];
            let start = buffer.as_mut_ptr() as usize;
            let end = start + buffer.len() * size_of::<u64>();
            let items = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            if PdhGetFormattedCounterArrayW(self.counter, format, &mut bytes, &mut count, items)
                != 0
                || count as usize
                    > buffer.len() * size_of::<u64>() / size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>()
            {
                return None;
            }
            let mut engines = HashMap::<String, f32>::new();
            for item in std::slice::from_raw_parts(items, count as usize) {
                if !matches!(
                    item.FmtValue.CStatus,
                    PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA
                ) {
                    continue;
                }
                let address = item.szName as usize;
                if address < start || address >= end || address % 2 != 0 {
                    continue;
                }
                let name_buffer = std::slice::from_raw_parts(item.szName, (end - address) / 2);
                let length = name_buffer.iter().position(|value| *value == 0)?;
                let name = String::from_utf16_lossy(&name_buffer[..length]);
                // Sum processes sharing the same physical GPU engine, rather
                // than incorrectly summing unrelated 3D/copy/compute engines.
                let Some((_, engine)) = name.split_once("_luid_") else {
                    continue;
                };
                let value = item.FmtValue.Anonymous.doubleValue as f32;
                if value.is_finite() {
                    *engines.entry(engine.to_string()).or_default() += value.max(0.0);
                }
            }
            engines
                .values()
                .copied()
                .reduce(f32::max)
                .map(|load| load.clamp(0.0, 100.0))
        }
    }
}

impl Drop for GpuUsageCounter {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

/// A single warm model is retained; changing backends never doubles its memory.
pub struct AdaptiveBackend {
    sys: System,
    gpu_counter: Option<GpuUsageCounter>,
    current: Option<ComputeBackend>,
    last_switch: Instant,
    pending: Option<ComputeBackend>,
    pending_count: u8,
    gpu_retry_after: Option<Instant>,
    cpu_speed: Option<f64>,
    gpu_speed: Option<f64>,
    snapshot: LoadSnapshot,
}

impl AdaptiveBackend {
    pub fn new() -> Self {
        #[cfg(feature = "vulkan")]
        {
            static REGISTER_BACKENDS: Once = Once::new();
            REGISTER_BACKENDS.call_once(|| unsafe {
                llama_cpp_sys_2::ggml_backend_load_all();
            });
        }
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        Self {
            sys,
            gpu_counter: cfg!(feature = "vulkan")
                .then(GpuUsageCounter::new)
                .flatten(),
            current: None,
            last_switch: Instant::now(),
            pending: None,
            pending_count: 0,
            gpu_retry_after: None,
            cpu_speed: None,
            gpu_speed: None,
            snapshot: LoadSnapshot::default(),
        }
    }

    pub fn select(&mut self, model_bytes: u64, resident_gpu: bool) -> ComputeBackend {
        // Measure recent load, not an average over a potentially long idle gap.
        self.sys.refresh_cpu_usage();
        if let Some(counter) = self.gpu_counter.as_ref() {
            let _ = counter.sample();
        }
        std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
        self.sys.refresh_cpu_usage();
        let gpu_free_bytes = if cfg!(feature = "vulkan") {
            crate::engine::hardware::vulkan_memory_free()
        } else {
            None
        };
        let gpu_percent = self.gpu_counter.as_ref().and_then(GpuUsageCounter::sample);
        self.snapshot = LoadSnapshot {
            cpu_percent: self.sys.global_cpu_usage(),
            gpu_percent,
            gpu_free_bytes,
        };
        let now = Instant::now();
        let candidate = desired_backend(
            self.snapshot,
            self.current,
            if resident_gpu { 0 } else { model_bytes },
            self.cpu_speed,
            self.gpu_speed,
            self.gpu_retry_after.is_some_and(|deadline| now < deadline),
        );
        let selected = stabilize_selection(
            self.current,
            candidate,
            now.duration_since(self.last_switch),
            self.snapshot.gpu_free_bytes.is_none_or(|free| {
                free < required_gpu_memory(if resident_gpu { 0 } else { model_bytes })
            }),
            &mut self.pending,
            &mut self.pending_count,
        );
        tracing::info!(
            backend = ?selected, cpu_percent = self.snapshot.cpu_percent,
            gpu_percent = ?gpu_percent, gpu_free_bytes = ?gpu_free_bytes,
            "Adaptive inference decision"
        );
        selected
    }

    pub fn activate(&mut self, backend: ComputeBackend) {
        if self.current != Some(backend) {
            self.current = Some(backend);
            self.last_switch = Instant::now();
            self.pending = None;
            self.pending_count = 0;
        }
    }

    pub fn gpu_failed(&mut self) {
        self.gpu_retry_after = Some(Instant::now() + GPU_RETRY_DELAY);
        self.activate(ComputeBackend::Cpu);
    }

    pub fn record_performance(
        &mut self,
        backend: ComputeBackend,
        work_units: f64,
        elapsed: Duration,
    ) {
        if work_units <= 0.0 {
            return;
        }
        let speed = elapsed.as_secs_f64() / work_units;
        let slot = match backend {
            ComputeBackend::Cpu => &mut self.cpu_speed,
            ComputeBackend::Gpu => &mut self.gpu_speed,
        };
        *slot = Some(slot.map_or(speed, |previous| previous * 0.75 + speed * 0.25));
    }

    pub fn thread_count(&self) -> usize {
        let cap = if self.snapshot.cpu_percent >= 85.0 {
            1
        } else if self.snapshot.cpu_percent >= 65.0 {
            2
        } else {
            4
        };
        crate::engine::worker_thread_count().min(cap)
    }
}

fn required_gpu_memory(model_bytes: u64) -> u64 {
    (model_bytes.saturating_mul(3) / 2).saturating_add(MEMORY_RESERVE)
}

fn desired_backend(
    load: LoadSnapshot,
    current: Option<ComputeBackend>,
    model_bytes: u64,
    cpu_speed: Option<f64>,
    gpu_speed: Option<f64>,
    gpu_cooling_down: bool,
) -> ComputeBackend {
    let Some(free) = load.gpu_free_bytes else {
        return ComputeBackend::Cpu;
    };
    let required = required_gpu_memory(model_bytes);
    if gpu_cooling_down || free < required {
        return ComputeBackend::Cpu;
    }
    let gpu_load = load.gpu_percent.unwrap_or(0.0);
    if gpu_load >= 85.0 {
        return if load.cpu_percent < 70.0 {
            ComputeBackend::Cpu
        } else {
            current.unwrap_or(ComputeBackend::Gpu)
        };
    }
    if gpu_load <= 55.0 {
        return ComputeBackend::Gpu;
    }
    if let (Some(cpu), Some(gpu)) = (cpu_speed, gpu_speed) {
        if load.cpu_percent < 50.0 && cpu < gpu * 0.8 {
            return ComputeBackend::Cpu;
        }
    }
    current.unwrap_or(ComputeBackend::Gpu)
}

fn stabilize_selection(
    current: Option<ComputeBackend>,
    candidate: ComputeBackend,
    elapsed: Duration,
    memory_critical: bool,
    pending: &mut Option<ComputeBackend>,
    count: &mut u8,
) -> ComputeBackend {
    let Some(current) = current else {
        return candidate;
    };
    if candidate == current {
        *pending = None;
        *count = 0;
        return current;
    }
    if memory_critical && candidate == ComputeBackend::Cpu {
        return candidate;
    }
    if *pending == Some(candidate) {
        *count = count.saturating_add(1);
    } else {
        *pending = Some(candidate);
        *count = 1;
    }
    if elapsed >= SWITCH_DWELL && *count >= 2 {
        candidate
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(cpu: f32, gpu: f32, free_gb: u64) -> LoadSnapshot {
        LoadSnapshot {
            cpu_percent: cpu,
            gpu_percent: Some(gpu),
            gpu_free_bytes: Some(free_gb * 1024 * 1024 * 1024),
        }
    }

    #[test]
    fn routes_to_cpu_when_gpu_is_busy_but_cpu_has_headroom() {
        assert_eq!(
            desired_backend(
                load(20.0, 95.0, 8),
                Some(ComputeBackend::Gpu),
                1_000_000_000,
                None,
                None,
                false
            ),
            ComputeBackend::Cpu
        );
    }
    #[test]
    fn uses_gpu_when_cpu_is_busy_and_gpu_has_headroom() {
        assert_eq!(
            desired_backend(
                load(95.0, 20.0, 8),
                Some(ComputeBackend::Cpu),
                1_000_000_000,
                None,
                None,
                false
            ),
            ComputeBackend::Gpu
        );
    }
    #[test]
    fn critical_gpu_memory_and_missing_gpu_use_cpu() {
        assert_eq!(
            desired_backend(
                load(10.0, 0.0, 0),
                Some(ComputeBackend::Gpu),
                1_000_000_000,
                None,
                None,
                false
            ),
            ComputeBackend::Cpu
        );
        assert_eq!(
            desired_backend(
                LoadSnapshot::default(),
                None,
                1_000_000_000,
                None,
                None,
                false
            ),
            ComputeBackend::Cpu
        );
    }
    #[test]
    fn keeps_current_backend_when_both_processors_are_busy() {
        assert_eq!(
            desired_backend(
                load(95.0, 95.0, 8),
                Some(ComputeBackend::Cpu),
                1_000_000_000,
                None,
                None,
                false
            ),
            ComputeBackend::Cpu
        );
    }
    #[test]
    fn measured_latency_can_prefer_cpu_under_moderate_gpu_load() {
        assert_eq!(
            desired_backend(
                load(20.0, 65.0, 8),
                Some(ComputeBackend::Gpu),
                1_000_000_000,
                Some(0.1),
                Some(0.4),
                false
            ),
            ComputeBackend::Cpu
        );
    }
    #[test]
    fn failed_gpu_is_not_retried_during_cooldown() {
        assert_eq!(
            desired_backend(load(10.0, 0.0, 8), None, 1_000_000_000, None, None, true),
            ComputeBackend::Cpu
        );
    }
    #[test]
    fn resident_gpu_model_does_not_need_its_allocation_budget_again() {
        let snapshot = load(10.0, 20.0, 1);
        assert_eq!(
            desired_backend(
                snapshot,
                Some(ComputeBackend::Gpu),
                1_600_000_000,
                None,
                None,
                false
            ),
            ComputeBackend::Cpu
        );
        assert_eq!(
            desired_backend(snapshot, Some(ComputeBackend::Gpu), 0, None, None, false),
            ComputeBackend::Gpu
        );
    }
    #[test]
    fn switches_require_two_observations_and_thirty_seconds() {
        let mut pending = None;
        let mut count = 0;
        let current = Some(ComputeBackend::Gpu);
        assert_eq!(
            stabilize_selection(
                current,
                ComputeBackend::Cpu,
                Duration::from_secs(10),
                false,
                &mut pending,
                &mut count
            ),
            ComputeBackend::Gpu
        );
        assert_eq!(
            stabilize_selection(
                current,
                ComputeBackend::Cpu,
                Duration::from_secs(20),
                false,
                &mut pending,
                &mut count
            ),
            ComputeBackend::Gpu
        );
        assert_eq!(
            stabilize_selection(
                current,
                ComputeBackend::Cpu,
                Duration::from_secs(31),
                false,
                &mut pending,
                &mut count
            ),
            ComputeBackend::Cpu
        );
    }
    #[test]
    fn memory_emergency_bypasses_dwell_time() {
        let mut pending = None;
        let mut count = 0;
        assert_eq!(
            stabilize_selection(
                Some(ComputeBackend::Gpu),
                ComputeBackend::Cpu,
                Duration::ZERO,
                true,
                &mut pending,
                &mut count
            ),
            ComputeBackend::Cpu
        );
    }
}
