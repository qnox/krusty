//! `I { … }` and an implicit conversion of a lambda to a functional interface in a KLIB body.
//!
//! The serialized `SAM_CONVERSION` names the interface and wraps the function value it converts.
//! A lambda literal becomes the implementation of that interface's single abstract method, as
//! checked FIR lowering records a source lambda converted to it: the lambda carries the checked
//! target, and every backend realizes the implementing class from it. The target's shape comes from
//! the same provider-normalized discovery source calls use.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr, IrSamMethod, IrSamTarget, IrSamTypeParameter};
use crate::metadata::klib_ir::tree::KlibIrExprId;
use crate::symbol_resolver::{SamMethodDeclaration, SamSignature};
use crate::types::Ty;

impl BodyLowering<'_> {
    /// The lambda `argument`, converted to the functional interface `target`, typed `ty`.
    pub(super) fn sam_conversion(
        &mut self,
        target: Ty,
        argument: KlibIrExprId,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if ty != target {
            return Err(mismatch("a SAM conversion is typed apart from its target"));
        }
        let signature = self.linker.sam_signature(target)?;
        let sam = sam_target(&signature)?;
        let function = self.expression(argument)?;
        let Some(IrExpr::Lambda { sam: slot, .. }) = self.ir.exprs.get_mut(function as usize)
        else {
            return unsupported("a SAM conversion of a function value that is no lambda literal");
        };
        if slot.is_some() {
            return Err(mismatch("a lambda converts to two functional interfaces"));
        }
        *slot = Some(Box::new(sam));
        Ok(self.typed(function, ty))
    }
}

/// The checked target a lambda implements, from the interface shape the provider published.
fn sam_target(signature: &SamSignature) -> Lowered<IrSamTarget> {
    let method_target = match signature.declaration {
        Some(SamMethodDeclaration::External(callable)) => IrSamMethod::External(callable),
        Some(SamMethodDeclaration::FunctionTypeInvoke) => IrSamMethod::FunctionTypeInvoke,
        Some(SamMethodDeclaration::Module(_)) | None => {
            return unsupported("a SAM conversion to a method with no dependency identity");
        }
    };
    if signature.suspend || signature.has_receiver || signature.context_count != 0 {
        return unsupported("a SAM conversion of a suspend, receiver or context method");
    }
    Ok(IrSamTarget {
        classifier: signature.internal,
        method: signature.method.clone(),
        method_target,
        parameters: signature.params.clone(),
        contravariant_parameters: signature.contravariant_params.clone(),
        result: signature.ret,
        declared_parameters: signature.declared_params.clone(),
        declared_result: signature.declared_ret,
        context_count: 0,
        has_receiver: false,
        suspend: false,
        source_suspend: false,
        overridden_results: signature.overridden_results.clone(),
        function_adapter: false,
        wraps_function_value: false,
        nullable: false,
        kotlin_interface: signature.kotlin_interface,
        parameter_identities: signature.parameter_identities.to_vec(),
        type_parameters: signature
            .type_parameters
            .iter()
            .map(|parameter| IrSamTypeParameter {
                name: parameter.name.clone(),
                bounds: parameter.bounds.clone(),
            })
            .collect(),
    })
}
