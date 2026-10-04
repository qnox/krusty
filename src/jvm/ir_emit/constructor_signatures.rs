//! The parameters a constructor's generic `Signature` is written over
//! (`signature_formatter::constructor_positions` writes it).
//!
//! A primary constructor's parameters are its declared ones, each with its source type, behind the
//! outer instance an inner class takes. An enum constructor and the constructor of an enum entry's
//! subclass take the entry's name and ordinal first.

use super::declaration_types::class_ctor_jvm_tys;
use super::signature_formatter::{ConstructorParameter, JvmSignatureFormatter};
use super::type_descriptor;
use crate::ir::{IrClass, IrCtorParameterProvenance, IrFile};
use crate::types::Ty;

/// The primary constructor's `Signature`, or `None` when it needs none.
pub(super) fn primary_constructor_signature(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    class: &IrClass,
) -> Option<String> {
    formatter.constructor_signature(&primary_constructor_parameters(ir, class))
}

/// An enum's constructor `Signature`: its source parameters behind the name and ordinal.
pub(super) fn enum_constructor_signature(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    class: &IrClass,
) -> Option<String> {
    let parameters = enum_prefix()
        .chain(primary_constructor_parameters(ir, class))
        .collect::<Vec<_>>();
    formatter.constructor_signature(&parameters)
}

/// The `Signature` of an enum entry subclass's constructor over the entry's selected parameters.
pub(super) fn enum_entry_constructor_signature(
    formatter: &JvmSignatureFormatter<'_>,
    parameters: &[Ty],
    descriptors: &[Ty],
) -> Option<String> {
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

fn primary_constructor_parameters(ir: &IrFile, class: &IrClass) -> Vec<ConstructorParameter> {
    let field_type_parameters = ir.field_signatures(&class.fq_name());
    let mut field_index = 0usize;
    class_ctor_jvm_tys(class)
        .into_iter()
        .enumerate()
        .map(|(index, descriptor)| {
            let argument = class.ctor_args.get(index);
            if let Some(argument) = argument.filter(|argument| {
                argument.provenance == IrCtorParameterProvenance::EnclosingInstance
            }) {
                let Ty::Obj(outer, _) = argument.ty.non_null() else {
                    panic!("an inner class's outer instance is a classifier type");
                };
                return ConstructorParameter::OuterInstance(outer);
            }
            // A constructor without recorded arguments takes its leading fields.
            let stored = argument.is_none_or(|argument| argument.is_field);
            let field = stored.then(|| class.fields.get(field_index)).flatten();
            if stored {
                field_index += 1;
            }
            let ty = match argument.and_then(|argument| argument.declared_ty) {
                Some(declared) => declared,
                None => match field {
                    Some(field) => field_type_parameters
                        .and_then(|parameters| {
                            parameters.iter().find(|(name, _)| *name == field.name)
                        })
                        .map(|(_, parameter)| {
                            Ty::ty_param(parameter, Ty::nullable(Ty::obj("kotlin/Any")))
                        })
                        .unwrap_or(field.ty),
                    None => descriptor,
                },
            };
            ConstructorParameter::Regular {
                ty,
                descriptor: type_descriptor(descriptor),
            }
        })
        .collect()
}
