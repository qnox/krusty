//! Which cast operands kotlinc proves non-null, so that `as T` carries no null guard.
//!
//! kotlinc's lowering guards every cast to a non-null type with
//! `Intrinsics.checkNotNull(value, "null cannot be cast to non-null type T")`, whatever the operand's
//! declared type: an operand typed non-null can still be `null` at run time (a property read before
//! its initializer ran, an unchecked generic result). Its bytecode post-pass then deletes each
//! guard whose operand a nullability analysis of the method proves non-null. That analysis starts
//! from values the method itself produced or checked: a constant, a `new`, a class literal, a value
//! that already passed a null check (`!!`, a parameter's entry assertion, an earlier guarded cast),
//! and locals only ever stored from such values. Everything else, a call result, a field, `this`,
//! a string template, is unknown and keeps its guard.

use std::collections::{HashMap, HashSet};

use crate::ir::{IrExpr, IrFile, IrTypeOp};

use super::Emitter;

/// Every value stored into each local of one body: its initializer and each later assignment.
#[derive(Default)]
pub(super) struct ValueStores {
    stores: HashMap<u32, Vec<u32>>,
}

impl ValueStores {
    /// Collect the stores reachable inside one body. Value indices restart for every body, so a
    /// lambda's own body is a separate domain and only its captures belong to this one.
    pub(super) fn collect(ir: &IrFile, roots: &[u32]) -> Self {
        let mut stores = Self::default();
        let mut seen = HashSet::new();
        let mut pending = roots.to_vec();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            match ir.expr(expression) {
                IrExpr::Variable { index, init, .. } => {
                    let values = stores.stores.entry(*index).or_default();
                    values.extend(init.iter().copied());
                }
                IrExpr::SetValue { var, value } => {
                    stores.stores.entry(*var).or_default().push(*value);
                }
                _ => {}
            }
            match ir.expr(expression) {
                IrExpr::Lambda { captures, .. } => pending.extend(captures.iter().copied()),
                _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                    pending.push(child)
                }),
            }
        }
        stores
    }
}

/// How far a proof follows locals stored from other locals before giving up. A cycle (`v = v`)
/// ends here too; giving up only keeps a guard.
const PROOF_DEPTH: usize = 8;

impl Emitter<'_> {
    /// Whether kotlinc's nullability analysis proves the value of `expression` non-null.
    pub(super) fn known_non_null(&self, expression: u32) -> bool {
        self.known_non_null_within(expression, PROOF_DEPTH)
    }

    fn known_non_null_within(&self, expression: u32, depth: usize) -> bool {
        let Some(depth) = depth.checked_sub(1) else {
            return false;
        };
        match self.ir.expr(expression) {
            IrExpr::Const(constant) => !matches!(constant, crate::ir::IrConst::Null),
            IrExpr::New { .. }
            | IrExpr::ClassConst { .. }
            | IrExpr::KClassLiteral { .. }
            | IrExpr::NotNullAssert { .. }
            | IrExpr::Throw { .. } => true,
            IrExpr::TypeOp { op, arg, .. } => match op {
                IrTypeOp::CastNonNull => true,
                IrTypeOp::Cast | IrTypeOp::ImplicitCoercion => {
                    self.known_non_null_within(*arg, depth)
                }
                _ => false,
            },
            IrExpr::Block {
                value: Some(value), ..
            } => self.known_non_null_within(*value, depth),
            IrExpr::When { branches } => {
                branches.iter().any(|(condition, _)| condition.is_none())
                    && branches
                        .iter()
                        .all(|&(_, result)| self.known_non_null_within(result, depth))
            }
            IrExpr::GetValue(value) => {
                self.checked_parameters.contains(value)
                    || self.value_stores.stores.get(value).is_some_and(|stores| {
                        !stores.is_empty()
                            && stores
                                .iter()
                                .all(|&stored| self.known_non_null_within(stored, depth))
                    })
            }
            _ => false,
        }
    }
}
