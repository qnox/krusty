//! JVM emission of the builtin members an abstract class materializes.
//!
//! Each member is an `ACC_PUBLIC|ACC_ABSTRACT` method under its Kotlin JVM signature, with its
//! generic signature and no annotations, followed by its special bridge. A renamed member's bridge
//! is `ACC_PUBLIC|ACC_FINAL|ACC_BRIDGE` under the JVM name with the member's own descriptor and
//! signature (`final int size()`, `final Set<String> keySet()`), then, where the declaration's
//! erased descriptor differs, an ordinary synthetic bridge there that calls the member as well
//! (`Object remove(int)`). A type-safe member's bridge is the final special bridge at the erased
//! descriptor that tests each checked argument, as two methods when the member's reference result
//! is narrower (`Object get(Object)`, then `Integer get(Object)`). Every bridge maps to the class
//! declaration's line and names its locals after the builtin declaration.

use super::super::{method_descriptor, ClassWriter, EmitEnv, JvmSignatureFormatter};
use super::read_only_stubs::{emit_forwarding, Target, ThrowingMethod};
use crate::ir::{IrClass, IrFile};
use crate::jvm::abstract_builtin_overrides::{AbstractBuiltinOverride, SpecialBridge};

const ACC_PUBLIC: u16 = 0x0001;
const ACC_ABSTRACT: u16 = 0x0400;

pub(super) fn emit(ir: &IrFile, class: &IrClass, cw: &mut ClassWriter, env: &EmitEnv<'_>) {
    let formatter = JvmSignatureFormatter::new(ir, env);
    for member in env
        .collection_members
        .abstract_overrides
        .for_class(class.fq_name_id())
    {
        emit_member(class, cw, &formatter, member);
    }
}

fn emit_member(
    class: &IrClass,
    cw: &mut ClassWriter,
    formatter: &JvmSignatureFormatter<'_>,
    member: &AbstractBuiltinOverride,
) {
    let parameters = member.physical_parameters();
    let result = member.physical_result();
    let descriptor = method_descriptor(&parameters, result);
    let signature = formatter
        .method_positions(&member.parameters, &member.result)
        .filter(|signature| *signature != descriptor);
    cw.add_abstract_method_sig(
        ACC_PUBLIC | ACC_ABSTRACT,
        &member.name,
        &descriptor,
        signature.as_deref(),
    );
    let target = Target {
        name: &member.name,
        parameters: &parameters,
        result,
    };
    match &member.bridge {
        SpecialBridge::Renamed { jvm_name, erased } => {
            let special = ThrowingMethod {
                name: jvm_name,
                parameters: &parameters,
                parameter_names: &member.parameter_names,
                descriptor: &descriptor,
                signature: signature.as_deref(),
            };
            emit_forwarding::<{ 0x0001 | 0x0010 | 0x0040 }>(
                class, cw, &target, &special, result, None,
            );
            if let Some((erased_parameters, erased_result)) = erased {
                let erased_descriptor = method_descriptor(erased_parameters, *erased_result);
                let bridge = ThrowingMethod {
                    name: jvm_name,
                    parameters: erased_parameters,
                    parameter_names: &member.parameter_names,
                    descriptor: &erased_descriptor,
                    signature: None,
                };
                emit_forwarding::<{ 0x0001 | 0x0040 | 0x1000 }>(
                    class,
                    cw,
                    &target,
                    &bridge,
                    *erased_result,
                    None,
                );
            }
        }
        SpecialBridge::TypeSafe {
            barrier,
            erased: (erased_parameters, erased_result),
        } => {
            let erased_descriptor = method_descriptor(erased_parameters, *erased_result);
            let bridge = ThrowingMethod {
                name: &member.name,
                parameters: erased_parameters,
                parameter_names: &member.parameter_names,
                descriptor: &erased_descriptor,
                signature: None,
            };
            let checked = Some((*barrier, member.parameters.as_slice()));
            let narrower =
                result != *erased_result && result.is_reference() && erased_result.is_reference();
            if !narrower {
                emit_forwarding::<{ 0x0001 | 0x0010 | 0x0040 }>(
                    class,
                    cw,
                    &target,
                    &bridge,
                    *erased_result,
                    checked,
                );
                return;
            }
            emit_forwarding::<{ 0x0001 | 0x0010 | 0x0040 | 0x1000 }>(
                class,
                cw,
                &target,
                &bridge,
                *erased_result,
                checked,
            );
            let narrowed = method_descriptor(erased_parameters, result);
            let special = ThrowingMethod {
                descriptor: &narrowed,
                ..bridge
            };
            emit_forwarding::<{ 0x0001 | 0x0010 | 0x0040 }>(
                class, cw, &target, &special, result, checked,
            );
        }
    }
}
