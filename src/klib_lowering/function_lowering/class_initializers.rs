//! The actual instance/constructor parameter frame of a serialized class initializer.

use super::super::class_initialization::ClassInitialization;
use super::*;

pub(in super::super) fn initializer_header(
    plan: &ClassInitialization<'_, '_>,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    let receiver = plan
        .header
        .class
        .this_receiver
        .as_ref()
        .ok_or_else(|| mismatch("a class initializer has no instance receiver".to_owned()))?;
    let mut values = HashMap::default();
    values.insert(
        receiver.base.symbol.clone(),
        ParameterValue {
            index: 0,
            stable_read: true,
            erased: None,
        },
    );
    let mut params = Vec::new();
    let mut identities = Vec::new();
    for (ordinal, parameter) in plan.header.function.regular_parameters.iter().enumerate() {
        let index = u32::try_from(ordinal + 1).expect("constructor parameters fit u32");
        if values
            .insert(
                parameter.base.symbol.clone(),
                ParameterValue {
                    index,
                    stable_read: true,
                    erased: None,
                },
            )
            .is_some()
        {
            return Err(mismatch(
                "a constructor parameter repeats the instance receiver or another parameter"
                    .to_owned(),
            ));
        }
        params.push(semantic_type(
            plan.header.arena,
            &plan.header.types,
            parameter.ty,
        )?);
        identities.push(IrParameterIdentity::source(parameter.name.clone()));
    }
    let count = params.len();
    Ok(FunctionHeader {
        name: plan.header.function.name.clone(),
        params,
        ret: Ty::Unit,
        identities,
        context_count: 0,
        extension_receiver: false,
        dispatch_receiver: None,
        receiver_slot: Some(plan.header.contract.classifier),
        singleton_extension_unread_receiver: None,
        values,
        defaults: vec![None; count],
        type_parameters: plan.header.types.clone(),
        erasures: HashMap::default(),
        erased_result: None,
    })
}
