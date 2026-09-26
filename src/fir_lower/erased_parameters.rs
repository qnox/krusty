//! Values crossing into a declaration's declared parameter types.
//!
//! A call site sees a declaration's parameters substituted (`T` = `Int`), but the declaration's
//! body is compiled once against its declared types. Where no selected-call boundary carries the
//! difference (a local function's arguments, a specialized property reference's receiver), lowering
//! records it as a semantic coercion from the checked type to the declared one.

use crate::fir::{FirConversion, FirConversionKind, FirExprId};
use crate::ir::{ExprId, IrExpr, IrTypeOp};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    /// The checked type a value has after its recorded conversion.
    pub(super) fn converted_type(
        &self,
        value: FirExprId,
        conversion: Option<FirConversion>,
    ) -> Result<Ty, FirLoweringFailure> {
        Ok(match conversion.map(|conversion| conversion.kind) {
            Some(
                FirConversionKind::NumericWidening { to }
                | FirConversionKind::NumericConversion { to }
                | FirConversionKind::NullabilityWidening { to }
                | FirConversionKind::SmartCast { to }
                | FirConversionKind::PlatformNarrowing { to, .. }
                | FirConversionKind::SuspendFunction { to, .. },
            ) => to.get(),
            Some(FirConversionKind::CoerceToUnit) => Ty::Unit,
            Some(FirConversionKind::Sam(_)) | None => self
                .body
                .expr(value)
                .ok_or(FirLoweringFailure::MissingExpression(value))?
                .ty
                .get(),
        })
    }

    /// `value`, of checked type `actual`, crossing into a slot declared `declared`.
    ///
    /// The comparison is semantic and nothing else: identical types have no boundary to cross, and
    /// what a differing pair costs — a box, an unbox, a `checkcast`, or no instruction — is read
    /// off the physical types by the backend when it emits the coercion.
    pub(super) fn coerce_to_declared(&mut self, value: ExprId, actual: Ty, declared: Ty) -> ExprId {
        if actual == declared {
            return value;
        }
        self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: declared,
        })
    }
}
