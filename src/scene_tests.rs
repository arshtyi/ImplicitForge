use super::*;
use serde_json::{Value, json};

fn document(root: Value) -> Value {
    json!({"schema_version": 1, "name": "test", "parameter": "t", "tint": [0.2,0.3,0.4], "root": root})
}

fn scene(root: Value) -> Scene {
    load_json(&document(root).to_string(), [0.5; 3]).unwrap()
}

fn ball(x: f32, r: f32) -> Value {
    json!({"sphere": [[x,0,0],r]})
}

fn check_field(scene: &Scene, point: Vec3, expected: f32) {
    let a = scene.distance::<false, false>(point, &mut 0);
    let b = scene.distance::<true, false>(point, &mut 0);
    assert!((a - expected).abs() <= 1e-6, "{point}: {a} vs {expected}");
    assert_eq!(a, b, "{point}: pruned field");
    assert_eq!(a, postfix(&scene.code, point), "{point}: postfix field");
}

#[test]
fn translated_primitive_boundaries_and_sign() {
    let c = Vec3::new(2.0, -1.0, 3.0);
    let s = scene(json!({"sphere": [[2,-1,3],2]}));
    check_field(&s, c, -2.0);
    check_field(&s, c + 2.0 * Vec3::Z, 0.0);
    check_field(&s, c - 3.0 * Vec3::Y, 1.0);
    let b = scene(json!({"box": [[2,-1,3],[2,1,3]]}));
    check_field(&b, c, -1.0);
    check_field(&b, c + Vec3::new(-2.0, 1.0, 3.0), 0.0);
    check_field(&b, c + Vec3::new(5.0, 5.0, 0.0), 5.0);
    let t = scene(json!({"torus": [[2,-1,3],2,0.5]}));
    check_field(&t, c, 1.5);
    check_field(&t, c - 2.0 * Vec3::X, -0.5);
    check_field(&t, c + Vec3::new(0.0, -0.5, 2.0), 0.0);
    check_field(&t, c + 2.5 * Vec3::X, 0.0);
}

#[test]
fn ordered_boolean_and_blend_surfaces() {
    let a = ball(0.0, 2.0);
    let b = ball(0.0, 1.0);
    check_field(&scene(json!({"difference":[a,b]})), Vec3::ZERO, 1.0);
    check_field(&scene(json!({"difference":[a,b]})), Vec3::X * 1.5, -0.5);
    check_field(&scene(json!({"difference":[a,b]})), Vec3::X, 0.0);
    check_field(&scene(json!({"difference":[b,a]})), Vec3::X * 1.5, 0.5);
    check_field(&scene(json!({"intersection":[a,b]})), Vec3::X * 1.5, 0.5);
    check_field(&scene(json!({"union":[a,b]})), Vec3::X * 1.5, -0.5);
    check_field(&scene(json!({"smooth":[0,a,b]})), Vec3::X * 1.5, -0.5);
    check_field(&scene(json!({"smooth":[1,b,b]})), Vec3::X * 1.25, 0.0);
    check_field(&scene(json!({"smooth":[1,a,b]})), Vec3::ZERO, -2.0);
}

#[test]
fn pruning_handles_overlap_ties_and_near_ties() {
    // An unsigned box bound here would incorrectly skip the deeper interior.
    check_field(
        &scene(json!({"union":[ball(0.0,1.0),ball(0.0,2.0)]})),
        Vec3::ZERO,
        -2.0,
    );
    for r in [1.0, 1.0 + f32::EPSILON] {
        let s = scene(json!({"union": [ball(0.0,r),ball(0.0,1.0)]}));
        let mut n = 0;
        assert_eq!(s.distance::<true, true>(Vec3::ZERO, &mut n), -r);
        assert_eq!(n, 2, "a tie, or one ulp of slack, cannot authorize pruning");
    }
}

#[test]
fn unknown_smooth_expansion_and_partial_union_bound() {
    // At x=1.2 both original spheres are outside, but their smooth union is inside.
    let puff = json!({"smooth": [1,ball(0.0,1.0),ball(0.0,1.0)]});
    let p = Vec3::X * 1.2;
    let s = scene(json!({"union": [ball(1.2,0.02),puff]}));
    check_field(&s, p, -0.05);
    let s = scene(json!({"union":[ball(99.0,1.0),puff]}));
    let mut n = 0;
    check_field(&s, p, -0.05);
    s.distance::<true, true>(p, &mut n);
    assert_eq!(
        n, 2,
        "unknown subtree visited, remote bounded sibling skipped"
    );
    check_field(&scene(json!({"union": [puff,puff]})), p, -0.05);
}

#[test]
fn finite_union_incumbents_do_not_escape_through_nonunion_operators() {
    let nested = json!({"union": [ball(0.0,3.0),ball(9.0,1.0)]});
    let cavity = json!({"difference": [ball(0.0,4.0), nested]});
    check_field(
        &scene(json!({"union": [ball(0.0,0.5),cavity]})),
        Vec3::ZERO,
        -0.5,
    );
    // The subtractor must keep its true positive distance in empty space.
    let distant = json!({"union":[ball(10.0,1.0),ball(11.0,1.0)]});
    let cavity = json!({"difference": [ball(0.0,4.0),distant]});
    check_field(
        &scene(json!({"union": [ball(0.0,0.5),cavity]})),
        Vec3::ZERO,
        -4.0,
    );
}

#[test]
fn schema_rejects_invalid_endpoints_even_when_not_sampled() {
    let root = ball(0.0, 1.0);
    let mut doc = document(root.clone());
    for knob in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
        assert!(load_json(&doc.to_string(), [knob; 3]).is_err());
    }
    for (key, value) in [
        ("schema_version", json!(2)),
        ("version", json!(1)),
        ("tint", json!([1, 2, 1])),
        ("name", json!(" ")),
        ("parameter", json!("")),
        ("typo", json!(1)),
    ] {
        doc = document(root.clone());
        doc[key] = value;
        assert!(load_json(&doc.to_string(), [0.0; 3]).is_err());
    }
    for bad in [
        json!({"sphere":[[0,0,0],[1,-1]]}),
        json!({"sphere":[[0,0,0],{"parameter":0,"range":[1,2]}]}),
        json!({"box":[[0,0,0],[1,0,1]]}),
        json!({"torus":[[0,0,0],1,-1]}),
        json!({"smooth":[[0,-1],root,root]}),
        json!({"sphere":[[0,0],1]}),
        json!({"cylinder":[[0,0,0],1,[1,-1]]}),
        json!({"smooth_subtract":[[0,-1],root,root]}),
        json!({"not_sdf":[]}),
    ] {
        assert!(load_json(&document(bad).to_string(), [0.0; 3]).is_err());
    }
    let ramp = document(json!({"sphere": [[[0,2],0,0],[1,3]]})).to_string();
    for (t, expected) in [(0.0, 1.0), (0.5, -1.0), (1.0, -3.0)] {
        check_field(
            &load_json(&ramp, [t, 0.7, 0.9]).unwrap(),
            Vec3::X * 2.0,
            expected,
        );
    }
}

#[test]
fn indexed_ranges_select_only_the_named_control() {
    let x = json!({"parameter": 2, "range": [1, 3]});
    let y = json!({"parameter": 0, "range": [4, 0]});
    let r = json!({"parameter": 1, "range": [2, 6]});
    let doc = json!({"schema_version": 2, "name":"test", "parameters":["Y", "Radius", "X"],
        "tint":[1,1,1], "root":{"sphere":[[x,y,0],r]}});
    let s = load_json(&doc.to_string(), [0.25, 1.0, 0.0]).unwrap();
    assert_eq!(s.parameter_names, ["Y", "Radius", "X"]);
    assert_eq!(s.code[0].center, [1.0, 3.0, 0.0, 0.0]);
    assert_eq!(s.code[0].size[0], 6.0);
    for (key, invalid) in [
        ("parameters", json!(["A", "B"])),
        ("parameters", json!(["A", "A", "C"])),
        ("parameters", json!(["A", "B", " "])),
        ("parameter", json!("legacy")),
        ("schema_version", json!(3)),
    ] {
        let mut bad = doc.clone();
        bad[key] = invalid;
        assert!(load_json(&bad.to_string(), [0.0; 3]).is_err());
    }
    for invalid in [
        json!([1, 2]),
        json!({"parameter":3,"range":[1,2]}),
        json!({"parameter":0,"range":[1,-1]}),
        json!({"parameter":0,"range":[1,2],"typo":true}),
    ] {
        let mut bad = doc.clone();
        bad["root"]["sphere"][1] = invalid;
        assert!(load_json(&bad.to_string(), [0.0; 3]).is_err());
    }
}

#[test]
fn postfix_length_arity_stack_and_dimensions() {
    let leaf = Instruction::new(SPHERE, Vec3::ZERO, Vec3::X);
    let op = Instruction::operator(UNION, 0.0);
    let rake = |depth| {
        let mut code = vec![leaf; depth];
        code.extend(std::iter::repeat_n(op, depth - 1));
        code
    };
    assert!(validate_postfix(&rake(MAX_STACK)).is_ok());
    assert!(validate_postfix(&rake(MAX_STACK + 1)).is_err());
    let mut largest = fixture(128, 1.0, true).code;
    assert_eq!(largest.len(), MAX_INSTRUCTIONS);
    assert!(validate_postfix(&largest).is_ok());
    largest.extend([leaf, op]);
    assert!(validate_postfix(&largest).is_err());
    for bad in [
        vec![],
        vec![op],
        vec![leaf, leaf],
        vec![Instruction::operator(99, 0.0)],
        vec![leaf, leaf, Instruction::operator(SMOOTH_SUBTRACT, -0.1)],
        vec![leaf, Instruction::operator(SMOOTH_SUBTRACT, 0.0)],
        vec![Instruction::new(CYLINDER, Vec3::ZERO, Vec3::X)],
        vec![Instruction {
            op: [CYLINDER, 3, 0, 0],
            ..Instruction::new(CYLINDER, Vec3::ZERO, Vec3::ONE)
        }],
        vec![leaf, leaf, Instruction::operator(SMOOTH, -0.1)],
        vec![Instruction::new(SPHERE, Vec3::NAN, Vec3::X)],
        vec![Instruction::new(TORUS, Vec3::ZERO, Vec3::X)],
    ] {
        assert!(validate_postfix(&bad).is_err());
    }
    assert_eq!(std::mem::size_of::<Instruction>(), 48);
}

// Independent stack traversal with the same opcode ABI as the compute shader.
fn postfix(code: &[Instruction], p: Vec3) -> f32 {
    let mut stack = [0.0; MAX_STACK];
    let mut n = 0;
    for ins in code {
        let s = Vec3::from_slice(&ins.size);
        let q = p - Vec3::from_slice(&ins.center);
        let value = match ins.op[0] {
            SPHERE => q.length() - s.x,
            BOX => Shape::Cuboid(s).field(q),
            TORUS | CYLINDER => {
                let (radial, height) = match ins.op[1] {
                    0 => (Vec2::new(q.x, q.z), q.y),
                    1 => (Vec2::new(q.y, q.z), q.x),
                    2 => (Vec2::new(q.x, q.y), q.z),
                    _ => unreachable!(),
                };
                let side = radial.length() - s.x;
                if ins.op[0] == TORUS {
                    Vec2::new(side, height).length() - s.y
                } else {
                    let cap = height.abs() - s.y;
                    Vec2::new(side.max(0.0), cap.max(0.0)).length() + side.max(cap).min(0.0)
                }
            }
            op => {
                let (a, b): (f32, f32) = (stack[n - 2], stack[n - 1]);
                n -= 2;
                match op {
                    UNION => a.min(b),
                    DIFFERENCE => a.max(-b),
                    INTERSECTION => a.max(b),
                    SMOOTH => smooth(a, b, s.x),
                    SMOOTH_SUBTRACT if s.x == 0.0 => a.max(-b),
                    SMOOTH_SUBTRACT => -smooth(-a, b, s.x),
                    _ => unreachable!(),
                }
            }
        };
        stack[n] = value;
        n += 1;
    }
    stack[0]
}

#[test]
fn every_demo_parameter_changes_solid_membership_independently() {
    for i in 0..DEMO_COUNT {
        for axis in 0..3 {
            let scenes = [0.0, 1.0].map(|t| {
                let mut v = [0.5; 3];
                v[axis] = t;
                demo(i, v)
            });
            let mut changes = 0;
            for x in -10..=10 {
                // Vertical spacing must resolve the 0.15-unit gallery roof slab.
                for y in -12..=24 {
                    for z in -6..=6 {
                        let p =
                            Vec3::new(x as f32 + 0.13, (y as f32 + 0.23) * 0.5, z as f32 + 0.17)
                                * 0.19;
                        let signs = scenes.each_ref().map(|s| {
                            let d = s.distance::<false, false>(p, &mut 0);
                            assert_eq!(d, postfix(&s.code, p));
                            assert_eq!(d, s.distance::<true, false>(p, &mut 0));
                            d < 0.0
                        });
                        changes += usize::from(signs[0] != signs[1]);
                    }
                }
            }
            assert!(
                changes > 1,
                "demo {i}, control {axis}: no appreciable volume moved"
            );
        }
    }
}

#[test]
fn historical_default_geometry_is_byte_exact() {
    // FNV-1a of the three original v1 instruction streams sampled at 0.5.
    let expected = [0x6f28ff593f357e8b, 0xc999d113ad9fddf2, 0x3dea52649132215f];
    for (index, expected) in expected.into_iter().enumerate() {
        let s = demo(index, [0.5; 3]);
        let bytes = s.code.iter().flat_map(|ins| {
            ins.op
                .into_iter()
                .chain(ins.center.map(f32::to_bits))
                .chain(ins.size.map(f32::to_bits))
                .flat_map(u32::to_le_bytes)
        });
        let fingerprint = bytes.fold(0xcbf29ce484222325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        });
        assert_eq!(fingerprint, expected, "{} at default settings", s.name);
    }
}

#[test]
fn stamps_append_ordered_boolean_subtrees() {
    let base = || scene(ball(0.0, 1.0));
    let stamp = Stamp {
        center: [1.5, 0.0, 0.0],
        size: 1.0,
        blend: 0.5,
        ..Stamp::default()
    };
    // At 0.75 both operands have distance -0.25. At 1.5 only the stamp is solid.
    for (op, overlap, stamp_only) in [
        (StampOp::Union, -0.25, -1.0),
        (StampOp::Subtract, 0.25, 1.0),
        (StampOp::Intersect, -0.25, 0.5),
        (StampOp::Smooth, -0.375, -1.0),
    ] {
        let s = base().with_stamp(Stamp { op, ..stamp });
        assert_eq!(s.code.len(), 3);
        check_field(&s, Vec3::X * 0.75, overlap);
        check_field(&s, Vec3::X * 1.5, stamp_only);
        assert_eq!(postfix(&s.code, Vec3::X * 1.5), stamp_only);
    }
    // Fill the center with a box after carving a ring out of the sphere.
    let torus = Stamp {
        shape: StampShape::Torus,
        op: StampOp::Subtract,
        center: [0.0; 3],
        ..stamp
    };
    let cube = Stamp {
        shape: StampShape::Box,
        half_extents: [0.8; 3],
        center: [0.0; 3],
        ..stamp
    };
    let p = Vec3::X * 0.72;
    let filled = base().with_stamp(torus).with_stamp(cube);
    let carved = base().with_stamp(cube).with_stamp(torus);
    check_field(&filled, p, -0.08);
    check_field(&carved, p, 0.28);
    check_field(&carved, Vec3::ZERO, -0.44);
    for s in [filled, carved] {
        assert_eq!(s.code.len(), 5);
        validate_postfix(&s.code).unwrap();
        assert_eq!(postfix(&s.code, p), s.distance::<true, false>(p, &mut 0));
    }
}

#[test]
fn oriented_stamp_distances_and_world_bounds() {
    let center = Vec3::new(2.0, -1.0, 3.0);
    for axis in [StampAxis::X, StampAxis::Y, StampAxis::Z] {
        // These are right-handed local -> world rotations, including signs.
        let world = |v: Vec3| match axis {
            StampAxis::X => Vec3::new(v.y, -v.x, v.z),
            StampAxis::Y => v,
            StampAxis::Z => Vec3::new(v.x, -v.z, v.y),
        };
        let make = |shape, extents| {
            let stamp = Stamp {
                shape,
                axis,
                center: center.to_array(),
                size: 2.0,
                half_height: 1.0,
                half_extents: [2.0, 1.0, 3.0],
                ..Stamp::default()
            };
            let [leaf, _] = stamp.instructions();
            let s =
                Scene::compile("probe".into(), Default::default(), [1.; 3], vec![leaf]).unwrap();
            let bounds = s.root_bounds.unwrap();
            let world_extents = world(extents).abs();
            assert_eq!(bounds.lo, center - world_extents);
            assert_eq!(bounds.hi, center + world_extents);
            s
        };
        let cylinder = make(StampShape::Cylinder, Vec3::new(2.0, 1.0, 2.0));
        for (local, d) in [
            (Vec3::ZERO, -1.0),
            (Vec3::new(1.75, 0.0, 0.0), -0.25),
            (Vec3::new(0.0, -0.75, 0.0), -0.25),
            (Vec3::new(0.0, 0.5, -2.0), 0.0),
            (Vec3::new(0.5, -1.0, 0.0), 0.0),
            (Vec3::new(2.0, 1.0, 0.0), 0.0),
            (Vec3::new(3.0, 0.0, 4.0), 3.0),
            (Vec3::new(0.0, 5.0, 1.0), 4.0),
            (Vec3::new(3.0, -5.0, 4.0), 5.0),
        ] {
            check_field(&cylinder, center + world(local), d);
        }
        let cuboid = make(StampShape::Box, Vec3::new(2.0, 1.0, 3.0));
        for (dir, half) in [(Vec3::X, 2.0), (Vec3::Y, 1.0), (Vec3::Z, 3.0)] {
            check_field(&cuboid, center + world(dir * half), 0.0);
            check_field(&cuboid, center - world(dir * (half + 0.5)), 0.5);
        }
        let torus = make(StampShape::Torus, Vec3::new(2.0, 0.56, 2.0));
        check_field(&torus, center + world(Vec3::new(1.44, 0.0, 0.0)), -0.56);
        check_field(&torus, center + world(Vec3::new(0.0, 0.56, 1.44)), 0.0);
    }
}

#[test]
fn independent_blend_and_exact_zero_width() {
    let base = || scene(ball(0.0, 2.0));
    for k in [0.0, 0.25, 1.0] {
        let stamp = Stamp {
            size: 1.0,
            center: [0.0; 3],
            blend: k,
            ..Stamp::default()
        };
        let cut = base().with_stamp(Stamp {
            op: StampOp::SmoothSubtract,
            ..stamp
        });
        // At x=1.5 both the sphere and negated cutter equal -0.5.
        check_field(&cut, 1.5 * Vec3::X, -0.5 + k / 4.0);
        check_field(&cut, Vec3::ZERO, 1.0);
        check_field(&cut, 3.0 * Vec3::X, 1.0);
        let union = base().with_stamp(Stamp {
            op: StampOp::Smooth,
            center: [2.0, 0.0, 0.0],
            ..stamp
        });
        check_field(&union, 1.5 * Vec3::X, -0.5 - k / 4.0);
        assert_eq!(union.root_bounds.is_none(), k > 0.0);
        assert_eq!(cut.root_bounds.unwrap().hi, Vec3::splat(2.0));
        if k == 0.0 {
            let hard = base().with_stamp(Stamp {
                op: StampOp::Subtract,
                ..stamp
            });
            for x in -16..=16 {
                let p = Vec3::X * (x as f32 * 0.25);
                assert_eq!(
                    cut.distance::<true, false>(p, &mut 0),
                    hard.distance::<true, false>(p, &mut 0)
                );
            }
        }
    }
    // A smooth-cut left operand with unknown bounds remains unknown. A hard union
    // must still visit it at this point outside the original support planes.
    let puff = json!({"smooth": [1, ball(0.0,1.0), ball(0.0,1.0)]});
    let cut = json!({"smooth_subtract": [0.5, puff, {"cylinder":[[9,0,0],1,2]}]});
    check_field(
        &scene(json!({"union":[ball(1.2,0.02),cut]})),
        1.2 * Vec3::X,
        -0.05,
    );
}

#[test]
fn regrouped_oriented_stamps_match_ordered_postfix() {
    let base = || scene(ball(-4.0, 0.5));
    let mut stamps = Vec::new();
    for (i, axis) in [StampAxis::X, StampAxis::Y, StampAxis::Z]
        .into_iter()
        .enumerate()
    {
        for (j, shape) in [StampShape::Cylinder, StampShape::Box, StampShape::Torus]
            .into_iter()
            .enumerate()
        {
            stamps.push(Stamp {
                shape,
                axis,
                center: [i as f32 - 1.0, 0.25, j as f32 - 1.0],
                half_extents: [0.25, 1.25, 0.5],
                half_height: 1.5,
                ..Stamp::default()
            });
        }
    }
    let sequential = stamps.iter().copied().fold(base(), Scene::with_stamp);
    let grouped = base().with_stamps(&stamps, true);
    for x in -5..=5 {
        for z in -5..=5 {
            for y in [-0.75, 0.25, 1.0] {
                let p = Vec3::new(x as f32 * 0.45, y, z as f32 * 0.45);
                let expected = postfix(&sequential.code, p);
                check_field(&grouped, p, expected);
            }
        }
    }
}

#[test]
fn lattice_regrouping_preserves_field_and_reduces_work() {
    for count in [1, 19, 64, 128] {
        let a = fixture(count, 1.3, false);
        let b = fixture(count, 1.3, true);
        let mut full = 0;
        let mut pruned = 0;
        for x in -11..=11 {
            for z in -9..=9 {
                let p = Vec3::new(x as f32 * 0.37, -0.96, z as f32 * 0.43);
                let expected = a.distance::<false, true>(p, &mut full);
                assert_eq!(expected, a.distance::<true, true>(p, &mut pruned));
                assert_eq!(expected, b.distance::<false, false>(p, &mut 0));
                assert_eq!(expected, b.distance::<true, false>(p, &mut 0));
            }
        }
        assert_eq!(full, 23 * 19 * count as u64);
        if count >= 64 {
            assert!(pruned < full / 4);
        }
    }
}
