//! Optional headless compute renderer. A successful frame includes dispatch,
//! transfer to a staging buffer, mapping, and a copy into the CPU framebuffer.

use std::sync::{Arc, OnceLock, mpsc};
use std::time::Duration;

use crate::render::Params;
use crate::scene::{Instruction, MAX_INSTRUCTIONS, Scene, validate_postfix};
use wgpu::BufferUsages as Usage;

mod selection;

struct FrameBuffers {
    output: wgpu::Buffer,
    staging: wgpu::Buffer,
    bindings: wgpu::BindGroup,
}

const WORKGROUP: u32 = 8;
const PROGRAM_BYTES: u64 = (MAX_INSTRUCTIONS * size_of::<Instruction>()) as u64;

pub struct Gpu {
    pub name: String,
    pub software: bool,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    instructions: wgpu::Buffer,
    frame: Option<FrameBuffers>,
    // wgpu's default uncaptured-error handler panics. Keep the first error so
    // the caller can switch backends even when the driver reports it later.
    failure: Arc<OnceLock<String>>,
}

fn buffer(device: &wgpu::Device, label: &str, size: u64, usage: Usage) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage,
        mapped_at_creation: false,
    })
}

impl Gpu {
    pub fn new() -> Result<Self, String> {
        selection::open().map_err(|e| e.reasons.join("; "))
    }

    fn from_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        name: String,
        software: bool,
    ) -> Result<Self, String> {
        let failure = Arc::new(OnceLock::new());
        let sink = Arc::clone(&failure);
        device.on_uncaptured_error(Arc::new(move |e| {
            let _ = sink.set(format!("wgpu: {e}"));
        }));
        let sink = Arc::clone(&failure);
        device.set_device_lost_callback(move |reason, message| {
            let _ = sink.set(format!("Compute device lost ({reason:?}): {message}"));
        });
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("postfix sphere tracer"),
            source: wgpu::ShaderSource::Wgsl(include_str!("trace.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ray dispatch"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(error) = pollster::block_on(device.pop_error_scope()) {
            return Err(format!("Compute pipeline validation on {name}: {error}"));
        }
        let params = buffer(
            &device,
            "camera",
            size_of::<Params>() as u64,
            Usage::UNIFORM | Usage::COPY_DST,
        );
        let instructions = buffer(
            &device,
            "postfix scene",
            PROGRAM_BYTES,
            Usage::STORAGE | Usage::COPY_DST,
        );
        let mut gpu = Self {
            name,
            software,
            device,
            queue,
            pipeline,
            params,
            instructions,
            frame: None,
            failure,
        };
        // Some drivers defer linking or fail only on the SSBO-to-host path.
        // Confirm a non-black pixel from the real tracer before selecting one.
        let scene = crate::scene::fixture(1, 1., false);
        let probe = Params::new(&scene, Default::default(), 1, 1).single_sample();
        if gpu.render(&scene, &probe)? == [0] {
            return Err("trace dispatch produced an empty probe pixel".into());
        }
        Ok(gpu)
    }

    fn check_device(&self) -> Result<(), String> {
        match self.failure.get() {
            Some(error) => Err(format!("{}: {error}", self.name)),
            None => Ok(()),
        }
    }

    fn allocate_frame(&self, capacity: u64) -> FrameBuffers {
        let output = buffer(
            &self.device,
            "RGB pixels",
            capacity,
            Usage::STORAGE | Usage::COPY_SRC,
        );
        let staging = buffer(
            &self.device,
            "pixel readback",
            capacity,
            Usage::COPY_DST | Usage::MAP_READ,
        );
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame buffers"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.instructions.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        FrameBuffers {
            output,
            staging,
            bindings,
        }
    }

    pub fn render(&mut self, scene: &Scene, params: &Params) -> Result<Vec<u32>, String> {
        self.check_device()?;
        validate_postfix(&scene.code)?;
        let [width, height, n, _] = params.dims;
        if n as usize != scene.code.len() {
            return Err("Camera instruction count disagrees with scene".into());
        }
        let limits = self.device.limits();
        let groups = [width.div_ceil(WORKGROUP), height.div_ceil(WORKGROUP)];
        if width == 0
            || height == 0
            || groups
                .iter()
                .any(|&g| g > limits.max_compute_workgroups_per_dimension)
        {
            return Err(format!("Unsupported compute dimensions {width} x {height}"));
        }
        let maximum = limits
            .max_buffer_size
            .min(u64::from(limits.max_storage_buffer_binding_size));
        let bytes = u64::from(width) * u64::from(height) * 4;
        // Map from offset zero through an aligned end even for odd pixel counts.
        let transfer_bytes = bytes.next_multiple_of(wgpu::MAP_ALIGNMENT);
        if transfer_bytes > maximum {
            return Err(format!(
                "Aligned frame needs {transfer_bytes} bytes, device limit is {maximum}"
            ));
        }
        if self
            .frame
            .as_ref()
            .is_none_or(|f| f.output.size() < transfer_bytes)
        {
            // Nearby sizes and all subsequent smaller refinement levels reuse
            // both buffers and their bind group. Bind at least 16 bytes on GL.
            let capacity = transfer_bytes.next_power_of_two().max(16).min(maximum);
            self.frame = Some(self.allocate_frame(capacity));
            self.check_device()?;
        }
        let frame = self.frame.as_ref().expect("frame allocated above");
        self.queue
            .write_buffer(&self.params, 0, bytemuck::bytes_of(params));
        self.queue
            .write_buffer(&self.instructions, 0, bytemuck::cast_slice(&scene.code));
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &frame.bindings, &[]);
            pass.dispatch_workgroups(groups[0], groups[1], 1);
        }
        encoder.copy_buffer_to_buffer(&frame.output, 0, &frame.staging, 0, transfer_bytes);
        self.queue.submit([encoder.finish()]);
        self.check_device()?;

        let slice = frame.staging.slice(0..transfer_bytes);
        let (tx, rx) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        // Native poll waits for execution AND delivers map callbacks. An
        // unhealthy/timed-out device must never feed a stale staging image.
        if let Err(e) = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(30)),
        }) {
            let error = self
                .failure
                .get_or_init(|| format!("Compute readback poll failed: {e}"));
            return Err(error.clone());
        }
        self.check_device()?;
        rx.try_recv()
            .map_err(|e| format!("Missing readback callback: {e}"))?
            .map_err(|e| format!("Cannot map staging buffer: {e}"))?;
        let pixels = bytemuck::cast_slice(&slice.get_mapped_range()[..bytes as usize]).to_vec();
        frame.staging.unmap();
        Ok(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        render::{Camera, cpu},
        scene::{DEMO_COUNT, Stamp, StampOp, StampShape, demo},
    };

    #[test]
    fn compute_images_match_cpu_when_adapter_available() {
        let mut gpu = match selection::open() {
            Ok(gpu) => gpu,
            Err(error) if !error.attempted_setup => {
                eprintln!("SKIP GPU image comparison: {}", error.reasons.join("; "));
                return;
            }
            Err(error) => panic!("GPU setup or probe failed: {}", error.reasons.join("; ")),
        };
        eprintln!("GPU image comparison on {}", gpu.name);
        let gallery = (0..DEMO_COUNT).map(|i| demo(i, [0.5; 3]));
        let stamped = [
            (StampShape::Box, StampOp::Union),
            (StampShape::Torus, StampOp::Subtract),
            (StampShape::Sphere, StampOp::Intersect),
            (StampShape::Sphere, StampOp::Smooth),
        ]
        .into_iter()
        .map(|(shape, op)| {
            demo(1, [0.3, 0.7, 0.2]).with_stamp(Stamp {
                shape,
                op,
                center: [0.55, 0.35, 0.58],
                size: 0.75,
            })
        });
        let cases = gallery
            .chain(stamped)
            .enumerate()
            .map(|(index, scene)| (scene, [(65, 43), (63, 41), (67, 45)][index % 3]));
        // Hit the longest legal program and the widest legal dispatch without
        // making the CPU reference render a full-resolution image.
        let boundaries = [
            (crate::scene::fixture(128, 1., false), (9, 7)),
            (crate::scene::fixture(1, 1., false), (1440, 1)),
        ];
        for (scene, (w, h)) in cases.chain(boundaries) {
            let params = Params::new(&scene, Camera::default(), w, h);
            for params in [params, params.single_sample()] {
                let reference = cpu::<false, false>(&scene, &params, false, &|| false).unwrap();
                let result = gpu.render(&scene, &params).unwrap();
                assert_eq!(result.len(), reference.pixels.len());
                let mut sum = 0u64;
                let mut outliers = 0;
                for (&a, &b) in result.iter().zip(&reference.pixels) {
                    let mut peak = 0;
                    for shift in [0, 8, 16] {
                        let delta = ((a >> shift) & 255u32).abs_diff((b >> shift) & 255u32);
                        sum += u64::from(delta);
                        peak = peak.max(delta);
                    }
                    outliers += usize::from(peak > 4);
                }
                // Permit rare differently classified silhouette/shadow rays, but
                // not a systematic lighting, channel packing, or camera offset.
                let mae = sum as f64 / (3 * result.len()) as f64;
                assert!(
                    mae <= 0.75 && outliers * 100 <= result.len(),
                    "{} on {}: MAE={mae:.3}/255, {outliers}/{} pixels exceed 4/255",
                    scene.name,
                    gpu.name,
                    result.len()
                );
            }
        }
    }
}
