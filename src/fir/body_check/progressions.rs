use super::*;
use crate::fir::{
    FirCallArgument, FirConversionKind, FirExprKind, FirProgressionClass, FirProgressionSource,
    FirRangeCounterKind, FirRuntimeFunction, FirStatementKind,
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
    ) -> Result<Option<FirLoopHeader>, BodyCheckFailure> {
        let Some(counter) = FirRangeCounterKind::of(variable_ty.get()).filter(|counter| {
            matches!(
                counter,
                FirRangeCounterKind::Int | FirRangeCounterKind::Long | FirRangeCounterKind::Char
            )
        }) else {
            return Ok(None);
        };
        let Some(source) = self.progression_source(iterable_source, iterable)? else {
            return Ok(None);
        };
        let progression = match &source {
            FirProgressionSource::Value { progression, .. } => Some(progression.counter),
            _ => match self.body.expr(iterable) {
                Some(expression) => self
                    .progression_class(expression.ty.get().non_null(), iterable_source)?
                    .map(|progression| progression.counter),
                None => None,
            },
        };
        Ok(
            (progression == Some(counter)).then_some(FirLoopHeader::Progression {
                variable,
                counter,
                source,
            }),
        )
    }

    fn progression_source(
        &self,
        source: ExprId,
        checked: FirExprId,
    ) -> Result<Option<FirProgressionSource>, BodyCheckFailure> {
        if let Some(built) = self.progression_builder(source, checked)? {
            return Ok(Some(built));
        }
        let Some(expression) = self.body.expr(checked) else {
            return Ok(None);
        };
        match &expression.kind {
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
                Ok(Some(FirProgressionSource::Literal {
                    operation: *operation,
                    start: *start,
                    end: *end,
                }))
            }
            _ => self.progression_value(checked, source),
        }
    }

    /// A call to one of the `kotlin.ranges` builders, matched on the selected declaration.
    fn progression_builder(
        &self,
        source: ExprId,
        checked: FirExprId,
    ) -> Result<Option<FirProgressionSource>, BodyCheckFailure> {
        let Some((intrinsic, receiver_source, receiver, argument)) =
            self.progression_builder_call(source, checked)
        else {
            return Ok(None);
        };
        let nested = || -> Result<Option<FirProgressionSource>, BodyCheckFailure> {
            Ok(self
                .progression_source(receiver_source, receiver)?
                .filter(FirProgressionSource::has_inclusive_last))
        };
        Ok(match (intrinsic, argument) {
            (CompilerIntrinsic::RangeDownTo | CompilerIntrinsic::RangeUntil, Some(end)) => {
                Some(FirProgressionSource::Literal {
                    operation: if intrinsic == CompilerIntrinsic::RangeDownTo {
                        FirRangeOperation::DownTo
                    } else {
                        FirRangeOperation::Until
                    },
                    start: receiver,
                    end,
                })
            }
            (CompilerIntrinsic::ProgressionStep, Some(step)) => match nested()? {
                Some(nested) => Some(FirProgressionSource::Step {
                    nested: Box::new(nested),
                    step,
                    last_element: self.progression_last_element(checked, source)?,
                }),
                None => None,
            },
            (CompilerIntrinsic::ProgressionReversed, None) => {
                nested()?.map(|nested| FirProgressionSource::Reversed(Box::new(nested)))
            }
            _ => None,
        })
    }

    /// The selected builder intrinsic, its receiver (source and checked) and its one argument.
    fn progression_builder_call(
        &self,
        source: ExprId,
        checked: FirExprId,
    ) -> Option<(CompilerIntrinsic, ExprId, FirExprId, Option<FirExprId>)> {
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
                [_],
            ) => Some(*value),
            _ => return None,
        };
        Some((intrinsic, *receiver_source, receiver.value, argument))
    }

    /// `DefaultProgressionHandler`: a value of a progression class is read through its `first`,
    /// `last` and `step`. The class comes from the value's most precise type
    /// (`getMostPreciseTypeFromValInitializer`), which sees through implicit conversions and from a
    /// `val` read to its initializer. A smart cast the loop does not need is not applied: every
    /// progression class declares the members the loop reads.
    fn progression_value(
        &self,
        iterable: FirExprId,
        source: ExprId,
    ) -> Result<Option<FirProgressionSource>, BodyCheckFailure> {
        let Some(class) = self.most_precise_type(iterable) else {
            return Ok(None);
        };
        let Some(progression) = self.progression_class(class, source)? else {
            return Ok(None);
        };
        let mut value = iterable;
        while let Some(FirExprKind::ImplicitConversion {
            value: inner,
            conversion,
        }) = self.body.expr(value).map(|expression| &expression.kind)
        {
            let progression_before = self.body.expr(*inner).is_some_and(|inner| {
                inner
                    .ty
                    .get()
                    .non_null()
                    .obj_internal()
                    .and_then(crate::types::wk::progression_class)
                    .is_some()
            });
            if !matches!(
                conversion.kind,
                FirConversionKind::SmartCast { .. } | FirConversionKind::NullabilityWidening { .. }
            ) || !progression_before
            {
                break;
            }
            value = *inner;
        }
        Ok(Some(FirProgressionSource::Value {
            progression: Box::new(progression),
            iterable: value,
        }))
    }

    /// The progression class `class` is, with the `first`, `last` and `step` members resolution
    /// selected from its declarations. A `kotlin.ranges` progression class whose declarations do
    /// not publish them is a missing stable target, never an iterator loop.
    fn progression_class(
        &self,
        class: Ty,
        source: ExprId,
    ) -> Result<Option<FirProgressionClass>, BodyCheckFailure> {
        let Some(kind) = class
            .obj_internal()
            .filter(|_| class.type_args().is_empty())
            .and_then(crate::types::wk::progression_class)
        else {
            return Ok(None);
        };
        let span = self.file.expr_span(source);
        let missing = || self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget);
        let plan = self.info.progression_plan(class).ok_or_else(missing)?;
        let counter = FirRangeCounterKind::of(plan.first.ty)
            .filter(|_| plan.last.ty == plan.first.ty)
            .ok_or_else(missing)?;
        let member = |member: crate::resolve::ProgressionMember| {
            self.property_target_at(
                span,
                None,
                Some(super::properties::ExternalPropertyTarget {
                    property: member.property,
                    receiver: Some(class),
                    parameters: Vec::new(),
                    result: member.ty,
                    extension_receiver_parameter: None,
                }),
            )
        };
        Ok(Some(FirProgressionClass {
            ty: class,
            counter,
            first: member(plan.first)?,
            last: member(plan.last)?,
            step: match kind {
                crate::types::wk::ProgressionClass::Range => None,
                crate::types::wk::ProgressionClass::Progression => Some(member(plan.step)?),
            },
        }))
    }

    /// The `getProgressionLastElement` overload resolution selected for the stepped progression's
    /// class. A stepped progression without one is a missing stable target.
    fn progression_last_element(
        &self,
        stepped: FirExprId,
        source: ExprId,
    ) -> Result<FirRuntimeFunction, BodyCheckFailure> {
        let span = self.file.expr_span(source);
        let plan = self
            .body
            .expr(stepped)
            .and_then(|stepped| self.info.progression_plan(stepped.ty.get().non_null()))
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
        let function = &plan.last_element;
        Ok(FirRuntimeFunction {
            function: function.function,
            parameters: function.parameters.clone(),
            result: function.result,
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
