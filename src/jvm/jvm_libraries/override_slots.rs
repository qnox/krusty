//! JVM publication of provider-normalized override slots.
//!
//! Erasure, mapped classifier names, and value-class exclusions stay at this provider boundary.
//! Common hierarchy code consumes only [`ProviderOverrideSlot`] identity and ownership.

use super::JvmLibraries;
use crate::libraries::{FunctionInfo, ProviderOverrideSlot, SemanticPlatform};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

#[derive(Clone, Debug, PartialEq, Eq)]
enum JvmOverrideSlotInput {
    Type(Ty),
    Function(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct JvmOverrideKey {
    effective_name: String,
    inputs: Box<[JvmOverrideSlotInput]>,
}

fn normalized_input(libraries: &JvmLibraries, ty: Ty) -> JvmOverrideSlotInput {
    fn erased_ty(input: JvmOverrideSlotInput) -> Ty {
        match input {
            JvmOverrideSlotInput::Type(ty) => ty,
            JvmOverrideSlotInput::Function(arity) => Ty::obj(&format!("kotlin/Function{arity}")),
        }
    }

    let ty = libraries.library_value_form(ty);
    let erased = match ty {
        unsigned if unsigned.is_unsigned() => unsigned.scalar_value_repr().unwrap(),
        Ty::String => Ty::obj("kotlin/String"),
        Ty::Obj(name, arguments) if name == crate::types::wk::array() => {
            let element = arguments
                .first()
                .copied()
                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
            Ty::obj_args(
                "kotlin/Array",
                &[erased_ty(normalized_input(libraries, element))],
            )
        }
        Ty::Obj(name, _) => Ty::Obj(name, &[]),
        Ty::Null | Ty::Nothing | Ty::Error => Ty::obj("kotlin/Any"),
        Ty::Fun(signature) => {
            return JvmOverrideSlotInput::Function(
                signature.params.len() + usize::from(signature.suspend),
            );
        }
        Ty::Nullable(inner) => match *inner {
            Ty::UInt => Ty::obj("kotlin/UInt"),
            Ty::ULong => Ty::obj("kotlin/ULong"),
            other if other.boxed_ref().is_some() => erased_ty(normalized_input(
                libraries,
                other.boxed_ref().unwrap_or(other),
            )),
            other => erased_ty(normalized_input(libraries, other)),
        },
        Ty::PlatformNullable(inner) => {
            return normalized_input(libraries, Ty::nullable(*inner));
        }
        Ty::TyParam(_, bound) | Ty::DefinitelyNotNull(bound) => {
            return normalized_input(libraries, *bound);
        }
        other => other,
    };
    JvmOverrideSlotInput::Type(erased)
}

pub(super) fn slot(
    libraries: &JvmLibraries,
    owner: TypeName,
    effective_name: String,
    inputs: impl IntoIterator<Item = Ty>,
) -> ProviderOverrideSlot {
    ProviderOverrideSlot::new(
        owner,
        JvmOverrideKey {
            effective_name,
            inputs: inputs
                .into_iter()
                .map(|input| normalized_input(libraries, input))
                .collect(),
        },
    )
}

pub(super) fn source_slot(
    libraries: &JvmLibraries,
    source: &dyn SymbolSource,
    function: &FunctionInfo,
) -> Option<ProviderOverrideSlot> {
    function.stable_declaration?;
    let inputs = function
        .is_extension()
        .then(|| function.semantic_receiver())
        .flatten()
        .into_iter()
        .chain(function.semantic_params().iter().copied())
        .collect::<Vec<_>>();
    if inputs.iter().copied().any(|input| {
        let input = input.non_null();
        input.is_unsigned()
            || input.kotlin_class_internal().is_some_and(|classifier| {
                source
                    .classifier(classifier)
                    .is_some_and(|classifier| classifier.value_underlying.is_some())
            })
    }) {
        return None;
    }
    Some(slot(
        libraries,
        crate::jvm::jvm_class_map::to_jvm_type_name(function.callable.owner),
        function.callable.name.clone(),
        inputs,
    ))
}
