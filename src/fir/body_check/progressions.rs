use super::*;
use crate::fir::{FirConversionKind, FirExprKind, FirProgressionClass, FirStatementKind};

impl BodyFirChecker<'_> {
    /// `DefaultProgressionHandler`: a loop over a value of a progression class reads the value's
    /// `first`, `last` and `step`. The class comes from the iterable's most precise type
    /// (`getMostPreciseTypeFromValInitializer`), which sees through implicit conversions and from a
    /// `val` read to its initializer. A smart cast the loop does not need is not applied: every
    /// progression class declares the members the loop reads.
    pub(super) fn progression_loop_header(
        &self,
        variable: LocalValueId,
        variable_ty: ResolvedTy,
        iterable: FirExprId,
    ) -> Option<FirLoopHeader> {
        let progression = FirProgressionClass::of(self.most_precise_type(iterable)?)
            .filter(|progression| progression.counter.ty() == variable_ty.get())?;
        let mut value = iterable;
        while let Some(FirExprKind::ImplicitConversion { value: inner, conversion }) =
            self.body.expr(value).map(|expression| &expression.kind)
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
        Some(FirLoopHeader::Progression {
            variable,
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
