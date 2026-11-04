//! Implicit conversions the checker selects, and where their nodes stand in the source.

use super::super::header::{FirExprId, FirPlatformNarrowingId, FirSamConversionId, OriginId};
use super::super::signature::ResolvedTy;
use super::{FirBody, FirExpr, FirExprKind};

/// A checked implicit conversion selected by the frontend. Lowering applies this decision and does
/// not decide assignability, boxing, coercion, or smart-cast eligibility again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirConversionKind {
    NumericWidening {
        to: ResolvedTy,
    },
    NumericConversion {
        to: ResolvedTy,
    },
    NullabilityWidening {
        to: ResolvedTy,
    },
    SmartCast {
        to: ResolvedTy,
    },
    Sam(FirSamConversionId),
    /// A Java/platform value committed to Kotlin's non-null type. The checker records both the
    /// boundary and the source expression text used by Kotlin's failure; common lowering merely
    /// realizes the already-selected yields-or-throws operation.
    PlatformNarrowing {
        narrowing: FirPlatformNarrowingId,
        to: ResolvedTy,
    },
    /// A regular function value converted to a suspend and/or `Unit`-returning function type, both
    /// shapes selected by the frontend; `ordinal` is its source-order place in its innermost callable.
    FunctionValue {
        from: ResolvedTy,
        to: ResolvedTy,
        ordinal: u32,
    },
    CoerceToUnit,
}

/// A selected conversion and what required it: a call argument, a lambda's result, an assignment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirConversion {
    pub origin: OriginId,
    pub kind: FirConversionKind,
}

impl FirBody {
    /// Wrap the checked `value` in an implicit conversion to `target`.
    ///
    /// kotlinc builds an implicit cast, coercion, widening or SAM wrap at its operand's offsets:
    /// the conversion has no source extent of its own, so it takes the operand's origin. A block
    /// is no operand of its own either: kotlinc converts the statement that yields its value (a
    /// lambda's last expression is its return value), so the conversion takes that value's
    /// origin. What required the conversion (a call argument, a lambda's result, an assignment)
    /// stays on [`FirConversion::origin`].
    pub fn add_implicit_conversion(
        &mut self,
        value: FirExprId,
        target: ResolvedTy,
        conversion: FirConversion,
    ) -> FirExprId {
        let mut yielded = value;
        while let FirExprKind::Block {
            result: Some(result),
            ..
        } = self.expressions[yielded.raw() as usize].kind
        {
            yielded = result;
        }
        let origin = self.expressions[yielded.raw() as usize].origin;
        self.add_expr(FirExpr {
            origin,
            ty: target,
            kind: FirExprKind::ImplicitConversion { value, conversion },
        })
    }
}
