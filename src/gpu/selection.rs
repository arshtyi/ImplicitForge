//! Native headless adapter discovery. Keep driver loaders resident, but only
//! retain a device if the real pipeline and readback both work on it.

use super::{Gpu, PROGRAM_BYTES, Params, WORKGROUP};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use wgpu::{Adapter, Backend, Backends, DeviceType, Instance, InstanceDescriptor, Limits};

// Layout::target caps the final UI frame at 1440 x 1080. Preview and
// measurement frames also fit. Pixels are packed u32s (no texture limits).
const MAX_WIDTH: u32 = 1440;
const MAX_HEIGHT: u32 = 1080;
const STORAGE_BYTES: u32 = MAX_WIDTH * MAX_HEIGHT * 4;
const _: () = assert!(STORAGE_BYTES as u64 >= PROGRAM_BYTES);

fn required_limits(allowed: &Limits) -> Limits {
    Limits {
        max_bind_groups: 1,
        max_bindings_per_bind_group: 3,
        max_uniform_buffers_per_shader_stage: 1,
        max_uniform_buffer_binding_size: size_of::<Params>() as u32,
        max_storage_buffers_per_shader_stage: 2,
        max_storage_buffer_binding_size: STORAGE_BYTES,
        max_buffer_size: u64::from(STORAGE_BYTES),
        // Two live three-descriptor groups while growing the readback buffers.
        max_non_sampler_bindings: 6,
        max_compute_invocations_per_workgroup: WORKGROUP * WORKGROUP,
        max_compute_workgroup_size_x: WORKGROUP,
        max_compute_workgroup_size_y: WORKGROUP,
        max_compute_workgroup_size_z: 1,
        max_compute_workgroups_per_dimension: MAX_WIDTH.max(MAX_HEIGHT).div_ceil(WORKGROUP),
        max_compute_workgroup_storage_size: 0,
        // No dynamic offsets. Offset zero satisfies each adapter's alignment.
        min_uniform_buffer_offset_alignment: allowed.min_uniform_buffer_offset_alignment,
        min_storage_buffer_offset_alignment: allowed.min_storage_buffer_offset_alignment,
        max_texture_dimension_1d: 0,
        max_texture_dimension_2d: 0,
        max_texture_dimension_3d: 0,
        max_texture_array_layers: 0,
        max_dynamic_uniform_buffers_per_pipeline_layout: 0,
        max_dynamic_storage_buffers_per_pipeline_layout: 0,
        max_sampled_textures_per_shader_stage: 0,
        max_samplers_per_shader_stage: 0,
        max_storage_textures_per_shader_stage: 0,
        max_vertex_buffers: 0,
        max_vertex_attributes: 0,
        max_vertex_buffer_array_stride: 0,
        max_inter_stage_shader_components: 0,
        max_color_attachments: 0,
        max_color_attachment_bytes_per_sample: 0,
        // All remaining fields (binding arrays, subgroups, ray tracing, etc.) are zero.
        ..Limits::defaults()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Rank {
    Discrete,
    Integrated,
    Virtual,
    Other,
    Software,
}

fn rank(kind: DeviceType, name: &str) -> Rank {
    let lower = name.to_ascii_lowercase();
    if kind == DeviceType::Cpu
        || [
            "llvmpipe",
            "lavapipe",
            "softpipe",
            "swiftshader",
            "software",
            "basic render driver",
            "mesa offscreen",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return Rank::Software;
    }
    match kind {
        DeviceType::DiscreteGpu => Rank::Discrete,
        DeviceType::IntegratedGpu => Rank::Integrated,
        DeviceType::VirtualGpu => Rank::Virtual,
        DeviceType::Other => Rank::Other,
        DeviceType::Cpu => Rank::Software,
    }
}

struct Candidate {
    adapter: Adapter,
    name: String,
    rank: Rank,
}

#[derive(Default)]
pub(super) struct Failure {
    pub(super) reasons: Vec<String>,
    // Distinguishes a genuinely absent/unsupported adapter from broken device
    // setup, WGSL, or execution in the live parity test.
    pub(super) attempted_setup: bool,
}

fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|cause| {
        let message = cause
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| cause.downcast_ref::<&str>().copied())
            .unwrap_or("no driver diagnostic");
        Err(format!("wgpu panicked: {message}"))
    })
}

fn resident(desc: &InstanceDescriptor, mask: Backends) -> Instance {
    // On WSL automatic Mesa selection can load other native drivers with TLS
    // destructors during GL initialization. Preserve that combined instance.
    static INSTANCES: Mutex<Vec<(Backends, Instance)>> = Mutex::new(Vec::new());
    let mut instances = INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, instance)) = instances.iter().find(|(backends, _)| *backends == mask) {
        return instance.clone();
    }
    let instance = Instance::new(&InstanceDescriptor {
        backends: mask,
        ..desc.clone()
    });
    instances.push((mask, instance.clone()));
    instance
}

fn collect(
    desc: &InstanceDescriptor,
    mask: Backends,
    combined: bool,
    pool: &mut Vec<Candidate>,
    failure: &mut Failure,
) {
    for backend in [Backend::Vulkan, Backend::Metal, Backend::Dx12, Backend::Gl] {
        let bit = Backends::from(backend);
        if !mask.contains(bit) {
            continue;
        }
        let result = guarded(|| {
            let instance = resident(desc, if combined { desc.backends } else { bit });
            Ok(instance.enumerate_adapters(bit))
        });
        match result {
            Ok(adapters) if adapters.is_empty() => failure
                .reasons
                .push(format!("{backend:?}: no headless adapter")),
            Ok(adapters) => {
                for adapter in adapters {
                    let info = match guarded(|| Ok(adapter.get_info())) {
                        Ok(info) => info,
                        Err(e) => {
                            failure
                                .reasons
                                .push(format!("{backend:?} adapter info: {e}"));
                            continue;
                        }
                    };
                    let rank = rank(info.device_type, &info.name);
                    let name = format!(
                        "{} ({:?}, {:?}{})",
                        info.name,
                        info.device_type,
                        backend,
                        if rank == Rank::Software {
                            ", software"
                        } else {
                            ""
                        }
                    );
                    pool.push(Candidate {
                        adapter,
                        name,
                        rank,
                    });
                }
            }
            Err(e) => failure.reasons.push(format!("{backend:?}: {e}")),
        }
    }
}

fn try_candidates(
    candidates: impl IntoIterator<Item = Candidate>,
    failure: &mut Failure,
) -> Option<Gpu> {
    for Candidate {
        adapter,
        name,
        rank,
    } in candidates
    {
        let result = guarded(|| {
            if !adapter
                .get_downlevel_capabilities()
                .flags
                .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
            {
                return Err("missing compute shader support".into());
            }
            let allowed = adapter.limits();
            let limits = required_limits(&allowed);
            let mut missing = Vec::new();
            limits.check_limits_with_fail_fn(&allowed, false, |limit, need, have| {
                missing.push(format!("{limit} requires {need}, adapter has {have}"));
            });
            if !missing.is_empty() {
                return Err(missing.join(", "));
            }
            failure.attempted_setup = true;
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("ImplicitForge compute"),
                    required_features: wgpu::Features::empty(),
                    required_limits: limits,
                    ..Default::default()
                }))
                .map_err(|e| format!("device creation: {e}"))?;
            Gpu::from_device(device, queue, name.clone(), rank == Rank::Software)
        });
        match result {
            Ok(gpu) => return Some(gpu),
            Err(e) => failure.reasons.push(format!("{name}: {e}")),
        }
    }
    None
}

pub(super) fn open() -> Result<Gpu, Failure> {
    let mut desc = InstanceDescriptor::from_env_or_default();
    let native = Backends::VULKAN | Backends::METAL | Backends::DX12 | Backends::GL;
    desc.backends &= Instance::enabled_backend_features() & native;
    // This private device only records direct dispatches. wgpu otherwise
    // compiles two extra indirect-validation pipelines during device setup.
    desc.flags
        .remove(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL);
    let mut failure = Failure::default();
    if desc.backends.is_empty() {
        failure
            .reasons
            .push("No enabled native wgpu backend in the requested mask".into());
        return Err(failure);
    }
    let wsl = cfg!(target_os = "linux") && std::path::Path::new("/dev/dxg").exists();
    let gl_first =
        wsl && std::env::var_os("GALLIUM_DRIVER").as_deref() == Some(std::ffi::OsStr::new("d3d12"));
    let mut pool = Vec::new();
    let mut remaining = desc.backends;
    if gl_first && remaining.contains(Backends::GL) {
        collect(&desc, Backends::GL, false, &mut pool, &mut failure);
        pool.sort_by_key(|c| c.rank);
        let hardware = pool.partition_point(|c| c.rank < Rank::Software);
        if let Some(gpu) = try_candidates(pool.drain(..hardware), &mut failure) {
            return Ok(gpu);
        }
        remaining.remove(Backends::GL);
    }
    collect(&desc, remaining, wsl && !gl_first, &mut pool, &mut failure);
    pool.sort_by_key(|c| c.rank);
    try_candidates(pool, &mut failure).ok_or(failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_devices_across_backends_with_software_last() {
        let mut kinds = [
            rank(DeviceType::DiscreteGpu, "SwiftShader"),
            rank(DeviceType::Other, "D3D12 (hardware)"),
            rank(DeviceType::IntegratedGpu, "integrated"),
            rank(DeviceType::VirtualGpu, "hosted"),
            rank(DeviceType::DiscreteGpu, "discrete"),
        ];
        kinds.sort();
        assert_eq!(
            kinds,
            [
                Rank::Discrete,
                Rank::Integrated,
                Rank::Virtual,
                Rank::Other,
                Rank::Software
            ]
        );
        assert_eq!(rank(DeviceType::Cpu, "unknown"), Rank::Software);
        assert_eq!(
            rank(DeviceType::Other, "D3D12 (Microsoft Basic Render Driver)"),
            Rank::Software
        );
    }

    #[test]
    fn limits_fit_ui_shader_and_allow_restrictive_alignment() {
        let mut allowed = Limits::downlevel_webgl2_defaults();
        allowed.min_storage_buffer_offset_alignment = 512;
        let needs = required_limits(&allowed);
        assert_eq!(needs.min_storage_buffer_offset_alignment, 512);
        assert_eq!(needs.max_storage_buffer_binding_size, 1440 * 1080 * 4);
        assert_eq!(needs.max_compute_workgroups_per_dimension, 180);
        assert_eq!(needs.max_uniform_buffer_binding_size, 96);
        assert_eq!(needs.max_compute_invocations_per_workgroup, 64);
        assert_eq!(needs.max_texture_dimension_2d, 0);
        assert!(needs.check_limits(&needs));
        for insufficient in [
            Limits {
                max_storage_buffers_per_shader_stage: 1,
                ..needs.clone()
            },
            Limits {
                max_buffer_size: needs.max_buffer_size - 1,
                ..needs.clone()
            },
            Limits {
                max_storage_buffer_binding_size: needs.max_storage_buffer_binding_size - 1,
                ..needs.clone()
            },
            Limits {
                max_compute_workgroup_size_y: 7,
                ..needs.clone()
            },
        ] {
            assert!(!needs.check_limits(&insufficient));
        }
    }
}
