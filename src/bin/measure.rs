use implicit_forge::{
    gpu::Gpu,
    render::{Camera, Frame, Params, cpu},
    resample::blit,
    scene::{DEMO_COUNT, Scene, demo, fixture},
};
use std::{
    error::Error,
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::Path,
    time::Instant,
};

const SAMPLES: usize = 21;
const TARGET: (u32, u32) = (160, 96);

#[derive(Clone, Copy)]
enum Mode {
    FullSerial,
    PrunedSerial,
    FullTiles,
    PrunedTiles,
    Compute,
}
use Mode::*;

impl Mode {
    fn label(self) -> &'static str {
        match self {
            FullSerial => "serial-full",
            PrunedSerial => "serial-pruned",
            FullTiles => "rayon-full",
            PrunedTiles => "rayon-pruned",
            Compute => "wgpu-full",
        }
    }
}

fn render<const COUNT: bool>(
    mode: Mode,
    scene: &Scene,
    params: &Params,
    gpu: &mut Option<Gpu>,
) -> Result<Frame, String> {
    let parallel = matches!(mode, FullTiles | PrunedTiles);
    match mode {
        FullSerial | FullTiles => {
            Ok(cpu::<false, COUNT>(scene, params, parallel, &|| false).unwrap())
        }
        PrunedSerial | PrunedTiles => {
            Ok(cpu::<true, COUNT>(scene, params, parallel, &|| false).unwrap())
        }
        Compute => Ok(Frame {
            pixels: gpu
                .as_mut()
                .ok_or("no compute adapter")?
                .render(scene, params)?,
            steps: 0,
            evals: 0,
        }),
    }
}

// Use the same displayed RGB8 values for the window and the error metric.
fn error(reference: &[u32], target: (u32, u32), pixels: &[u32], size: (u32, u32)) -> (f64, u32) {
    let (mut sum, mut worst) = (0u64, 0);
    let mut enlarged = Vec::new();
    let displayed = if size == target {
        pixels
    } else {
        let shape = [target.0 as usize, target.1 as usize];
        enlarged.resize(shape[0] * shape[1], 0);
        blit(
            pixels,
            [size.0 as usize, size.1 as usize],
            &mut enlarged,
            shape,
            shape[0],
        );
        &enlarged
    };
    for (&a, &b) in reference.iter().zip(displayed) {
        for shift in [0, 8, 16] {
            let diff = ((a >> shift) & 255).abs_diff((b >> shift) & 255);
            sum += u64::from(diff);
            worst = worst.max(diff);
        }
    }
    (sum as f64 / (reference.len() * 3) as f64, worst)
}

struct Variant {
    mode: Mode,
    size: (u32, u32),
    params: Params,
    counts: Option<(u64, u64)>,
    elapsed: Vec<f64>,
}

fn measure_case(
    out: &mut impl Write,
    id: &str,
    scene: Scene,
    preview: bool,
    gpu: &mut Option<Gpu>,
) -> Result<(), Box<dyn Error>> {
    let camera = Camera::default();
    let target_params = Params::new(&scene, camera, TARGET.0, TARGET.1);
    let reference = render::<false>(FullSerial, &scene, &target_params, gpu)?.pixels;
    let mut variants = Vec::new();
    let mut modes = vec![
        (FullSerial, 1),
        (PrunedSerial, 1),
        (FullTiles, 1),
        (PrunedTiles, 1),
    ];
    if gpu.is_some() {
        modes.push((Compute, 1));
    }
    if preview {
        modes.extend([(PrunedTiles, 4), (PrunedTiles, 2)]);
    }
    for (mode, divisor) in modes {
        let size = (TARGET.0 / divisor, TARGET.1 / divisor);
        let params = Params::new(&scene, camera, size.0, size.1);
        let params = if divisor == 1 {
            params
        } else {
            params.single_sample()
        };
        // Warm the uninstrumented kernel (including shader compilation). Collect counters separately.
        render::<false>(mode, &scene, &params, gpu)?;
        let counts = if matches!(mode, Compute) {
            None
        } else {
            let frame = render::<true>(mode, &scene, &params, gpu)?;
            Some((frame.evals, frame.steps))
        };
        variants.push(Variant {
            mode,
            size,
            params,
            counts,
            elapsed: Vec::with_capacity(SAMPLES),
        });
    }
    // Rotate the first variant to avoid always rewarding the last one with warm caches.
    for sample in 0..SAMPLES {
        for j in 0..variants.len() {
            let index = (j + sample) % variants.len();
            let v = &mut variants[index];
            let start = Instant::now();
            let frame = render::<false>(v.mode, &scene, &v.params, gpu)?;
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            v.elapsed.push(ms);
            let (mae, max) = error(&reference, TARGET, &frame.pixels, v.size);
            let (evals, steps) = v
                .counts
                .map(|(e, s)| (e.to_string(), s.to_string()))
                .unwrap_or_default();
            writeln!(
                out,
                "{id},{},{},{},{sample},{ms:.6},{evals},{steps},{mae:.7},{max}",
                v.mode.label(),
                v.size.0,
                v.size.1
            )?;
        }
    }
    for v in &mut variants {
        v.elapsed.sort_by(f64::total_cmp);
        let quantile = |p: usize| v.elapsed[(p * SAMPLES).div_ceil(100) - 1];
        eprintln!(
            "{id:12} {:14} {:3}x{:<3} p50 {:8.3} ms   p95 {:8.3} ms",
            v.mode.label(),
            v.size.0,
            v.size.1,
            quantile(50),
            quantile(95)
        );
    }
    Ok(())
}

fn gallery(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    for i in 0..DEMO_COUNT {
        let scene = demo(i, [0.5; 3]);
        let (w, h) = (640, 400);
        let params = Params::new(&scene, Camera::default(), w, h);
        let frame = cpu::<true, false>(&scene, &params, true, &|| false).unwrap();
        let mut file = BufWriter::new(File::create(dir.join(format!("demo-{}.ppm", i + 1)))?);
        write!(file, "P6\n{w} {h}\n255\n")?;
        for pixel in frame.pixels {
            file.write_all(&pixel.to_be_bytes()[1..])?;
        }
        file.flush()?;
    }
    Ok(())
}

fn stamp_tree_bench() -> Result<(), Box<dyn Error>> {
    use implicit_forge::scene::Stamp;

    const COUNT: usize = 64;
    const FRAMES: usize = 21;
    let stamps: Vec<_> = (0..COUNT)
        .map(|i| {
            let x = (i % 8) as f32 - 3.5;
            let z = (i / 8) as f32 - 3.5;
            Stamp {
                center: [x * 0.85, -1.04, z * 0.85],
                size: 0.33,
                ..Stamp::default()
            }
        })
        .collect();
    let base = demo(0, [0.5; 3]);
    let mut incremental = base;
    for stamp in stamps.iter().copied() {
        incremental = incremental.with_stamp(stamp);
    }
    let optimized = demo(0, [0.5; 3]).with_stamps(&stamps, true);
    let mut gpu = None;
    let camera = Camera::default();
    let params = Params::new(&optimized, camera, TARGET.0, TARGET.1);
    let reference = render::<false>(FullSerial, &incremental, &params, &mut gpu)?.pixels;
    writeln!(
        io::stdout(),
        "# scene=demo0;stamps=64;layout=8x8;spacing=0.85;radius=0.33;camera=default;resolution={}x{};samples={FRAMES};mode=pruned_tiles",
        TARGET.0,
        TARGET.1
    )?;
    writeln!(
        io::stdout(),
        "case,variant,sample,ms,primitive_evals,ray_steps,mae_rgb8,max_rgb8,differing_pixels"
    )?;
    for (_, scene) in [("incremental", &incremental), ("regrouped", &optimized)] {
        render::<false>(PrunedTiles, scene, &params, &mut gpu)?;
    }
    let counts = [
        render::<true>(PrunedTiles, &incremental, &params, &mut gpu)?,
        render::<true>(PrunedTiles, &optimized, &params, &mut gpu)?,
    ];
    let mut times = [Vec::with_capacity(FRAMES), Vec::with_capacity(FRAMES)];
    for sample in 0..FRAMES {
        for offset in 0..2 {
            let index = (sample + offset) % 2;
            let (variant, scene) = if index == 0 {
                ("incremental", &incremental)
            } else {
                ("regrouped", &optimized)
            };
            let start = Instant::now();
            let frame = render::<false>(PrunedTiles, scene, &params, &mut gpu)?;
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            times[index].push(ms);
            let (mae, max) = error(&reference, TARGET, &frame.pixels, TARGET);
            let differing = frame
                .pixels
                .iter()
                .zip(&reference)
                .filter(|(a, b)| a != b)
                .count();
            writeln!(
                io::stdout(),
                "stamp64,{variant},{sample},{ms:.6},{},{},{mae:.7},{max},{differing}",
                counts[index].evals,
                counts[index].steps
            )?;
        }
    }
    for (variant, samples) in ["incremental", "regrouped"].into_iter().zip(&mut times) {
        samples.sort_by(f64::total_cmp);
        eprintln!("{variant}: p50 {:.3} ms", samples[FRAMES / 2]);
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--stamp-tree"] {
        return stamp_tree_bench();
    }
    let cpu_only = match args.as_slice() {
        [] => false,
        [a] if a == "--cpu" => true,
        [a, dir] if a == "--gallery" => return Ok(gallery(Path::new(dir))?),
        _ => return Err("usage: measure [--cpu | --gallery DIRECTORY | --stamp-tree]".into()),
    };
    let mut gpu = if cpu_only {
        None
    } else {
        match Gpu::new() {
            Ok(gpu) => {
                eprintln!("Compute adapter: {}", gpu.name);
                Some(gpu)
            }
            Err(e) => {
                eprintln!("Compute unavailable: {e}");
                None
            }
        }
    };
    eprintln!(
        "Rayon workers: {}. Timed frames per variant: {SAMPLES}.",
        rayon::current_num_threads()
    );
    let mut out = BufWriter::new(io::stdout().lock());
    writeln!(
        out,
        "case,mode,width,height,sample,ms,primitive_evals,ray_steps,mae_rgb8,max_rgb8"
    )?;
    for (id, count, spacing, chain) in [
        ("few", 8, 0.85, false),
        ("many", 32, 0.85, false),
        ("sparse", 32, 1.65, false),
        ("chain", 32, 0.85, true),
    ] {
        measure_case(
            &mut out,
            id,
            fixture(count, spacing, chain),
            false,
            &mut gpu,
        )?;
    }
    measure_case(&mut out, "sculpture", demo(0, [0.5; 3]), true, &mut gpu)?;
    Ok(out.flush()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_error_uses_all_target_pixels_and_color_channels() {
        let (mae, max) = error(&[0x010203, 0x000609], (2, 1), &[0x010203], (1, 1));
        assert!((mae - 11.0 / 6.0).abs() < 1e-10);
        assert_eq!(max, 6);
    }
}
