//! The parameters a constructor's generic `Signature` is written over
//! (`signature_formatter::constructor_positions` writes it).
//!
//! A primary constructor's parameters are its declared ones, each with its source type, behind the
//! outer instance an inner class takes. An enum constructor and the constructor of an enum entry's
//! subclass take the entry's name and ordinal first.

use super::declaration_types::class_ctor_jvm_tys;
use super::signature_formatter::{ConstructorParameter, JvmSignatureFormatter};
use super::type_descriptor;
use crate::ir::{IrClass, IrCtorParameterProvenance};
use crate::types::Ty;

/// The primary constructor's `Signature`, or `None` when it needs none.
pub(super) fn primary_constructor_signature(
    formatter: &JvmSignatureFormatter<'_>,
    class: &IrClass,
) -> Option<String> {
    formatter.constructor_signature(&primary_constructor_parameters(class))
}

/// An enum's constructor `Signature`: its source parameters behind the name and ordinal.
pub(super) fn enum_constructor_signature(
    formatter: &JvmSignatureFormatter<'_>,
    class: &IrClass,
) -> Option<String> {
    let parameters = enum_prefix()
        .chain(primary_constructor_parameters(class))
        .collect::<Vec<_>>();
    formatter.constructor_signature(&parameters)
}

/// The `Signature` of an enum entry subclass's constructor over the entry's selected parameters.
pub(super) fn enum_entry_constructor_signature(
    formatter: &JvmSignatureFormatter<'_>,
    parameters: &[Ty],
    descriptors: &[Ty],
) -> Option<String> {
    assert_eq!(
        parameters.len(),
        descriptors.len(),
        "an enum entry subclass's selected parameters match its JVM parameters"
    );
    let parameters = enum_prefix()
        .chain(
            parameters
                .iter()
                .zip(descriptors)
                .map(|(&ty, &descriptor)| ConstructorParameter::Regular {
                    ty,
                    descriptor: type_descriptor(descriptor),
                }),
        )
        .collect::<Vec<_>>();
    formatter.constructor_signature(&parameters)
}

fn enum_prefix() -> impl Iterator<Item = ConstructorParameter> {
    [
        ConstructorParameter::EnumSynthetic,
        ConstructorParameter::EnumSynthetic,
    ]
    .into_iter()
}

fn primary_constructor_parameters(class: &IrClass) -> Vec<ConstructorParameter> {
    let descriptors = class_ctor_jvm_tys(class);
    assert_eq!(
        class.ctor_args.len(),
        descriptors.len(),
        "{}: a primary constructor's recorded parameters match its JVM parameters",
        class.fq_name()
    );
    class
        .ctor_args
        .iter()
        .zip(descriptors)
        .map(|(argument, descriptor)| {
            if argument.provenance == IrCtorParameterProvenance::EnclosingInstance {
                let Ty::Obj(outer, _) = argument.ty.non_null() else {
                    panic!("an inner class's outer instance is a classifier type");
                };
                return ConstructorParameter::OuterInstance(outer);
            }
            ConstructorParameter::Regular {
                ty: argument.declared_ty.unwrap_or(argument.ty),
                descriptor: type_descriptor(descriptor),
            }
        })
        .collect()
}
