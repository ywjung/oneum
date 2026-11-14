//! Choosing the graphics adapter the window is drawn with.
//!
//! eframe asks the system for one adapter and gives up when a device cannot be made on
//! it. On machines with an unusual graphics setup (a virtual machine, remote desktop,
//! an old or missing driver) the first choice is often the one that fails while
//! another would work: Direct3D 12 on Windows' software renderer, Vulkan, OpenGL.
//! Here every adapter that can show the window is tried, in order of preference, and
//! the first on which a device can really be made is used. The trial runs on a
//! throw-away instance, because an adapter makes only one device. What is asked of the
//! device is the little that drawing the interface needs, not wgpu's defaults, which
//! some adapters do not meet.
use eframe::egui_wgpu::{self, wgpu};
use std::sync::Arc;

/// eframe's graphics settings with the adapter chosen as described above.
pub fn wgpu_options() -> egui_wgpu::WgpuConfiguration {
    let backends =
        wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL);
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = backends;
    setup.device_descriptor = Arc::new(device_descriptor);
    setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
        select(adapters, surface, backends)
    }));
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    let supported = adapter.limits();
    wgpu::DeviceDescriptor {
        label: Some("OnEum"),
        required_limits: wgpu::Limits {
            // Large enough for a window on a 4K+ display, where the adapter allows it.
            max_texture_dimension_2d: supported.max_texture_dimension_2d.min(8192),
            ..wgpu::Limits::downlevel_webgl2_defaults()
        },
        ..Default::default()
    }
}

/// Lower is preferred: real graphics hardware first, the software renderer last; the
/// system's own interface first (Direct3D 12 on Windows, Metal on a Mac).
fn rank(device_type: wgpu::DeviceType, backend: wgpu::Backend) -> (u8, u8) {
    let kind = match device_type {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    };
    let interface = match backend {
        wgpu::Backend::Dx12 | wgpu::Backend::Metal => 0,
        wgpu::Backend::Vulkan => 1,
        wgpu::Backend::Gl => 2,
        _ => 3,
    };
    (kind, interface)
}

fn summary(info: &wgpu::AdapterInfo) -> String {
    format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type)
}

fn same_adapter(a: &wgpu::AdapterInfo, b: &wgpu::AdapterInfo) -> bool {
    a.name == b.name
        && a.vendor == b.vendor
        && a.device == b.device
        && a.backend == b.backend
        && a.device_type == b.device_type
}

fn select(
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
    backends: wgpu::Backends,
) -> Result<wgpu::Adapter, String> {
    for adapter in adapters {
        tracing::info!(adapter = %summary(&adapter.get_info()), "graphics adapter found");
    }
    let mut candidates: Vec<&wgpu::Adapter> = adapters
        .iter()
        .filter(|adapter| surface.is_none_or(|surface| adapter.is_surface_supported(surface)))
        .collect();
    candidates.sort_by_key(|adapter| {
        let info = adapter.get_info();
        rank(info.device_type, info.backend)
    });

    let trial = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: wgpu::InstanceFlags::from_build_config().with_env(),
        backend_options: wgpu::BackendOptions::from_env_or_default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let twins = block_on(trial.enumerate_adapters(backends));
    let mut failures = Vec::new();
    for adapter in candidates {
        let info = adapter.get_info();
        let outcome = match twins
            .iter()
            .find(|twin| same_adapter(&twin.get_info(), &info))
        {
            Some(twin) => block_on(twin.request_device(&device_descriptor(twin)))
                .map(drop)
                .map_err(|e| e.to_string()),
            // Not seen by the trial instance: it cannot be tried apart, so it is taken
            // as it is (eframe reports the error if it fails).
            None => Ok(()),
        };
        match outcome {
            Ok(()) => {
                tracing::info!(adapter = %summary(&info), "drawing with this graphics adapter");
                return Ok(adapter.clone());
            }
            Err(error) => {
                tracing::warn!(adapter = %summary(&info), %error, "graphics adapter cannot be used");
                failures.push(format!("{}: {error}", summary(&info)));
            }
        }
    }
    Err(if adapters.is_empty() {
        "no graphics adapter was found (Direct3D 12, Vulkan or OpenGL 3.3 is needed)".to_string()
    } else if failures.is_empty() {
        let found: Vec<String> = adapters.iter().map(|a| summary(&a.get_info())).collect();
        format!(
            "none of the graphics adapters can show the window: {}",
            found.join(", ")
        )
    } else {
        format!(
            "no graphics adapter could be started: {}",
            failures.join("; ")
        )
    })
}

/// Runs a wgpu future to completion. On native systems wgpu's futures are ready at once.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_comes_before_software_and_the_system_interface_first() {
        use wgpu::{Backend, DeviceType};
        let mut order = [
            (DeviceType::Cpu, Backend::Dx12),
            (DeviceType::IntegratedGpu, Backend::Gl),
            (DeviceType::IntegratedGpu, Backend::Vulkan),
            (DeviceType::DiscreteGpu, Backend::Vulkan),
            (DeviceType::IntegratedGpu, Backend::Dx12),
            (DeviceType::VirtualGpu, Backend::Dx12),
        ];
        order.sort_by_key(|&(kind, backend)| rank(kind, backend));
        assert_eq!(
            order,
            [
                (DeviceType::DiscreteGpu, Backend::Vulkan),
                (DeviceType::IntegratedGpu, Backend::Dx12),
                (DeviceType::IntegratedGpu, Backend::Vulkan),
                (DeviceType::IntegratedGpu, Backend::Gl),
                (DeviceType::VirtualGpu, Backend::Dx12),
                (DeviceType::Cpu, Backend::Dx12),
            ]
        );
    }

    /// On this machine: an adapter is found and a device is made with what is asked.
    #[test]
    fn an_adapter_of_this_machine_is_chosen() {
        let backends = wgpu::Backends::PRIMARY | wgpu::Backends::GL;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapters = block_on(instance.enumerate_adapters(backends));
        if adapters.is_empty() {
            // A build machine without graphics: the error says so.
            let error = select(&adapters, None, backends).unwrap_err();
            assert!(error.contains("no graphics adapter"));
            return;
        }
        let chosen = select(&adapters, None, backends).expect("an adapter is usable");
        let (_device, _queue) = block_on(chosen.request_device(&device_descriptor(&chosen)))
            .expect("the chosen adapter makes a device");
    }
}
