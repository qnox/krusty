//! The actual instance/constructor parameter frame of a serialized class initializer.

use super::*;

/// The initializer frame of a same-library file-local class. Slot 0 is the instance receiver at
/// its declared type, including the class's type arguments; the constructor parameters follow.
pub(in super::super) fn local_initializer_header(
    function: &KlibIrFunction,
    receiver: &KlibIrParameter,
    receiver_ty: Ty,
    arena: &KlibIrArena,
    types: &KlibTypeParameters,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    let mut values = HashMap::default();
    bind_slot(&mut values, &receiver.base.symbol, 0)?;
    let mut params = Vec::new();
    let mut identities = Vec::new();
    let mut defaults = Vec::new();
    for (ordinal, parameter) in function.regular_parameters.iter().enumerate() {
        let index = u32::try_from(ordinal + 1).expect("constructor parameters fit u32");
        bind_slot(&mut values, &parameter.base.symbol, index)?;
        params.push(semantic_type_in(arena, types, parameter.ty, resolve)?);
        identities.push(IrParameterIdentity::source(parameter.name.clone()));
        defaults.push(parameter.default_value);
    }
    Ok(member_header(
        BoundParameters {
            name: function.name.clone(),
            params,
            ret: Ty::Unit,
            identities,
            defaults,
            values,
        },
        receiver_ty,
        types.clone(),
    ))
}

/// A member function of a same-library file-local class. Its dispatch receiver is the class.
pub(in super::super) fn local_member_header(
    function: &KlibIrFunction,
    arena: &KlibIrArena,
    types: &KlibTypeParameters,
    receiver_ty: Ty,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    if function.constructor {
        return Err(mismatch(
            "a constructor is not a member function of its class".to_owned(),
        ));
    }
    if !function.type_parameters.is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a generic member of a non-public class",
        ));
    }
    if !function.context_parameters.is_empty()
        || function.extension_receiver.is_some()
        || function.companion_extension_class.is_some()
    {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a member receiver or context of a non-public class",
        ));
    }
    let Some(receiver) = &function.dispatch_receiver else {
        return Err(mismatch(
            "a member of a non-public class has no instance receiver".to_owned(),
        ));
    };
    if semantic_type_in(arena, types, receiver.ty, resolve)? != receiver_ty {
        return Err(mismatch(
            "a member receiver is not its declaring class".to_owned(),
        ));
    }
    let mut values = HashMap::default();
    bind_slot(&mut values, &receiver.base.symbol, 0)?;
    let mut params = Vec::new();
    let mut identities = Vec::new();
    let mut defaults = Vec::new();
    for (ordinal, parameter) in function.regular_parameters.iter().enumerate() {
        let index = u32::try_from(ordinal + 1).expect("member parameters fit u32");
        bind_slot(&mut values, &parameter.base.symbol, index)?;
        params.push(semantic_type_in(arena, types, parameter.ty, resolve)?);
        identities.push(IrParameterIdentity::source(parameter.name.clone()));
        defaults.push(parameter.default_value);
    }
    Ok(member_header(
        BoundParameters {
            name: function.name.clone(),
            params,
            ret: semantic_type_in(arena, types, function.return_type, resolve)?,
            identities,
            defaults,
            values,
        },
        receiver_ty,
        types.clone(),
    ))
}

fn bind_slot(
    values: &mut HashMap<KlibIrSymbol, ParameterValue>,
    symbol: &KlibIrSymbol,
    index: u32,
) -> Result<(), KlibBodyDeclineReason> {
    if values
        .insert(
            symbol.clone(),
            ParameterValue {
                index,
                stable_read: true,
                erased: None,
            },
        )
        .is_some()
    {
        return Err(mismatch(
            "a parameter repeats the instance receiver or another parameter".to_owned(),
        ));
    }
    Ok(())
}

struct BoundParameters {
    name: String,
    params: Vec<Ty>,
    ret: Ty,
    identities: Vec<IrParameterIdentity>,
    defaults: Vec<Option<KlibIrExprId>>,
    values: HashMap<KlibIrSymbol, ParameterValue>,
}

fn member_header(
    bound: BoundParameters,
    receiver_ty: Ty,
    type_parameters: KlibTypeParameters,
) -> FunctionHeader {
    FunctionHeader {
        name: bound.name,
        params: bound.params,
        ret: bound.ret,
        identities: bound.identities,
        context_count: 0,
        extension_receiver: false,
        dispatch_receiver: Some(receiver_ty),
        receiver_slot: None,
        value_receiver: Some(receiver_ty),
        singleton_extension_unread_receiver: None,
        values: bound.values,
        defaults: bound.defaults,
        type_parameters,
        erasures: HashMap::default(),
        erased_result: None,
    }
}
