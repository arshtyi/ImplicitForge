use crate::scene::Scene;
use bytemuck::{Pod, Zeroable};
use glam::{Vec3, Vec4, Vec4Swizzles, vec3};
use rayon::prelude::*;
use std::{f32::consts::PI, ops::Add};

const TILE: usize = 16;
const FLOOR_Y: f32 = -1.2;
const FAR: f32 = 32.0;
const HIT_EPSILON: f32 = 0.001;
const LIGHT: Vec3 = vec3(-0.48, 0.80, 0.36);

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub radius: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: 0.58,
            pitch: 0.31,
            radius: 7.35,
        }
    }
}

/// Uniform shared with WGSL. Right and up include the perspective scale.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Params {
    pub dims: [u32; 4],
    pub eye: [f32; 4],
    pub forward: [f32; 4],
    pub right: [f32; 4],
    pub up: [f32; 4],
    pub tint: [f32; 4],
}

impl Params {
    pub fn new(scene: &Scene, camera: Camera, width: u32, height: u32) -> Self {
        let (sy, cy) = camera.yaw.sin_cos();
        let (sp, cp) = camera.pitch.sin_cos();
        let outward = vec3(sy * cp, sp, cy * cp);
        let right = vec3(cy, 0.0, -sy);
        let up = outward.cross(right);
        // Set the field of view on the shorter dimension to preserve framing on resize.
        let scale = 23.0_f32.to_radians().tan() / width.min(height).max(1) as f32;
        Self {
            dims: [width, height, scene.code.len() as u32, 4],
            eye: (outward * camera.radius).extend(0.0).into(),
            forward: (-outward).extend(0.0).into(),
            right: (right * (width as f32 * scale)).extend(0.0).into(),
            up: (up * (height as f32 * scale)).extend(0.0).into(),
            tint: Vec3::from_array(scene.tint).extend(0.0).into(),
        }
    }

    /// Use the pixel center only for a lower-cost render.
    pub fn single_sample(mut self) -> Self {
        self.dims[3] = 1;
        self
    }
}

pub struct Frame {
    pub pixels: Vec<u32>,
    /// Primary and shadow march samples. Normal probes only contribute to evals.
    pub steps: u64,
    /// Primitive evaluations across all distance queries.
    pub evals: u64,
}

#[derive(Default)]
struct Stats {
    steps: u64,
    evals: u64,
}

impl Add for Stats {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            steps: self.steps + rhs.steps,
            evals: self.evals + rhs.evals,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Surface {
    Object,
    Floor,
}

struct Kernel<'a, const PRUNE: bool, const COUNT: bool> {
    scene: &'a Scene,
    params: &'a Params,
}

impl<const PRUNE: bool, const COUNT: bool> Kernel<'_, PRUNE, COUNT> {
    fn step(&self, p: Vec3, stats: &mut Stats) -> f32 {
        if COUNT {
            stats.steps += 1;
        }
        self.scene.distance::<PRUNE, COUNT>(p, &mut stats.evals)
    }

    fn hit(&self, eye: Vec3, dir: Vec3, stats: &mut Stats) -> Option<(f32, Surface)> {
        let plane = if dir.y < -0.0001 && eye.y >= FLOOR_Y {
            (FLOOR_Y - eye.y) / dir.y
        } else {
            f32::INFINITY
        };
        let limit = plane.min(FAR);
        let mut t = 0.0;
        for _ in 0..128 {
            if t > limit {
                break;
            }
            // The magnitude also gives a conservative exit step from a solid interior.
            let d = self.step(eye + t * dir, stats).abs();
            if d < HIT_EPSILON {
                return Some((t, Surface::Object));
            }
            t += d;
        }
        (plane <= FAR).then_some((plane, Surface::Floor))
    }

    fn normal(&self, p: Vec3, stats: &mut Stats) -> Vec3 {
        let mut difference = |axis: Vec3| {
            let offset = axis * 0.0015;
            self.scene
                .distance::<PRUNE, COUNT>(p + offset, &mut stats.evals)
                - self
                    .scene
                    .distance::<PRUNE, COUNT>(p - offset, &mut stats.evals)
        };
        unit_or_up(vec3(
            difference(Vec3::X),
            difference(Vec3::Y),
            difference(Vec3::Z),
        ))
    }

    fn visibility(&self, p: Vec3, n: Vec3, stats: &mut Stats) -> f32 {
        let origin = p + 0.006 * n;
        let mut s = 0.012;
        let mut visibility = 1.0_f32;
        for _ in 0..48 {
            if s > 12.0 {
                break;
            }
            let d = self.step(origin + s * LIGHT, stats);
            if d < HIT_EPSILON {
                return 0.0;
            }
            // The minimum apparent clearance estimates a soft directional shadow.
            visibility = visibility.min(12.0 * d / s);
            s += d;
        }
        visibility
    }

    fn ray(&self, x: f32, y: f32) -> Vec3 {
        let p = self.params;
        let u = 2.0 * x / p.dims[0] as f32 - 1.0;
        let v = 1.0 - 2.0 * y / p.dims[1] as f32;
        (Vec4::from(p.forward).xyz() + u * Vec4::from(p.right).xyz() + v * Vec4::from(p.up).xyz())
            .normalize()
    }

    fn pixel(&self, x: usize, y: usize, stats: &mut Stats) -> u32 {
        let (x, y) = (x as f32, y as f32);
        if self.params.dims[3] == 1 {
            return pack(self.shade(self.ray(x + 0.5, y + 0.5), stats));
        }
        let mut sum = Vec3::ZERO;
        for dy in [0.25, 0.75] {
            for dx in [0.25, 0.75] {
                sum += self.shade(self.ray(x + dx, y + dy), stats);
            }
        }
        pack(sum * 0.25)
    }

    fn shade(&self, dir: Vec3, stats: &mut Stats) -> Vec3 {
        let eye = Vec4::from(self.params.eye).xyz();
        let sky = vec3(0.20, 0.30, 0.40).lerp(
            vec3(0.055, 0.095, 0.18),
            (dir.y * 1.8 + 0.18).clamp(0.0, 1.0),
        );
        let Some((t, surface)) = self.hit(eye, dir, stats) else {
            return sky;
        };
        let p = eye + t * dir;
        let (n, base, gloss, rim) = match surface {
            Surface::Object => {
                let n = self.normal(p, stats);
                let tint = Vec4::from(self.params.tint).xyz();
                let base = tint.lerp(
                    vec3(0.95, 0.55, 0.21),
                    0.22 * (p.y * 0.55 + 0.48).clamp(0.0, 1.0),
                );
                (n, base, 0.40, (1.0 - n.dot(-dir).clamp(0.0, 1.0)).powi(3))
            }
            Surface::Floor => {
                let pattern = 1.0 + 0.055 * (PI * p.x).cos() * (PI * p.z).cos();
                (Vec3::Y, vec3(0.115, 0.165, 0.205) * pattern, 0.08, 0.0)
            }
        };
        let nl = n.dot(LIGHT).max(0.0);
        let visibility = if nl > 0.0 {
            self.visibility(p, n, stats)
        } else {
            1.0
        };
        let specular = n.dot(unit_or_up(LIGHT - dir)).max(0.0).powi(32) * visibility * nl;
        let lit = base * ((0.30 + 0.10 * n.y) + 0.82 * nl * visibility)
            + vec3(0.95, 0.82, 0.65) * (specular * gloss)
            + vec3(0.12, 0.22, 0.30) * rim;
        let fog = ((t - 6.0) / 26.0).clamp(0.0, 1.0);
        lit.lerp(sky, fog * fog)
    }

    /// Split a strip into disjoint column ranges. All row handles fit on the stack.
    fn columns(
        &self,
        rows: &mut [&mut [u32]],
        x: usize,
        y: usize,
        parallel: bool,
        cancel: &(impl Fn() -> bool + Sync),
    ) -> Option<Stats> {
        let tiles = rows[0].len().div_ceil(TILE);
        if tiles == 1 {
            if cancel() {
                return None;
            }
            let mut stats = Stats::default();
            for (dy, row) in rows.iter_mut().enumerate() {
                for (dx, out) in row.iter_mut().enumerate() {
                    *out = self.pixel(x + dx, y + dy, &mut stats);
                }
            }
            return Some(stats);
        }
        let cut = (tiles / 2) * TILE;
        let mut tail: [&mut [u32]; TILE] = Default::default();
        let tail = &mut tail[..rows.len()];
        for (row, rest) in rows.iter_mut().zip(tail.iter_mut()) {
            (*row, *rest) = std::mem::take(row).split_at_mut(cut);
        }
        let mut left = || self.columns(rows, x, y, parallel, cancel);
        let mut right = || self.columns(tail, x + cut, y, parallel, cancel);
        if parallel {
            let (a, b) = rayon::join(left, right);
            Some(a? + b?)
        } else {
            Some(left()? + right()?)
        }
    }
}

fn unit_or_up(v: Vec3) -> Vec3 {
    let n2 = v.length_squared();
    if n2 > 1e-12 { v / n2.sqrt() } else { Vec3::Y }
}

fn pack(c: Vec3) -> u32 {
    let encode = |v: f32| (v.clamp(0.0, 1.0).sqrt() * 255.0 + 0.5) as u32;
    (encode(c.x) << 16) | (encode(c.y) << 8) | encode(c.z)
}

/// Both schedulers render the same clipped 16x16 tiles directly into the final image.
/// Counters are tile-local, with no atomic increments on the distance-query path.
pub fn cpu<const PRUNE: bool, const COUNT: bool>(
    scene: &Scene,
    params: &Params,
    parallel: bool,
    cancel: &(impl Fn() -> bool + Sync),
) -> Option<Frame> {
    if cancel() {
        return None;
    }
    let [w, h, _, _] = params.dims;
    let (w, h) = (w as usize, h as usize);
    let mut pixels = vec![0; w.checked_mul(h)?];
    let kernel = Kernel::<PRUNE, COUNT> { scene, params };
    let strip = |(i, buf): (usize, &mut [u32])| {
        let height = buf.len() / w;
        let mut it = buf.chunks_mut(w);
        let mut rows: [&mut [u32]; TILE] = std::array::from_fn(|_| it.next().unwrap_or_default());
        kernel.columns(&mut rows[..height], 0, i * TILE, parallel, cancel)
    };
    let stats = if pixels.is_empty() {
        Stats::default()
    } else if parallel {
        pixels
            .par_chunks_mut(w * TILE.min(h))
            .enumerate()
            .map(strip)
            .try_reduce(Stats::default, |a, b| Some(a + b))?
    } else {
        pixels
            .chunks_mut(w * TILE.min(h))
            .enumerate()
            .try_fold(Stats::default(), |a, item| Some(a + strip(item)?))?
    };
    (!cancel()).then_some(Frame {
        pixels,
        steps: stats.steps,
        evals: stats.evals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{DEMO_COUNT, demo, fixture};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn torus_floor_and_open_sky() {
        let scene = fixture(1, 1.0, false);
        let params = Params::new(&scene, Camera::default(), 1, 1);
        let k = Kernel::<false, true> {
            scene: &scene,
            params: &params,
        };
        let mut stats = Stats::default();
        let eye = vec3(0.33, 0.0, 0.0);
        let (t, surface) = k.hit(eye, -Vec3::Y, &mut stats).unwrap();
        assert_eq!(surface, Surface::Object);
        assert!((t - 0.91).abs() < HIT_EPSILON);
        assert_eq!(stats.steps, stats.evals);
        assert!(k.normal(eye - t * Vec3::Y, &mut stats).y > 0.999);
        assert_eq!(stats.evals, stats.steps + 6);
        // A vertical ray passes through the torus hole and lands on the floor.
        assert_eq!(
            k.hit(Vec3::ZERO, -Vec3::Y, &mut stats),
            Some((-FLOOR_Y, Surface::Floor))
        );
        assert_eq!(k.hit(Vec3::ZERO, Vec3::Y, &mut stats), None);
        let in_shadow = vec3(0.33, -1.04, 0.0) - (0.16 / LIGHT.y) * LIGHT;
        assert_eq!(k.visibility(in_shadow, Vec3::Y, &mut stats), 0.0);
        assert_eq!(
            k.visibility(vec3(-3.0, FLOOR_Y, 0.0), Vec3::Y, &mut stats),
            1.0
        );
    }

    #[test]
    fn supersampling_matches_linear_resolve_of_double_resolution() {
        let scene = demo(0, [0.5; 3]);
        let params = |w, h| Params::new(&scene, Camera::default(), w, h);
        let render = |p: Params| cpu::<false, true>(&scene, &p, false, &|| false).unwrap();
        let aa = render(params(31, 23));
        let center = render(params(31, 23).single_sample());
        let fine = render(params(62, 46).single_sample());
        assert_eq!((aa.steps, aa.evals), (fine.steps, fine.evals));
        let mut changed_edge = false;
        for (i, &pixel) in aa.pixels.iter().enumerate() {
            let start = (i / 31 * 62 + i % 31) * 2;
            for shift in [0, 8, 16] {
                let channel = |rgb: u32| ((rgb >> shift) & 255u32) as f32;
                let samples = [0, 1, 62, 63].map(|j| channel(fine.pixels[start + j]));
                // Squaring undoes the renderer's square-root display encoding.
                let expected = (samples.map(|v| v * v).iter().sum::<f32>() * 0.25).sqrt();
                // Packed references lose highlight detail above the display range.
                if !samples.contains(&255.0) {
                    assert!((channel(pixel) - expected).abs() <= 1.01);
                }
                let contrast = samples.into_iter().fold(0.0, f32::max)
                    - samples.into_iter().fold(255.0, f32::min);
                changed_edge |=
                    contrast > 16.0 && (channel(pixel) - channel(center.pixels[i])).abs() > 2.0;
            }
        }
        assert!(changed_edge);
    }

    #[test]
    fn clipped_tiles_match_row_major_reference_and_baseline() {
        let scene = fixture(9, 1.0, false);
        let params = Params::new(&scene, Camera::default(), 35, 19);
        let k = Kernel::<false, true> {
            scene: &scene,
            params: &params,
        };
        let mut reference = Vec::new();
        let mut stats = Stats::default();
        for y in 0..19 {
            for x in 0..35 {
                reference.push(k.pixel(x, y, &mut stats));
            }
        }
        for parallel in [false, true] {
            let baseline = cpu::<false, true>(&scene, &params, parallel, &|| false).unwrap();
            let pruned = cpu::<true, true>(&scene, &params, parallel, &|| false).unwrap();
            assert_eq!(baseline.pixels, reference);
            assert_eq!((baseline.steps, baseline.evals), (stats.steps, stats.evals));
            assert_eq!(pruned.pixels, reference);
            assert_eq!(pruned.steps, stats.steps);
            assert!(pruned.evals < stats.evals);
        }
    }

    #[test]
    fn default_camera_frames_demo_bounds_in_portrait_and_landscape() {
        let scene = fixture(1, 1.0, false);
        for (w, h) in [(101, 51), (51, 101), (51, 51)] {
            let params = Params::new(&scene, Camera::default(), w, h);
            let k = Kernel::<false, false> {
                scene: &scene,
                params: &params,
            };
            let eye = Vec4::from(params.eye).xyz();
            let f = Vec4::from(params.forward).xyz();
            let r = Vec4::from(params.right).xyz();
            let u = Vec4::from(params.up).xyz();
            assert!(k.ray(w as f32 * 0.5, h as f32 * 0.5).dot(-eye.normalize()) > 0.99999);
            assert!(k.ray(0.5, 0.5).dot(r) < 0.0 && k.ray(0.5, 0.5).dot(u) > 0.0);
            for x in [-2.15, 2.15] {
                for y in [-1.18, 1.90] {
                    for z in [-1.6, 1.6] {
                        let displacement = vec3(x, y, z) - eye;
                        let depth = displacement.dot(f);
                        assert!(depth > 0.0 && depth < FAR);
                        assert!(displacement.dot(r).abs() < depth * r.length_squared());
                        assert!(displacement.dot(u).abs() < depth * u.length_squared());
                    }
                }
            }
        }
    }

    #[test]
    fn each_gallery_model_has_a_visible_silhouette_with_margin() {
        let (w, h) = (80, 50);
        for index in 0..DEMO_COUNT {
            let scene = demo(index, [0.5; 3]);
            let params = Params::new(&scene, Camera::default(), w, h);
            let kernel = Kernel::<true, false> {
                scene: &scene,
                params: &params,
            };
            let eye = Vec4::from(params.eye).xyz();
            let mut object_pixels = 0;
            let mut stats = Stats::default();
            for y in 0..h {
                for x in 0..w {
                    let ray = kernel.ray(x as f32 + 0.5, y as f32 + 0.5);
                    if let Some((_, Surface::Object)) = kernel.hit(eye, ray, &mut stats) {
                        assert!(
                            x > 0 && x + 1 < w && y > 0 && y + 1 < h,
                            "{} touches the frame",
                            scene.name
                        );
                        object_pixels += 1;
                    }
                }
            }
            assert!(
                object_pixels > w * h / 30,
                "{} occupies too little of the default view",
                scene.name
            );
        }
    }

    #[test]
    fn demos_match_with_instrumentation_disabled() {
        for index in 0..DEMO_COUNT {
            let scene = demo(index, [0.5; 3]);
            let params = Params::new(&scene, Camera::default(), 23, 17);
            let baseline = cpu::<false, true>(&scene, &params, false, &|| false).unwrap();
            let pruned = cpu::<true, false>(&scene, &params, true, &|| false).unwrap();
            assert_eq!(pruned.pixels, baseline.pixels);
            assert_eq!((pruned.steps, pruned.evals), (0, 0));
        }
    }

    #[test]
    fn cancellation_between_tiles_discards_the_partial_frame() {
        let scene = fixture(1, 1.0, false);
        let params = Params::new(&scene, Camera::default(), 3 * TILE as u32, TILE as u32);
        for parallel in [false, true] {
            let calls = AtomicUsize::new(0);
            // Enter the frame, render one tile, then cancel at the next tile boundary.
            let cancel = || calls.fetch_add(1, Ordering::Relaxed) >= 2;
            assert!(cpu::<false, false>(&scene, &params, parallel, &cancel).is_none());
            assert!(calls.load(Ordering::Relaxed) >= 3);
        }
    }
}
