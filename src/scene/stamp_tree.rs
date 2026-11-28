use super::{Instruction, MAX_INSTRUCTIONS, Scene, Stamp, StampOp, UNION};
#[cfg(test)]
use super::{MAX_STACK, validate_postfix};
use glam::Vec3;

impl Scene {
    /// Appends stamps in order, spatially regrouping only consecutive hard unions.
    /// Existing geometry is an opaque left operand; every non-union stamp is a barrier.
    pub fn with_stamps(mut self, stamps: &[Stamp], regroup: bool) -> Self {
        let extra = stamps.len().checked_mul(2).expect("stamp count overflow");
        assert!(
            self.code.len() + extra <= MAX_INSTRUCTIONS,
            "stamps exceed instruction limit"
        );
        if !regroup {
            for stamp in stamps.iter().copied() {
                self = self.with_stamp(stamp);
            }
            return self;
        }
        if stamps.is_empty() {
            return self;
        }
        let mut code = self.code;
        let mut start = 0;
        while start < stamps.len() {
            if stamps[start].op == StampOp::Union {
                let end = (start + 1..stamps.len())
                    .find(|&i| stamps[i].op != StampOp::Union)
                    .unwrap_or(stamps.len());
                let mut leaves: Vec<_> = stamps[start..end]
                    .iter()
                    .map(|stamp| stamp.instructions()[0])
                    .collect();
                append_spatial_union(&mut leaves, &mut code);
                code.push(Instruction::operator(UNION, 0.0));
                start = end;
            } else {
                let [leaf, operator] = stamps[start].instructions();
                code.extend([leaf, operator]);
                start += 1;
            }
        }
        Scene::compile(self.name, self.parameter_names, self.tint, code)
            .expect("valid stamp composition within ABI limits")
    }
}

fn append_spatial_union(leaves: &mut [Instruction], code: &mut Vec<Instruction>) {
    if leaves.len() == 1 {
        code.push(leaves[0]);
        return;
    }
    let (lo, hi) = leaves.iter().fold(
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        |(lo, hi), leaf| {
            let c = Vec3::from_slice(&leaf.center);
            (lo.min(c), hi.max(c))
        },
    );
    let extents = hi - lo;
    let axis = if extents.x >= extents.y && extents.x >= extents.z {
        0
    } else if extents.y >= extents.z {
        1
    } else {
        2
    };
    let mid = leaves.len() / 2;
    leaves.select_nth_unstable_by(mid, |a, b| a.center[axis].total_cmp(&b.center[axis]));
    let (left, right) = leaves.split_at_mut(mid);
    append_spatial_union(left, code);
    append_spatial_union(right, code);
    code.push(Instruction::operator(UNION, 0.0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn regrouping_respects_non_union_barriers_and_preserves_field() {
        let stamps: Vec<_> = (0..12)
            .map(|i| Stamp {
                center: [((i * 7) % 11) as f32 - 5.0, -1.0, (i % 3) as f32],
                op: match i {
                    4 => StampOp::Subtract,
                    8 => StampOp::SmoothSubtract,
                    _ => StampOp::Union,
                },
                ..Stamp::default()
            })
            .collect();
        let mut incremental = super::super::demo(0, [0.5; 3]);
        for stamp in stamps.iter().copied() {
            incremental = incremental.with_stamp(stamp);
        }
        let grouped = super::super::demo(0, [0.5; 3]).with_stamps(&stamps, true);
        assert_eq!(incremental.code.len(), grouped.code.len());
        for x in -8..=8 {
            for y in -4..=2 {
                let p = Vec3::new(x as f32 * 0.37, y as f32 * 0.41, 0.23);
                assert_eq!(
                    incremental.distance::<false, false>(p, &mut 0),
                    grouped.distance::<false, false>(p, &mut 0)
                );
                assert_eq!(
                    incremental.distance::<true, false>(p, &mut 0),
                    grouped.distance::<true, false>(p, &mut 0)
                );
            }
        }
    }

    #[test]
    fn long_union_run_shortens_the_query_path() {
        let stamps: Vec<_> = (0..64)
            .map(|i| Stamp {
                center: [(i % 8) as f32, 0.0, (i / 8) as f32],
                size: 0.33,
                ..Default::default()
            })
            .collect();
        let build = |regroup| super::super::demo(0, [0.5; 3]).with_stamps(&stamps, regroup);
        let (chain, tree) = (build(false), build(true));
        let (mut chain_evals, mut tree_evals) = (0, 0);
        for s in &stamps {
            let p = Vec3::from_array(s.center);
            assert_eq!(
                chain.distance::<true, true>(p, &mut chain_evals),
                tree.distance::<true, true>(p, &mut tree_evals)
            );
        }
        assert!(tree_evals < chain_evals);
    }

    #[test]
    fn regrouped_worst_case_postfix_respects_stack_limit() {
        let stamps: Vec<_> = (0..(MAX_STACK - 1))
            .map(|i| Stamp {
                center: [i as f32, 0.0, 0.0],
                ..Stamp::default()
            })
            .collect();
        let grouped = super::super::demo(0, [0.5; 3]).with_stamps(&stamps, true);
        validate_postfix(&grouped.code).unwrap();
    }
}
