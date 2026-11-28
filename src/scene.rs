//! Scene documents describe ordered binary CSG trees. V1 ramps share a knob.
//! V2 ramps select one of three named controls with `{"parameter": i, "range": [a,b]}`.
use bytemuck::{Pod, Zeroable};
use glam::{Vec2, Vec3};
use serde::Deserialize;

mod stamp_tree;

pub const MAX_INSTRUCTIONS: usize = 255;
pub const MAX_STACK: usize = 64;
const MAX_MAGNITUDE: f32 = 1_000_000.0;
const SPHERE: u32 = 0;
const BOX: u32 = 1;
const TORUS: u32 = 2;
const UNION: u32 = 3;
const DIFFERENCE: u32 = 4;
const INTERSECTION: u32 = 5;
const SMOOTH: u32 = 6;
const CYLINDER: u32 = 7;
const SMOOTH_SUBTRACT: u32 = 8;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Instruction {
    pub op: [u32; 4],
    pub center: [f32; 4],
    pub size: [f32; 4],
}

impl Instruction {
    fn new(op: u32, center: Vec3, size: Vec3) -> Self {
        Self {
            op: [op, 0, 0, 0],
            center: center.extend(0.0).to_array(),
            size: size.extend(0.0).to_array(),
        }
    }

    fn operator(op: u32, k: f32) -> Self {
        Self::new(op, Vec3::ZERO, Vec3::new(k, 0.0, 0.0))
    }

    fn is_leaf(&self) -> bool {
        matches!(self.op[0], SPHERE | BOX | TORUS | CYLINDER)
    }

    fn validate(&self) -> Result<(), &'static str> {
        if !self
            .center
            .iter()
            .chain(&self.size)
            .all(|x| x.abs() <= MAX_MAGNITUDE)
        {
            return Err("nonfinite or excessive coordinate");
        }
        // Boxes encode world half-extents. Only surfaces of revolution use op.y.
        if self.op[1] != 0 && !(matches!(self.op[0], TORUS | CYLINDER) && self.op[1] <= 2) {
            return Err("invalid primitive axis");
        }
        let dimensions = match self.op[0] {
            SPHERE => 1,
            BOX => 3,
            TORUS | CYLINDER => 2,
            SMOOTH | SMOOTH_SUBTRACT if self.size[0] < 0.0 => return Err("negative blend width"),
            UNION | DIFFERENCE | INTERSECTION | SMOOTH | SMOOTH_SUBTRACT => 0,
            _ => return Err("unknown opcode"),
        };
        if self.size[..dimensions].iter().any(|&x| x <= 0.0) {
            return Err("nonpositive shape dimension");
        }
        Ok(())
    }
}

/// Validates the storage/stack contract before interpreting or uploading a program.
pub fn validate_postfix(code: &[Instruction]) -> Result<(), String> {
    if code.len() > MAX_INSTRUCTIONS {
        return Err(format!("scene exceeds {MAX_INSTRUCTIONS} instructions"));
    }
    let mut depth = 0;
    for (i, ins) in code.iter().enumerate() {
        ins.validate()
            .map_err(|err| format!("{err} at instruction {i}"))?;
        if ins.is_leaf() {
            depth += 1;
            if depth > MAX_STACK {
                return Err(format!("postfix stack exceeds {MAX_STACK}"));
            }
        } else {
            if depth < 2 {
                return Err(format!("stack underflow at instruction {i}"));
            }
            depth -= 1;
        }
    }
    if depth == 1 {
        Ok(())
    } else {
        Err(format!("expected one result, found {depth}"))
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(untagged)]
enum Scalar {
    Fixed(f32),
    Ramp([f32; 2]),
    Indexed(IndexedRange),
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexedRange {
    parameter: usize,
    range: [f32; 2],
}

#[derive(Clone, Copy)]
struct Sampling {
    version: u32,
    values: [f32; 3],
}

impl Scalar {
    fn sample(self, controls: Sampling, valid: impl Fn(f32) -> bool) -> Result<f32, String> {
        let ([a, b], t) = match self {
            Self::Fixed(x) => ([x, x], 0.0),
            Self::Ramp(pair) if controls.version == 1 => (pair, controls.values[0]),
            Self::Indexed(r) if controls.version == 2 => (
                r.range,
                *controls
                    .values
                    .get(r.parameter)
                    .ok_or("invalid parameter index")?,
            ),
            _ => return Err("range syntax does not match scene schema version".into()),
        };
        if ![a, b].iter().all(|&x| x.abs() <= MAX_MAGNITUDE && valid(x)) {
            return Err("invalid scene scalar or parameter endpoint".into());
        }
        // Weighted endpoints also preserve tiny positive dimensions at t = 1.
        Ok(if a == b { a } else { (1.0 - t) * a + t * b })
    }
}

type Vector = [Scalar; 3];
fn vector(v: Vector, t: Sampling, positive: bool) -> Result<Vec3, String> {
    let [x, y, z] = v.map(|v| v.sample(t, |a| !positive || a > 0.0));
    Ok(Vec3::new(x?, y?, z?))
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Definition {
    Sphere(Vector, Scalar),
    Box(Vector, Vector),
    Torus(Vector, Scalar, Scalar),
    Cylinder(Vector, Scalar, Scalar),
    Union(Box<Self>, Box<Self>),
    Difference(Box<Self>, Box<Self>),
    Intersection(Box<Self>, Box<Self>),
    Smooth(Scalar, Box<Self>, Box<Self>),
    SmoothSubtract(Scalar, Box<Self>, Box<Self>),
}

impl Definition {
    fn emit(self, t: Sampling, code: &mut Vec<Instruction>) -> Result<(), String> {
        use Definition::*;
        let mut join = |a: std::boxed::Box<Self>,
                        b: std::boxed::Box<Self>,
                        op,
                        k|
         -> Result<Instruction, String> {
            a.emit(t, code)?;
            b.emit(t, code)?;
            Ok(Instruction::operator(op, k))
        };
        let instr = match self {
            Sphere(c, r) => Instruction::new(
                SPHERE,
                vector(c, t, false)?,
                Vec3::X * r.sample(t, |x| x > 0.0)?,
            ),
            Box(c, s) => Instruction::new(BOX, vector(c, t, false)?, vector(s, t, true)?),
            Torus(c, r, s) => Instruction::new(
                TORUS,
                vector(c, t, false)?,
                Vec3::new(r.sample(t, |x| x > 0.0)?, s.sample(t, |x| x > 0.0)?, 0.0),
            ),
            Cylinder(c, r, h) => Instruction::new(
                CYLINDER,
                vector(c, t, false)?,
                Vec3::new(r.sample(t, |x| x > 0.0)?, h.sample(t, |x| x > 0.0)?, 0.0),
            ),
            Union(a, b) => join(a, b, UNION, 0.0)?,
            Difference(a, b) => join(a, b, DIFFERENCE, 0.0)?,
            Intersection(a, b) => join(a, b, INTERSECTION, 0.0)?,
            Smooth(k, a, b) => join(a, b, SMOOTH, k.sample(t, |x| x >= 0.0)?)?,
            SmoothSubtract(k, a, b) => join(a, b, SMOOTH_SUBTRACT, k.sample(t, |x| x >= 0.0)?)?,
        };
        if code.len() == MAX_INSTRUCTIONS {
            return Err("scene instruction limit exceeded".into());
        }
        code.push(instr);
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    name: String,
    parameter: Option<String>,
    parameters: Option<[String; 3]>,
    tint: [f32; 3],
    root: Definition,
}

pub fn load_json(text: &str, values: [f32; 3]) -> Result<Scene, String> {
    let doc: Document = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let parameter_names = match (doc.schema_version, doc.parameter, doc.parameters) {
        (1, Some(label), None) if !label.trim().is_empty() => [label, String::new(), String::new()],
        (2, None, Some(labels))
            if labels.iter().enumerate().all(|(i, name)| {
                !name.trim().is_empty() && labels[..i].iter().all(|n| n.trim() != name.trim())
            }) =>
        {
            labels
        }
        (1 | 2, _, _) => return Err("invalid parameter names for scene schema".into()),
        (v, _, _) => return Err(format!("unsupported scene schema version {v}")),
    };
    if doc.name.trim().is_empty() {
        return Err("missing scene name".into());
    }
    if !doc.tint.iter().all(|x| (0.0..=1.0).contains(x)) {
        return Err("tint outside [0,1]".into());
    }
    if !values.iter().all(|v| (0.0..=1.0).contains(v)) {
        return Err("expected finite parameters in [0,1]".into());
    }
    let mut code = Vec::new();
    doc.root.emit(
        Sampling {
            version: doc.schema_version,
            values,
        },
        &mut code,
    )?;
    Scene::compile(doc.name, parameter_names, doc.tint, code)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StampShape {
    #[default]
    Sphere,
    Box,
    Torus,
    Cylinder,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StampOp {
    #[default]
    Union,
    Subtract,
    Intersect,
    Smooth,
    SmoothSubtract,
}

/// World direction of the stamp's local Y axis. Zero preserves legacy Y-axis instructions.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u32)]
pub enum StampAxis {
    #[default]
    Y = 0,
    X = 1,
    Z = 2,
}

impl StampAxis {
    fn from_lane(lane: u32) -> Self {
        match lane {
            0 => Self::Y,
            1 => Self::X,
            2 => Self::Z,
            _ => unreachable!("validated stamp axis"),
        }
    }

    // A quarter turn about Z or X exchanges the axis extents. Signs drop out
    // because all three oriented primitives are mirror-symmetric in local space.
    fn world_extents(self, local: Vec3) -> Vec3 {
        match self {
            Self::Y => local,
            Self::X => Vec3::new(local.y, local.x, local.z),
            Self::Z => Vec3::new(local.x, local.z, local.y),
        }
    }

    fn radial_height(self, q: Vec3) -> Vec2 {
        let (plane, height) = match self {
            Self::Y => (Vec2::new(q.x, q.z), q.y),
            Self::X => (Vec2::new(q.y, q.z), q.x),
            Self::Z => (Vec2::new(q.x, q.y), q.z),
        };
        Vec2::new(plane.length(), height)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stamp {
    pub shape: StampShape,
    pub op: StampOp,
    pub center: [f32; 3],
    /// Sphere or cylinder radius, or torus outer radius.
    pub size: f32,
    pub half_extents: [f32; 3],
    pub half_height: f32,
    pub axis: StampAxis,
    /// Smooth operator blend width in world units, independent of dimensions.
    pub blend: f32,
}

impl Default for Stamp {
    fn default() -> Self {
        Self {
            shape: StampShape::Sphere,
            op: StampOp::Union,
            center: [0.0, 0.25, 1.0],
            size: 0.6,
            half_extents: [0.6; 3],
            half_height: 0.6,
            axis: StampAxis::Y,
            blend: 0.3,
        }
    }
}

impl Stamp {
    fn instructions(self) -> [Instruction; 2] {
        let r = self.size;
        let (op, size) = match self.shape {
            StampShape::Sphere => (SPHERE, Vec3::X * r),
            StampShape::Box => (
                BOX,
                self.axis.world_extents(Vec3::from_array(self.half_extents)),
            ),
            StampShape::Torus => {
                assert!(r <= MAX_MAGNITUDE, "excessive stamp radius");
                (TORUS, Vec3::new(0.72 * r, 0.28 * r, 0.0))
            }
            StampShape::Cylinder => (CYLINDER, Vec3::new(r, self.half_height, 0.0)),
        };
        let mut leaf = Instruction::new(op, Vec3::from_array(self.center), size);
        if matches!(op, TORUS | CYLINDER) {
            leaf.op[1] = self.axis as u32;
        }
        let (op, width) = match self.op {
            StampOp::Union => (UNION, 0.0),
            StampOp::Subtract => (DIFFERENCE, 0.0),
            StampOp::Intersect => (INTERSECTION, 0.0),
            StampOp::Smooth => (SMOOTH, self.blend),
            StampOp::SmoothSubtract => (SMOOTH_SUBTRACT, self.blend),
        };
        let join = Instruction::operator(op, width);
        leaf.validate().expect("valid stamp geometry");
        join.validate().expect("valid stamp operator");
        [leaf, join]
    }
}

#[derive(Debug, Clone, Copy)]
enum Shape {
    Sphere(f32),
    Cuboid(Vec3),
    Torus(f32, f32),
    OrientedTorus(f32, f32, StampAxis),
    Cylinder(f32, f32, StampAxis),
}

impl Shape {
    #[inline]
    fn field(self, q: Vec3) -> f32 {
        match self {
            Self::Sphere(r) => q.length() - r,
            Self::Cuboid(half) => {
                let d = q.abs() - half;
                d.max(Vec3::ZERO).length() + d.max_element().min(0.0)
            }
            Self::Torus(r, s) => Vec2::new(Vec2::new(q.x, q.z).length() - r, q.y).length() - s,
            Self::OrientedTorus(r, s, axis) => {
                (axis.radial_height(q) - Vec2::new(r, 0.0)).length() - s
            }
            Self::Cylinder(r, h, axis) => {
                let d = axis.radial_height(q).abs() - Vec2::new(r, h);
                d.max(Vec2::ZERO).length() + d.max_element().min(0.0)
            }
        }
    }
}

// Each of the six signed support-plane distances is <= the subtree field,
// even inside the solid. An unsigned distance to the box fails this contract.
#[derive(Clone, Copy, Debug)]
struct Bounds {
    lo: Vec3,
    hi: Vec3,
}

impl Bounds {
    fn union(self, other: Self) -> Self {
        Self {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }
    fn center(self) -> Vec3 {
        (self.lo + self.hi) * 0.5
    }
    fn lower(self, p: Vec3) -> f32 {
        (self.lo - p).max(p - self.hi).max_element()
    }
}

type Id = u8;

// Both child indices and all split data are local to the union node. One signed
// plane test is usually enough in a spatially coherent cluster of primitives.
#[derive(Debug)]
enum Visit {
    Split {
        axis: u8,
        mid: f32,
        low: Id,
        high: Id,
        low_edge: f32,
        high_edge: f32,
    },
    Partial {
        unknown: Id,
        known: Id,
        bounds: Bounds,
    },
    Both,
}

impl Visit {
    fn plan(a: Id, ab: Option<Bounds>, b: Id, bb: Option<Bounds>) -> Self {
        match (ab, bb) {
            (Some(ab), Some(bb)) => {
                let delta = (ab.center() - bb.center()).abs();
                let axis = if delta.x >= delta.y && delta.x >= delta.z {
                    0
                } else if delta.y >= delta.z {
                    1
                } else {
                    2
                };
                let (low, lb, high, hb) = if ab.center()[axis] <= bb.center()[axis] {
                    (a, ab, b, bb)
                } else {
                    (b, bb, a, ab)
                };
                Self::Split {
                    axis: axis as u8,
                    mid: (lb.center()[axis] + hb.center()[axis]) * 0.5,
                    low,
                    high,
                    low_edge: lb.hi[axis],
                    high_edge: hb.lo[axis],
                }
            }
            (Some(bounds), None) => Self::Partial {
                unknown: b,
                known: a,
                bounds,
            },
            (None, Some(bounds)) => Self::Partial {
                unknown: a,
                known: b,
                bounds,
            },
            (None, None) => Self::Both,
        }
    }
}

#[derive(Debug)]
enum Node {
    Leaf(Vec3, Shape),
    Union(Id, Id, Visit),
    Difference(Id, Id),
    Intersection(Id, Id),
    Smooth(Id, Id, f32),
    SmoothSubtract(Id, Id, f32),
}

type Subtree = (Id, Option<Bounds>);

impl Node {
    fn leaf(ins: &Instruction) -> (Self, Option<Bounds>) {
        let c = Vec3::from_slice(&ins.center);
        let s = Vec3::from_slice(&ins.size);
        let axis = StampAxis::from_lane(ins.op[1]);
        let (shape, e) = match ins.op[0] {
            SPHERE => (Shape::Sphere(s.x), Vec3::splat(s.x)),
            BOX => (Shape::Cuboid(s), s),
            TORUS => {
                // Keep the original horizontal torus (including every lattice leaf)
                // on its direct field path, without per-query axis dispatch.
                let shape = if axis == StampAxis::Y {
                    Shape::Torus(s.x, s.y)
                } else {
                    Shape::OrientedTorus(s.x, s.y, axis)
                };
                (
                    shape,
                    axis.world_extents(Vec3::new(s.x + s.y, s.y, s.x + s.y)),
                )
            }
            CYLINDER => (
                Shape::Cylinder(s.x, s.y, axis),
                axis.world_extents(Vec3::new(s.x, s.y, s.x)),
            ),
            _ => unreachable!(),
        };
        (
            Self::Leaf(c, shape),
            Some(Bounds {
                lo: c - e,
                hi: c + e,
            }),
        )
    }

    fn join(op: u32, k: f32, (a, ab): Subtree, (b, bb): Subtree) -> (Self, Option<Bounds>) {
        let union_bounds = || ab.zip(bb).map(|(x, y)| x.union(y));
        match op {
            UNION => (Self::Union(a, b, Visit::plan(a, ab, b, bb)), union_bounds()),
            // max(a,-b) and max(a,b) are both >= a. For intersection either child suffices.
            DIFFERENCE => (Self::Difference(a, b), ab),
            SMOOTH_SUBTRACT if k == 0.0 => (Self::Difference(a, b), ab),
            // Polynomial smooth max(a,-b) >= max(a,-b) >= a for every k.
            SMOOTH_SUBTRACT => (Self::SmoothSubtract(a, b, k), ab),
            INTERSECTION => (Self::Intersection(a, b), ab.or(bb)),
            // A positive blend can expand past both operands. Use the exact fallback.
            SMOOTH => (
                Self::Smooth(a, b, k),
                if k == 0.0 { union_bounds() } else { None },
            ),
            _ => unreachable!(),
        }
    }
}

#[derive(Debug)]
pub struct Scene {
    pub name: String,
    pub parameter_names: [String; 3],
    pub tint: [f32; 3],
    pub code: Vec<Instruction>,
    nodes: Vec<Node>,
    root_bounds: Option<Bounds>,
    scale: f32,
}

impl Scene {
    fn compile(
        name: String,
        parameter_names: [String; 3],
        tint: [f32; 3],
        code: Vec<Instruction>,
    ) -> Result<Self, String> {
        validate_postfix(&code)?;
        let mut nodes = Vec::with_capacity(code.len());
        let mut stack: Vec<(Id, Option<Bounds>)> = Vec::with_capacity(MAX_STACK);
        let mut scale = 1.0_f32;
        for ins in &code {
            let c = Vec3::from_slice(&ins.center);
            let s = Vec3::from_slice(&ins.size);
            scale = scale.max(c.abs().max_element() + s.max_element());
            let (node, bounds) = if ins.is_leaf() {
                Node::leaf(ins)
            } else {
                let right = stack.pop().unwrap();
                let left = stack.pop().unwrap();
                Node::join(ins.op[0], s.x, left, right)
            };
            stack.push((nodes.len() as Id, bounds));
            nodes.push(node);
        }
        Ok(Self {
            name,
            parameter_names,
            tint,
            code,
            nodes,
            root_bounds: stack[0].1,
            scale,
        })
    }

    /// Compose `scene op stamp`. Requires two free instructions and valid stamp
    /// dimensions, position and blend width. Reuses the compiled tree.
    pub fn with_stamp(mut self, stamp: Stamp) -> Self {
        assert!(
            self.code.len() + 2 <= MAX_INSTRUCTIONS,
            "stamp exceeds instruction limit"
        );
        let [primitive, operator] = stamp.instructions();
        let c = Vec3::from_slice(&primitive.center);
        let s = Vec3::from_slice(&primitive.size);
        let n = self.nodes.len();
        let (leaf, bounds) = Node::leaf(&primitive);
        let (root, root_bounds) = Node::join(
            operator.op[0],
            operator.size[0],
            ((n - 1) as Id, self.root_bounds),
            (n as Id, bounds),
        );
        self.nodes.extend([leaf, root]);
        self.code.extend([primitive, operator]);
        self.root_bounds = root_bounds;
        self.scale = self
            .scale
            .max(c.abs().max_element() + s.max_element())
            .max(operator.size[0]);
        self
    }

    #[inline]
    pub fn distance<const PRUNE: bool, const COUNT: bool>(&self, p: Vec3, evals: &mut u64) -> f32 {
        // Plane offsets and evaluated distances can both have cancellation at the
        // surface. Scale the guard by the original coordinates, not the residual.
        let guard = if PRUNE {
            32.0 * f32::EPSILON * self.scale.max(p.abs().max_element())
        } else {
            0.0
        };
        self.query::<PRUNE, COUNT>((self.nodes.len() - 1) as Id, p, f32::INFINITY, guard, evals)
    }

    // Only a hard-union ancestor may pass a finite incumbent. A skipped branch
    // then returns the incumbent; callers consume it only through min().
    fn query<const PRUNE: bool, const COUNT: bool>(
        &self,
        id: Id,
        p: Vec3,
        cap: f32,
        guard: f32,
        evals: &mut u64,
    ) -> f32 {
        let exact = |child, evals: &mut u64| {
            self.query::<PRUNE, COUNT>(child, p, f32::INFINITY, guard, evals)
        };
        match self.nodes[id as usize] {
            Node::Leaf(c, shape) => {
                if COUNT {
                    *evals += 1;
                }
                shape.field(p - c)
            }
            Node::Union(a, b, ref plan) if PRUNE => {
                let (first, second, lower) = match *plan {
                    Visit::Split {
                        axis,
                        mid,
                        low,
                        high,
                        low_edge,
                        high_edge,
                    } => {
                        let x = p[axis as usize];
                        if x <= mid {
                            (low, high, high_edge - x)
                        } else {
                            (high, low, x - low_edge)
                        }
                    }
                    Visit::Partial {
                        unknown,
                        known,
                        bounds,
                    } => (unknown, known, bounds.lower(p)),
                    Visit::Both => (a, b, f32::NEG_INFINITY),
                };
                let best = cap.min(self.query::<PRUNE, COUNT>(first, p, cap, guard, evals));
                if lower > best + guard {
                    best
                } else {
                    best.min(self.query::<PRUNE, COUNT>(second, p, best, guard, evals))
                }
            }
            Node::Union(a, b, _) => exact(a, evals).min(exact(b, evals)),
            Node::Difference(a, b) => exact(a, evals).max(-exact(b, evals)),
            Node::Intersection(a, b) => exact(a, evals).max(exact(b, evals)),
            Node::Smooth(a, b, k) => smooth(exact(a, evals), exact(b, evals), k),
            Node::SmoothSubtract(a, b, k) => -smooth(-exact(a, evals), exact(b, evals), k),
        }
    }
}

fn smooth(a: f32, b: f32, k: f32) -> f32 {
    if k == 0.0 {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

const DEMOS: [&str; 9] = [
    include_str!("../scenes/tidal_reliquary.json"),
    include_str!("../scenes/axial_coupler.json"),
    include_str!("../scenes/sliding_gallery.json"),
    include_str!("../scenes/shortwave_radio.json"),
    include_str!("../scenes/harbor_beacon.json"),
    include_str!("../scenes/pocket_automaton.json"),
    include_str!("../scenes/instant_camera.json"),
    include_str!("../scenes/stone_viaduct.json"),
    include_str!("../scenes/teacup_and_saucer.json"),
];
pub const DEMO_COUNT: usize = DEMOS.len();

pub fn demo(index: usize, values: [f32; 3]) -> Scene {
    load_json(DEMOS[index], values).expect("validated built-in demo and normalized parameters")
}

/// Generates 1..=128 tori on an XZ lattice. The left fold and spatial median
/// tree consume identical leaves. Only hard-union grouping/order changes.
pub fn fixture(count: usize, spacing: f32, chain: bool) -> Scene {
    assert!((1..=128).contains(&count) && spacing.is_finite() && spacing > 0.0);
    let cols = (count as f32).sqrt().ceil() as usize;
    let rows = count.div_ceil(cols);
    let mut centers: Vec<_> = (0..count)
        .map(|i| {
            Vec3::new(
                ((i % cols) as f32 - (cols - 1) as f32 * 0.5) * spacing,
                -1.04,
                ((i / cols) as f32 - (rows - 1) as f32 * 0.5) * spacing,
            )
        })
        .collect();
    fn leaf(c: Vec3) -> Instruction {
        Instruction::new(TORUS, c, Vec3::new(0.33, 0.13, 0.0))
    }
    fn balanced(centers: &mut [Vec3], code: &mut Vec<Instruction>) {
        if let [c] = centers {
            code.push(leaf(*c));
            return;
        }
        let lo = centers
            .iter()
            .copied()
            .fold(Vec3::splat(f32::INFINITY), Vec3::min);
        let hi = centers
            .iter()
            .copied()
            .fold(Vec3::splat(f32::NEG_INFINITY), Vec3::max);
        let axis = if hi.x - lo.x >= hi.z - lo.z { 0 } else { 2 };
        let mid = centers.len() / 2;
        centers.select_nth_unstable_by(mid, |a, b| {
            a[axis]
                .total_cmp(&b[axis])
                .then(a[2 - axis].total_cmp(&b[2 - axis]))
        });
        let (a, b) = centers.split_at_mut(mid);
        balanced(a, code);
        balanced(b, code);
        code.push(Instruction::operator(UNION, 0.0));
    }
    let mut code = Vec::with_capacity(count * 2 - 1);
    if chain {
        for (i, &c) in centers.iter().enumerate() {
            code.push(leaf(c));
            if i != 0 {
                code.push(Instruction::operator(UNION, 0.0));
            }
        }
    } else {
        balanced(&mut centers, &mut code);
    }
    Scene::compile(
        "Ring lattice".into(),
        ["Spacing".into(), String::new(), String::new()],
        [0.34, 0.68, 0.78],
        code,
    )
    .expect("valid fixture")
}

#[cfg(test)]
#[path = "scene_tests.rs"]
mod tests;
