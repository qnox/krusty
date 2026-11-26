//! The rich form of a function reference serialized by a producer older than Kotlin 2.4.
//!
//! Kotlin 2.4 serializes a function reference as an `IrRichFunctionReference`: its bound values,
//! the reflected declaration, the function-type member it implements, and an `invoke` function
//! that forwards its parameters to the referenced declaration. Older producers wrote a plain
//! `IrFunctionReference` instead, and kotlinc's `UpgradeCallableReferences` turns that form into
//! the rich one when it reads such a library. The decoder does the same, so every consumer of a
//! decoded body meets one reference form whichever compiler produced the library.
//!
//! Only a plain reference is upgraded: one without an adaptation origin, whose type is a
//! non-suspend reflective function type (`KFunctionN`) with every type argument given, and which
//! binds at most one receiver and no regular argument. Any other reference keeps its serialized
//! form, which the consumers decline as before.

use crate::libraries::function_classifiers;

use super::semantic_types::classifier_name;
use super::symbols::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use super::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrDeclarationBase, KlibIrExpr, KlibIrExprId,
    KlibIrExprKind, KlibIrFunction, KlibIrFunctionId, KlibIrMemberAccess, KlibIrNullability,
    KlibIrParameter, KlibIrStatement, KlibIrType, KlibIrTypeArgument, KlibIrTypeId,
};

/// The origin kotlinc gives the `invoke` function of a rich reference.
const INVOKE_ORIGIN: &str = "LOCAL_FUNCTION_FOR_LAMBDA";

/// The rich form of the legacy reference `access` of type `ty`, or `None` when the reference is
/// not a plain one. `local` allocates a fresh file-local identity for each synthesized
/// declaration.
pub(super) fn upgrade(
    arena: &mut KlibIrArena,
    ty: Option<KlibIrTypeId>,
    access: &KlibIrMemberAccess,
    reflection_target: Option<&KlibIrSymbol>,
    local: &mut impl FnMut() -> KlibIrSignature,
) -> Option<KlibIrExprKind> {
    if access.origin.is_some() || access.symbol.kind != KlibIrSymbolKind::Function {
        return None;
    }
    let (overridden, parameter_types, result) = reference_type(arena, ty?)?;
    let bound = bound_values(&access.arguments)?;
    let mut parameters = Vec::with_capacity(bound.len() + parameter_types.len());
    for value in &bound {
        parameters.push((arena.expr(*value).ty?, "receiver".to_owned()));
    }
    parameters.extend(
        parameter_types
            .into_iter()
            .enumerate()
            .map(|(index, ty)| (ty, format!("p{index}"))),
    );
    let invoke = invoke_function(arena, access, parameters, result, local);
    Some(KlibIrExprKind::RichFunctionReference {
        bound_values: bound,
        reflection_target: Some(reflection_target.unwrap_or(&access.symbol).clone()),
        overridden,
        invoke,
        flags: 0,
        origin: None,
    })
}

/// The `invoke` member the reference type implements, its invocation parameter types and its
/// result type, for a non-null, non-suspend `KFunctionN` with every type argument given.
fn reference_type(
    arena: &KlibIrArena,
    ty: KlibIrTypeId,
) -> Option<(KlibIrSymbol, Vec<KlibIrTypeId>, KlibIrTypeId)> {
    let KlibIrType::Simple {
        classifier,
        nullability,
        arguments,
        ..
    } = arena.ty(ty)
    else {
        return None;
    };
    let KlibIrSignature::Public(class) = &classifier.signature else {
        return None;
    };
    if classifier.kind != KlibIrSymbolKind::Class
        || *nullability == KlibIrNullability::MarkedNullable
    {
        return None;
    }
    let family = function_classifiers::classifier(classifier_name(class).ok()?)?;
    if !family.is_reflective() || family.is_suspend() || arguments.len() != family.arity() + 1 {
        return None;
    }
    let mut types = arguments
        .iter()
        .map(|argument| match argument {
            KlibIrTypeArgument::Type { ty, .. } => Some(*ty),
            KlibIrTypeArgument::Star => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let result = types.pop()?;
    let operands = (0..family.arity())
        .map(|index| format!("1:{index}"))
        .collect::<Vec<_>>()
        .join(";");
    let overridden = KlibIrSymbol {
        kind: KlibIrSymbolKind::Function,
        signature: KlibIrSignature::Public(
            class.member_function("invoke", &format!("invoke({operands}){{}}")),
        ),
    };
    Some((overridden, types, result))
}

/// The receiver a legacy reference binds, if any. A reference that binds two receivers or a
/// regular argument has no plain rich form.
fn bound_values(arguments: &KlibIrArguments) -> Option<Vec<KlibIrExprId>> {
    match arguments {
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => {
            if values.iter().any(Option::is_some) {
                return None;
            }
            match (dispatch_receiver, extension_receiver) {
                (Some(_), Some(_)) => None,
                (receiver, None) | (None, receiver) => Some(receiver.iter().copied().collect()),
            }
        }
        KlibIrArguments::Flat(values) => {
            let bound = values.iter().take_while(|value| value.is_some()).count();
            if bound > 1 || values[bound..].iter().any(Option::is_some) {
                return None;
            }
            Some(values.iter().flatten().copied().collect())
        }
    }
}

/// `fun <name>(parameters): result = target(parameters)`: the function the rich reference
/// invokes, forwarding every parameter in order (in Kotlin 2.4's one-argument-per-parameter
/// layout, which places a receiver first).
fn invoke_function(
    arena: &mut KlibIrArena,
    access: &KlibIrMemberAccess,
    parameters: Vec<(KlibIrTypeId, String)>,
    result: KlibIrTypeId,
    local: &mut impl FnMut() -> KlibIrSignature,
) -> KlibIrFunctionId {
    let symbol = KlibIrSymbol {
        kind: KlibIrSymbolKind::Function,
        signature: local(),
    };
    let parameters = parameters
        .into_iter()
        .map(|(ty, name)| KlibIrParameter {
            base: base(
                KlibIrSymbol {
                    kind: KlibIrSymbolKind::ValueParameter,
                    signature: local(),
                },
                "DEFINED",
            ),
            name,
            ty,
            vararg_element_type: None,
            default_value: None,
        })
        .collect::<Vec<_>>();
    let forwarded = parameters
        .iter()
        .map(|parameter| {
            Some(push(
                arena,
                Some(parameter.ty),
                KlibIrExprKind::GetValue {
                    symbol: parameter.base.symbol.clone(),
                    origin: None,
                },
            ))
        })
        .collect();
    let call = push(
        arena,
        Some(result),
        KlibIrExprKind::Call {
            access: KlibIrMemberAccess {
                symbol: access.symbol.clone(),
                arguments: KlibIrArguments::Flat(forwarded),
                type_arguments: access.type_arguments.clone(),
                origin: None,
            },
            super_qualifier: None,
        },
    );
    let nothing = nothing_type(arena);
    let returned = push(
        arena,
        Some(nothing),
        KlibIrExprKind::Return {
            target: symbol.clone(),
            value: call,
        },
    );
    let name = match &access.symbol.signature {
        KlibIrSignature::Public(signature) => signature.declaration().segments().last().cloned(),
        _ => None,
    };
    let id = KlibIrFunctionId(u32::try_from(arena.functions.len()).expect("arena fits u32"));
    arena.functions.push(KlibIrFunction {
        base: base(symbol, INVOKE_ORIGIN),
        name: name.unwrap_or_else(|| "<anonymous>".to_owned()),
        constructor: false,
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: parameters,
        return_type: result,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: Some(KlibIrBody::Block(vec![KlibIrStatement::Expression(
            returned,
        )])),
    });
    id
}

fn base(symbol: KlibIrSymbol, origin: &str) -> KlibIrDeclarationBase {
    KlibIrDeclarationBase {
        symbol,
        origin: origin.to_owned(),
        flags: 0,
        annotations: Vec::new(),
    }
}

fn push(arena: &mut KlibIrArena, ty: Option<KlibIrTypeId>, kind: KlibIrExprKind) -> KlibIrExprId {
    let id = KlibIrExprId(u32::try_from(arena.exprs.len()).expect("arena fits u32"));
    arena.exprs.push(KlibIrExpr { ty, kind });
    id
}

/// `kotlin.Nothing`, the type of a `return`.
fn nothing_type(arena: &mut KlibIrArena) -> KlibIrTypeId {
    let id = KlibIrTypeId(u32::try_from(arena.types.len()).expect("arena fits u32"));
    arena.types.push(KlibIrType::Simple {
        classifier: KlibIrSymbol {
            kind: KlibIrSymbolKind::Class,
            signature: KlibIrSignature::Public(
                crate::metadata::id_signature::KlibPublicIdSignature::classifier(
                    crate::types::KOTLIN_NOTHING,
                ),
            ),
        },
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    id
}

#[cfg(test)]
#[path = "reference_upgrade/tests.rs"]
mod tests;
