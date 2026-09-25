//! Values crossing into a declaration's erased parameter.
//!
//! A call site sees a declaration's parameters substituted (`T` = `Int`), but the declaration's
//! body is compiled once against its declared types. Where no selected-call boundary realizes the
//! difference (a local function, a context argument of a property accessor), lowering converts a
//! substituted scalar to the declared reference type itself, as a module call's argument policy
//! does.

use crate::fir::{FirConversion, FirConversionKind, FirExprId, FirReceiver, PropertyId};
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

    /// Box a substituted scalar passed to a declared reference parameter. A `Unit` value is left to
    /// its consumer, which materializes the singleton.
    pub(super) fn box_into_erased_parameter(
        &mut self,
        value: ExprId,
        source: Ty,
        declared: Ty,
    ) -> ExprId {
        if source.is_reference() || source == Ty::Unit || !declared.is_reference() {
            return value;
        }
        self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: declared,
        })
    }

    /// Context arguments of a module property accessor, converted to the accessor's declared
    /// context parameters.
    pub(super) fn module_property_context_arguments(
        &mut self,
        target: PropertyId,
        context_arguments: &[FirReceiver],
    ) -> Result<Vec<ExprId>, FirLoweringFailure> {
        let property = self
            .index
            .property(target)
            .ok_or(FirLoweringFailure::MissingProperty(target))?;
        let declared = self
            .index
            .signature(property.declaration)
            .ok_or(FirLoweringFailure::MissingProperty(target))?
            .parameters
            .get(..property.context_parameter_count as usize)
            .ok_or(FirLoweringFailure::MissingProperty(target))?
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        if declared.len() != context_arguments.len() {
            return Err(FirLoweringFailure::MissingProperty(target));
        }
        context_arguments
            .iter()
            .zip(declared)
            .map(|(receiver, declared)| {
                let value = self.expression_with_conversion(receiver.value, receiver.conversion)?;
                let source = self.converted_type(receiver.value, receiver.conversion)?;
                Ok(self.box_into_erased_parameter(value, source, declared))
            })
            .collect()
    }
}
