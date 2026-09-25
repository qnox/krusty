//! What a `for` loop iterates through, as checked FIR consumes it: the iterator protocol, selected
//! as ordinary operator calls, and for a `kotlin.ranges` progression the `first`, `last` and `step`
//! members kotlinc's `ForLoopsLowering` reads instead of iterating.

use crate::ast::{Expr, ExprId, StmtId};
use crate::diag::Span;
use crate::fir::ExternalPropertyId;
use crate::types::{wk, Ty};

use super::{Checker, CheckerScope, IncDecSite, IteratorProtocolTarget};

/// The members of a `kotlin.ranges` progression class a counted loop reads, selected from the
/// class's own declarations. Only the class identity is compiler-known (`wk::progression_class`);
/// the element type is the selected `first`'s type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgressionPlan {
    pub class: wk::ProgressionClass,
    pub first: ProgressionMember,
    pub last: ProgressionMember,
    pub step: ProgressionMember,
}

/// A selected member property of a progression class, read on a receiver of that class.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgressionMember {
    pub property: ExternalPropertyId,
    /// The selected property's type on the progression receiver.
    pub ty: Ty,
}

impl Checker<'_> {
    pub(super) fn iterator_protocol_target(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: Option<StmtId>,
        iterable_ty: Ty,
        span: Span,
        diagnose: bool,
    ) -> Result<Option<IteratorProtocolTarget>, ()> {
        let Some(iterator) = self.zero_arg_operator_call(
            scope,
            statement.map(IncDecSite::Statement),
            iterable_ty,
            "iterator",
            span,
            diagnose.then_some(span),
        )?
        else {
            return Ok(None);
        };
        let iter_ty = iterator.ret();
        let Some(has_next) = self.zero_arg_operator_call(
            scope,
            statement.map(IncDecSite::Statement),
            iter_ty,
            "hasNext",
            span,
            diagnose.then_some(span),
        )?
        else {
            return Ok(None);
        };
        if has_next.ret() != Ty::Boolean {
            if diagnose {
                self.diags.error(
                    span,
                    format!(
                        "the 'iterator().hasNext()' function of the loop range must return \
                         'Boolean', but returns '{}'.",
                        has_next.ret().source_name()
                    ),
                );
            }
            return Err(());
        }
        let Some(next) = self.zero_arg_operator_call(
            scope,
            statement.map(IncDecSite::Statement),
            iter_ty,
            "next",
            span,
            diagnose.then_some(span),
        )?
        else {
            return Ok(None);
        };
        let elem_ty = next.ret();
        Ok(Some(IteratorProtocolTarget {
            iterator: Box::new(iterator),
            has_next: Box::new(has_next),
            next: Box::new(next),
            iter_ty,
            elem_ty,
        }))
    }

    pub(super) fn record_iterator_protocol(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: Option<StmtId>,
        iterable: ExprId,
        iterable_ty: Ty,
    ) -> Result<Option<Ty>, ()> {
        let diagnose = statement.is_some();
        let Some(target) = self.iterator_protocol_target(
            scope,
            statement,
            iterable_ty,
            self.span(iterable),
            diagnose,
        )?
        else {
            return Ok(None);
        };
        let elem = target.elem_ty;
        self.iterator_protocols.insert(iterable, target);
        Ok(Some(elem))
    }

    /// Select the member plan of every progression class a counted loop over `iterable` may read:
    /// the iterable's own type and, through the `step`/`reversed` calls it is built from, their
    /// receivers' types, and a read's declared type. A `val` initializer's class is selected where
    /// the `val` is declared.
    pub(super) fn record_loop_progression_plans(
        &mut self,
        scope: &CheckerScope<'_>,
        iterable: ExprId,
    ) {
        let mut expression = iterable;
        loop {
            self.record_progression_plan(self.expr_types[expression.0 as usize]);
            // A smart-cast read iterates its binding's declared progression class.
            if let Expr::Name(name) = self.file.expr(expression) {
                if let Some(local) = self.lookup(scope, name) {
                    self.record_progression_plan(local.declared_ty);
                }
            }
            let Expr::Call { callee, .. } = self.file.expr(expression) else {
                return;
            };
            let Expr::Member { receiver, .. } = self.file.expr(*callee) else {
                return;
            };
            expression = *receiver;
        }
    }

    /// Select `first`, `last` and `step` on a `kotlin.ranges` progression class, once per class.
    /// A member the class's declarations do not publish leaves the class without a plan, and a
    /// counted loop that needs one is then rejected by the checker rather than iterated.
    pub(super) fn record_progression_plan(&mut self, ty: Ty) {
        let ty = ty.platform_lower_bound().non_null();
        if !ty.type_args().is_empty() || self.progression_plans.contains_key(&ty) {
            return;
        }
        let Some(class) = ty.obj_internal().and_then(wk::progression_class) else {
            return;
        };
        let resolver = self.resolver();
        let member = |name| {
            let selected = resolver.select_member_property(ty, name)?;
            Some(ProgressionMember {
                property: selected.property?.getter.external_property_identity?,
                ty: selected.ty,
            })
        };
        let (Some(first), Some(last), Some(step)) =
            (member("first"), member("last"), member("step"))
        else {
            return;
        };
        self.progression_plans.insert(
            ty,
            ProgressionPlan {
                class,
                first,
                last,
                step,
            },
        );
    }
}
