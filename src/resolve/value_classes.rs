//! Value-class semantics the checker owns: which types are value classes, and the commitment of a
//! Java value used as a value class's dispatch receiver.
//!
//! A value class member receives its dispatch receiver as the class's value rather than as an
//! object that could be `null`: `list[0].f()` with `list: ArrayList<V>` runs `V.f` on the value
//! `list[0]` produced. kotlinc therefore commits a platform or enhanced receiver of a value-class
//! type to that non-null type, exactly as it commits an argument, and checks it where it enters
//! (`checkNotNullExpressionValue(list.get(0), "get(...)")`). An ordinary class receiver is left to
//! fail on its own dereference, so only a value-class receiver is narrowed here.

use super::{Checker, ExprLowering, PlatformNarrowing, ResolvedCall};
use crate::ast::{Expr, ExprId};
use crate::types::Ty;

impl Checker<'_> {
    /// True if `t` names a `@JvmInline value class`, independent of which symbol provider owns it.
    ///
    /// Keep this as the single checker-level semantic query. The module provider exports its
    /// `value_field` through the same `LibraryType::value_underlying` shape as decoded dependencies,
    /// so callers must not pair a source-table lookup with a separate classpath fallback. Besides
    /// duplicating policy, that split would fail to recognize value classes supplied by any future
    /// provider added to the federated resolver.
    pub(super) fn ty_is_value_class(&self, t: Ty) -> bool {
        t.non_null().kotlin_class_internal().is_some_and(|name| {
            self.resolver()
                .classifier(name)
                .is_some_and(|class| class.value_underlying.is_some())
        })
    }

    /// Narrow the explicit receiver of every member call and member property read whose receiver
    /// type is a value class. A safe call has already tested its receiver.
    pub(super) fn narrow_value_class_receivers(&mut self) {
        let receivers: Vec<ExprId> = self
            .resolved_calls
            .iter()
            .filter(|(_, call)| matches!(call, ResolvedCall::Member(_)))
            .filter_map(|(&call, _)| match self.file.expr(call) {
                Expr::Call { callee, .. } => match self.file.expr(*callee) {
                    Expr::Member { receiver, .. } => Some(*receiver),
                    _ => None,
                },
                _ => None,
            })
            .chain(
                self.expr_lowers
                    .iter()
                    .filter(|(_, lowering)| {
                        matches!(lowering, ExprLowering::MemberPropertyRead { .. })
                    })
                    .filter_map(|(&read, _)| match self.file.expr(read) {
                        Expr::Member { receiver, .. } => Some(*receiver),
                        _ => None,
                    }),
            )
            .collect();
        for receiver in receivers {
            let received = self.expr_types[receiver.0 as usize].platform_lower_bound();
            if self.ty_is_value_class(received) && !received.admits_null() {
                self.narrow_platform_value(received, receiver, PlatformNarrowing::Argument);
            }
        }
    }
}
