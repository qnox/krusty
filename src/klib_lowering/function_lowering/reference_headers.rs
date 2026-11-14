//! Explicit reference storage and invocation roles over authenticated serialized wrapper slots.

use super::super::callable_references::DeclarationReference;
use super::*;
use crate::ir::IrGeneratedParameterRole;

pub(in super::super) fn reference_header(
    arena: &KlibIrArena,
    reference: &DeclarationReference<'_>,
    enclosing: &FunctionHeader,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    let function = arena.function(reference.invoke);
    let mut header = raw_function_header_with_class(
        None,
        HeaderPurpose::Body(None),
        arena,
        function,
        enclosing.type_parameters.clone(),
    )?;
    let bound_count = usize::from(reference.receiver.is_some());
    if header.context_count != 0
        || header.extension_receiver
        || header.dispatch_receiver.is_some()
        || header.params.len() != bound_count + reference.arity
    {
        return Err(mismatch(
            "a reference wrapper changed its admitted parameter roles".to_owned(),
        ));
    }
    for (index, parameter) in function.regular_parameters.iter().enumerate() {
        if header
            .values
            .get(&parameter.base.symbol)
            .map(|value| value.index as usize)
            != Some(index)
        {
            return Err(mismatch(
                "a reference wrapper changed its admitted parameter slot".to_owned(),
            ));
        }
        let role = if reference
            .receiver
            .as_ref()
            .is_some_and(|receiver| receiver.parameter.base.symbol == parameter.base.symbol)
        {
            IrGeneratedParameterRole::ReferenceBoundReceiver
        } else {
            IrGeneratedParameterRole::ReferenceInvokeValue {
                ordinal: u32::try_from(index - bound_count).expect("reference arity fits u32"),
            }
        };
        header.identities[index] = IrParameterIdentity::generated(role, None);
    }
    // These slots already exist in the producer's wrapper. Preserve its symbol→slot mapping;
    // unlike lexical closures, a bound reference must not prepend another capture parameter.
    Ok(header)
}
