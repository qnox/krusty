//! JVM continuation spill-field grouping, physical field indices, and encounter order.

use std::collections::{HashMap, HashSet};

use super::{is_suspension_point, object_ty};
use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};
use crate::types::Ty;

/// The continuation-field type for a spilled local. A `Unit`-typed local spills as the `kotlin/Unit`
/// object reference — a JVM field cannot carry the `void` ("V") descriptor that `Ty::Unit` produces,
/// and the live value across the suspension is the `Unit` singleton.
pub(super) fn spill_field_ty(ty: Ty) -> Ty {
    if ty == Ty::Unit {
        Ty::obj("kotlin/Unit")
    } else {
        ty
    }
}

/// The spill kind of a reference local. `@DebugMetadata`'s `n`/`s` arrays list these first and keep
/// every other spill in the order it was spilled; field layout instead groups by kind, in the
/// first-spill order recorded by [`SpillLayout`].
const REFERENCE_SPILL_KIND: char = 'L';

/// The per-kind spill field letter (kotlinc: references `L$`, ints `I$`, longs `J$`, …).
fn spill_kind(ty: &Ty) -> char {
    if ty.is_reference() {
        REFERENCE_SPILL_KIND
    } else {
        match *ty {
            Ty::Long => 'J',
            Ty::Float => 'F',
            Ty::Double => 'D',
            Ty::Boolean => 'Z',
            Ty::Char => 'C',
            Ty::Byte => 'B',
            Ty::Short => 'S',
            _ => 'I',
        }
    }
}

/// A local of the bottom type (`var x = null` — `Ty::Null`) has exactly one possible value, so
/// kotlinc gives it no continuation field and rematerializes it in every resume arm. Keeping it out
/// of the spill layout also preserves its verifier `null` type instead of widening it to `Object`.
pub(super) fn is_rematerialized_null(ty: &Ty) -> bool {
    matches!(ty.non_null(), Ty::Null)
}

/// The entries of a suspension's scope list that a resume arm rematerializes rather than reloads.
pub(super) fn rematerialized_nulls(list: &[(u32, Ty)]) -> Vec<u32> {
    list.iter()
        .filter(|(_, ty)| is_rematerialized_null(ty))
        .map(|&(local, _)| local)
        .collect()
}

/// Annotate each scope-list entry with its representation kind and position within that kind.
pub(super) fn kind_positions(list: &[(u32, Ty)]) -> Vec<(u32, Ty, char, u32)> {
    let mut counts = HashMap::<char, u32>::new();
    list.iter()
        .filter(|(_, ty)| !is_rematerialized_null(ty))
        .map(|&(local, ty)| {
            let kind = spill_kind(&ty);
            let position = counts.entry(kind).or_insert(0);
            let current = *position;
            *position += 1;
            (local, ty, kind, current)
        })
        .collect()
}

/// Positional entries in kotlinc's store order: references first, then primitives, preserving each
/// partition's scope-list order. Resume loads traverse this sequence in reverse.
pub(super) fn spill_order(list: &[(u32, Ty)]) -> Vec<(u32, Ty, char, u32)> {
    let mut positions = kind_positions(list);
    positions.sort_by_key(|&(_, _, kind, _)| kind != REFERENCE_SPILL_KIND);
    positions
}

/// Suspension points in final-body evaluation order. Spill field groups follow the first store of
/// each representation kind, so folding their layout through a `HashMap` would make class layout
/// depend on randomized map iteration rather than emitted code order.
pub(super) fn suspension_points_in_order(
    ir: &IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
) -> Vec<ExprId> {
    fn walk(
        ir: &IrFile,
        expression: ExprId,
        suspend_set: &HashSet<u32>,
        seen: &mut HashSet<ExprId>,
        out: &mut Vec<ExprId>,
    ) {
        if is_suspension_point(ir, expression, suspend_set) && seen.insert(expression) {
            out.push(expression);
        }
        if let IrExpr::Lambda { captures, .. } = &ir.exprs[expression as usize] {
            for &capture in captures {
                walk(ir, capture, suspend_set, seen, out);
            }
            return;
        }
        for_each_child(&ir.exprs, expression, &mut |child| {
            walk(ir, child, suspend_set, seen, out)
        });
    }

    let mut out = Vec::new();
    walk(ir, body, suspend_set, &mut HashSet::new(), &mut out);
    out
}

/// Positional spill-field layout: per-kind maxima over every suspension's scope list.
#[derive(Clone, Default)]
pub(super) struct SpillLayout {
    max: HashMap<char, u32>,
    /// Representation kinds in the order their first spill store appears in the final body.
    order: Vec<char>,
}

impl SpillLayout {
    pub(super) fn add_list(&mut self, list: &[(u32, Ty)]) {
        let mut counts = HashMap::<char, u32>::new();
        for (_, ty) in list {
            if is_rematerialized_null(ty) {
                continue;
            }
            let kind = spill_kind(ty);
            if !self.order.contains(&kind) {
                self.order.push(kind);
            }
            *counts.entry(kind).or_insert(0) += 1;
        }
        for (kind, count) in counts {
            let maximum = self.max.entry(kind).or_insert(0);
            *maximum = (*maximum).max(count);
        }
    }

    /// Field index relative to the first spill field; callers add the machine's field prefix.
    pub(super) fn slot(&self, kind: char, position: u32) -> u32 {
        let mut offset = 0;
        for &ordered_kind in &self.order {
            if ordered_kind == kind {
                return offset + position;
            }
            offset += self.count(ordered_kind);
        }
        panic!("spill kind {kind} was not recorded in the field layout")
    }

    /// Fields with representation groups in first-spilled order.
    pub(super) fn fields(&self) -> Vec<(String, Ty)> {
        let mut fields = Vec::new();
        for &kind in &self.order {
            let ty = match kind {
                'L' => object_ty(),
                'J' => Ty::Long,
                'F' => Ty::Float,
                'D' => Ty::Double,
                'Z' => Ty::Boolean,
                'C' => Ty::Char,
                'B' => Ty::Byte,
                'S' => Ty::Short,
                'I' => Ty::Int,
                _ => panic!("unknown JVM spill kind {kind}"),
            };
            for position in 0..self.count(kind) {
                fields.push((format!("{kind}${position}"), ty));
            }
        }
        fields
    }

    fn count(&self, kind: char) -> u32 {
        self.max
            .get(&kind)
            .copied()
            .expect("ordered spill kind has a field count")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrIntrinsicSuspensionKind, IrIntrinsicSuspensionPoint};

    #[test]
    fn suspension_points_follow_the_final_body_order() {
        let mut ir = IrFile::default();
        let first = ir.add_expr(IrExpr::UnitInstance);
        let second = ir.add_expr(IrExpr::UnitInstance);
        for point in [first, second] {
            ir.intrinsic_suspension_points.insert(
                point,
                IrIntrinsicSuspensionPoint {
                    result: Ty::Unit,
                    kind: IrIntrinsicSuspensionKind::Safe,
                },
            );
        }
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![first, second],
            value: None,
        });

        assert_eq!(
            suspension_points_in_order(&ir, body, &HashSet::new()),
            [first, second],
        );
    }

    #[test]
    fn references_lead_stores_without_changing_field_group_order() {
        let values = [
            (0, Ty::Int),
            (1, Ty::obj("test/SpillReference")),
            (2, Ty::Long),
        ];
        assert_eq!(
            spill_order(&values)
                .into_iter()
                .map(|(local, _, _, _)| local)
                .collect::<Vec<_>>(),
            [1, 0, 2]
        );

        let mut layout = SpillLayout::default();
        layout.add_list(&values);
        assert_eq!(
            layout
                .fields()
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["I$0", "L$0", "J$0"]
        );
    }
}
