use super::*;
use crate::fir::{
    FirCallArgument, FirConversionKind, FirExprKind, FirProgressionClass, FirProgressionSource,
    FirRangeCounterKind, FirStatementKind,
};
use crate::libraries::CompilerIntrinsic;
use crate::resolve::ResolvedCall;

impl BodyFirChecker<'_> {
    /// kotlinc's `HeaderInfoBuilder`: a loop whose iterable is a progression is counted. The
    /// iterable is matched as a `kotlin.ranges` builder (`downTo`, `until`, `step`, `reversed`), a
    /// range literal, or a progression value, in that order; `step` and `reversed` apply to the
    /// progression their receiver builds, when that one has an inclusive last bound.
    pub(super) fn progression_loop_header(
        &self,
        variable: LocalValueId,
        variable_ty: ResolvedTy,
        iterable_source: ExprId,
        iterable: FirExprId,
    ) -> Option<FirLoopHeader> {
        let counter = FirRangeCounterKind::of(variable_ty.get()).filter(|counter| {
            matches!(
                counter,
                FirRangeCounterKind::Int | FirRangeCounterKind::Long | FirRangeCounterKind::Char
            )
        })?;
        let source = self.progression_source(iterable_source, iterable)?;
        let matches_counter = match &source {
            FirProgressionSource::Value { progression, .. } => progression.counter == counter,
            _ => self
                .body
                .expr(iterable)
                .and_then(|expression| FirProgressionClass::of(expression.ty.get().non_null()))
                .is_some_and(|progression| progression.counter == counter),
        };
        matches_counter.then_some(FirLoopHeader::Progression {
            variable,
            counter,
            source,
        })
    }

    fn progression_source(
        &self,
        source: ExprId,
        checked: FirExprId,
    ) -> Option<FirProgressionSource> {
        if let Some(built) = self.progression_builder(source, checked) {
            return Some(built);
        }
        match &self.body.expr(checked)?.kind {
            FirExprKind::Range {
                operation,
                start,
                end,
                start_type,
                end_type,
            } if Ty::range_counter_type_for(start_type.get(), end_type.get())
                .and_then(FirRangeCounterKind::of)
                .is_some() =>
            {
                Some(FirProgressionSource::Literal {
                    operation: *operation,
                    start: *start,
                    end: *end,
                })
            }
            _ => self.progression_value(checked),
        }
    }

    /// A call to one of the `kotlin.ranges` builders, matched on the selected declaration.
    fn progression_builder(
        &self,
        source: ExprId,
        checked: FirExprId,
    ) -> Option<FirProgressionSource> {
        let Some(ResolvedCall::Extension(extension)) = self.info.resolved_calls.get(&source) else {
            return None;
        };
        let intrinsic = super::calls::selected_extension_intrinsic(extension)?;
        let Expr::Call { callee, args } = self.file.expr(source) else {
            return None;
        };
        let Expr::Member {
            receiver: receiver_source,
            ..
        } = self.file.expr(*callee)
        else {
            return None;
        };
        let FirExprKind::Call(call) = &self.body.expr(checked)?.kind else {
            return None;
        };
        let receiver = call.extension_receiver.filter(|receiver| {
            receiver.conversion.is_none_or(|conversion| {
                matches!(
                    conversion.kind,
                    FirConversionKind::SmartCast { .. }
                        | FirConversionKind::NullabilityWidening { .. }
                )
            })
        })?;
        let argument = match (&call.arguments[..], &args[..]) {
            ([], []) => None,
            (
                [FirCallArgument::Expression {
                    value,
                    conversion: None,
                    ..
                }],
                [argument_source],
            ) => Some((*value, *argument_source)),
            _ => return None,
        };
        let nested = |this: &Self| this.progression_source(*receiver_source, receiver.value);
        match (intrinsic, argument) {
            (CompilerIntrinsic::RangeDownTo | CompilerIntrinsic::RangeUntil, Some((end, _))) => {
                Some(FirProgressionSource::Literal {
                    operation: if intrinsic == CompilerIntrinsic::RangeDownTo {
                        FirRangeOperation::DownTo
                    } else {
                        FirRangeOperation::Until
                    },
                    start: receiver.value,
                    end,
                })
            }
            (CompilerIntrinsic::ProgressionStep, Some((step, _))) => nested(self)
                .filter(FirProgressionSource::has_inclusive_last)
                .map(|nested| FirProgressionSource::Step {
                    nested: Box::new(nested),
                    step,
                }),
            (CompilerIntrinsic::ProgressionReversed, None) => nested(self)
                .filter(FirProgressionSource::has_inclusive_last)
                .map(|nested| FirProgressionSource::Reversed(Box::new(nested))),
            _ => None,
        }
    }

    /// `DefaultProgressionHandler`: a value of a progression class is read through its `first`,
    /// `last` and `step`. The class comes from the value's most precise type
    /// (`getMostPreciseTypeFromValInitializer`), which sees through implicit conversions and from a
    /// `val` read to its initializer. A smart cast the loop does not need is not applied: every
    /// progression class declares the members the loop reads.
    fn progression_value(&self, iterable: FirExprId) -> Option<FirProgressionSource> {
        let progression = FirProgressionClass::of(self.most_precise_type(iterable)?)?;
        let mut value = iterable;
        while let Some(FirExprKind::ImplicitConversion {
            value: inner,
            conversion,
        }) = self.body.expr(value).map(|expression| &expression.kind)
        {
            let progression_before = self
                .body
                .expr(*inner)
                .and_then(|inner| FirProgressionClass::of(inner.ty.get().non_null()));
            if !matches!(
                conversion.kind,
                FirConversionKind::SmartCast { .. } | FirConversionKind::NullabilityWidening { .. }
            ) || progression_before.is_none()
            {
                break;
            }
            value = *inner;
        }
        Some(FirProgressionSource::Value {
            progression,
            iterable: value,
        })
    }

    fn most_precise_type(&self, expression: FirExprId) -> Option<Ty> {
        let checked = self.body.expr(expression)?;
        match &checked.kind {
            FirExprKind::ImplicitConversion { value, .. } => self.most_precise_type(*value),
            FirExprKind::ValueRead(value) => match self.immutable_local_initializer(*value) {
                Some(initializer) => self.most_precise_type(initializer),
                None => Some(checked.ty.get().non_null()),
            },
            _ => Some(checked.ty.get().non_null()),
        }
    }

    /// The initializer of a `val` declared earlier in this body.
    fn immutable_local_initializer(&self, value: LocalValueId) -> Option<FirExprId> {
        (0..self.body.statement_count()).find_map(|raw| {
            match &self
                .body
                .statement(crate::fir::FirStatementId::from_raw(raw as u32))?
                .kind
            {
                FirStatementKind::Local {
                    target,
                    mutable: false,
                    lateinit: false,
                    initializer,
                    ..
                } if *target == value => *initializer,
                _ => None,
            }
        })
    }
}
