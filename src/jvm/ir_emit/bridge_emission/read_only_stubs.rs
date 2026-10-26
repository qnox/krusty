//! JVM emission of read-only collection stubs and their bridges.
//!
//! A stub is `public`, throws `UnsupportedOperationException` with kotlinc's fixed message, and
//! carries no line numbers: kotlinc builds it without a source offset. Its bridge to the mutable
//! declaration's erased descriptor is `ACC_PUBLIC|ACC_BRIDGE|ACC_SYNTHETIC` with the same body,
//! except that a type-safe stub's bridge (`Object remove(Object)`) calls the stub.
//! A member the class already implements gets no stub, only that bridge, which forwards to the
//! implementation like any bridge and maps to the class declaration's line. A builtin with a
//! different JVM name (`removeAt`) is realized under that name, and its erased bridge directly
//! follows the stub; an implementation of it gets kotlinc's final special bridge under the JVM
//! name, `ACC_PUBLIC|ACC_FINAL|ACC_BRIDGE`, and the erased bridge forwards to that.

use super::super::{
    box_prim_free, discard, emit_return, finish_code_sig, jvm_tys, load, local_variable_desc,
    method_descriptor, slot_words, unbox_prim_from, ClassWriter, CodeBuilder, EmitEnv,
    JvmSignatureFormatter,
};
use crate::ir::{IrClass, IrFile};
use crate::jvm::read_only_collection_stubs::{ReadOnlyStub, StubBody};
use crate::types::Ty;

const MESSAGE: &str = "Operation is not supported for read-only collection";
const EXCEPTION: &str = "java/lang/UnsupportedOperationException";

/// The stubs follow the class's declared members, in the mutable declarations' member order.
pub(super) fn emit_stubs(ir: &IrFile, class: &IrClass, cw: &mut ClassWriter, env: &EmitEnv<'_>) {
    let formatter = JvmSignatureFormatter::new(ir, env);
    let stubs = env
        .collection_members
        .read_only_stubs
        .for_class(class.fq_name_id());
    for stub in stubs.iter().filter(|stub| stub.body == StubBody::Throwing) {
        let parameters = jvm_tys(&stub.parameters);
        let descriptor = method_descriptor(&parameters, stub.physical_result());
        let signature_result = stub.signature_result();
        let signature = if stub.declaration_site_wildcards {
            formatter.method_positions_with_declaration_site_wildcards(
                &stub.parameters,
                &signature_result,
            )
        } else {
            formatter.method_positions(&stub.parameters, &signature_result)
        }
        .filter(|signature| *signature != descriptor);
        let method = ThrowingMethod {
            name: &stub.physical_name,
            parameters: &parameters,
            parameter_names: &stub.parameter_names,
            descriptor: &descriptor,
            signature: signature.as_deref(),
        };
        emit_throwing::<0x0001>(class, cw, &method);
        if stub.is_renamed() && stub.bridge.is_some() {
            emit_stub_bridge(class, cw, stub);
        }
    }
}

/// The read-only bridges of one class, merged into its ordinary bridges in kotlinc's member order:
/// the bridge of a member the class declares sits with the ordinary bridges by that member's source
/// order, the bridge of an inherited member follows them, and the stubs' throwing bridges come last
/// because kotlinc appends the stubs after every other member.
pub(super) struct StubBridges<'a> {
    pending: Vec<&'a ReadOnlyStub>,
}

impl<'a> StubBridges<'a> {
    pub(super) fn of(class: &IrClass, env: &'a EmitEnv<'_>) -> Self {
        let mut pending: Vec<&ReadOnlyStub> = env
            .collection_members
            .read_only_stubs
            .for_class(class.fq_name_id())
            .iter()
            .filter(|stub| match stub.body {
                // A renamed stub's bridge directly follows it.
                StubBody::Throwing => stub.bridge.is_some() && !stub.is_renamed(),
                StubBody::Implemented { .. } => stub.bridge.is_some() || stub.is_renamed(),
            })
            .collect();
        // Stable: equal keys keep the candidates' order.
        pending.sort_by_key(|stub| match &stub.body {
            StubBody::Implemented { order, .. } => (0, *order),
            StubBody::Throwing => (1, 0),
        });
        Self { pending }
    }

    /// Emit the bridges of the class's own members declared before the member whose ordinary
    /// bridge, at source order `order`, comes next.
    pub(super) fn emit_before(&mut self, order: u32, class: &IrClass, cw: &mut ClassWriter) {
        let ready = self
            .pending
            .iter()
            .take_while(
                |stub| matches!(&stub.body, StubBody::Implemented { order: own, .. } if *own < order),
            )
            .count();
        for stub in self.pending.drain(..ready) {
            emit_stub_bridge(class, cw, stub);
        }
    }

    /// Emit every bridge not yet emitted.
    pub(super) fn emit_rest(self, class: &IrClass, cw: &mut ClassWriter) {
        for stub in self.pending {
            emit_stub_bridge(class, cw, stub);
        }
    }
}

fn emit_stub_bridge(class: &IrClass, cw: &mut ClassWriter, stub: &ReadOnlyStub) {
    if let StubBody::Implemented {
        target,
        target_result,
        ..
    } = &stub.body
    {
        let mut target = Target {
            name: &stub.name,
            parameters: target,
            result: *target_result,
        };
        if stub.is_renamed() {
            // kotlinc's special bridge: the implementation under the builtin's JVM name.
            let descriptor = method_descriptor(target.parameters, target.result);
            if !cw.has_method(&stub.physical_name, &descriptor) {
                let special = ThrowingMethod {
                    name: &stub.physical_name,
                    parameters: target.parameters,
                    parameter_names: &stub.parameter_names,
                    descriptor: &descriptor,
                    signature: None,
                };
                emit_forwarding::<{ 0x0001 | 0x0010 | 0x0040 }>(
                    class,
                    cw,
                    &target,
                    &special,
                    target.result,
                );
            }
            target.name = &stub.physical_name;
        }
        let Some((parameters, result)) = stub.bridge.as_ref() else {
            return;
        };
        let descriptor = method_descriptor(parameters, *result);
        if cw.has_method(&stub.physical_name, &descriptor) {
            return;
        }
        let method = ThrowingMethod {
            name: &stub.physical_name,
            parameters,
            parameter_names: &stub.parameter_names,
            descriptor: &descriptor,
            signature: None,
        };
        emit_forwarding::<{ 0x0001 | 0x0040 | 0x1000 }>(class, cw, &target, &method, *result);
        return;
    }
    let (parameters, result) = stub
        .bridge
        .as_ref()
        .expect("only a stub with a bridge descriptor is pending");
    let descriptor = method_descriptor(parameters, *result);
    if cw.has_method(&stub.physical_name, &descriptor) {
        return;
    }
    let method = ThrowingMethod {
        name: &stub.physical_name,
        parameters,
        parameter_names: &stub.parameter_names,
        descriptor: &descriptor,
        signature: None,
    };
    if stub.type_safe {
        // A type-safe stub already takes the erased parameters: its bridge calls it.
        let stub_parameters = jvm_tys(&stub.parameters);
        let target = Target {
            name: &stub.physical_name,
            parameters: &stub_parameters,
            result: stub.physical_result(),
        };
        emit_forwarding::<{ 0x0001 | 0x0040 | 0x1000 }>(class, cw, &target, &method, *result);
        return;
    }
    emit_throwing::<{ 0x0001 | 0x0040 | 0x1000 }>(class, cw, &method);
}

/// The member a bridge forwards to, by its JVM name and physical descriptor.
struct Target<'a> {
    name: &'a str,
    parameters: &'a [Ty],
    result: Ty,
}

/// The bridge of an implemented member: each erased argument is cast or unboxed to the
/// implementation's physical parameter, the class's member at that descriptor is called, and its
/// result is discarded for a `void` bridge (a `Nothing` member's `Void`) or boxed back to the
/// erased result when that is a reference.
fn emit_forwarding<const ACCESS: u16>(
    class: &IrClass,
    cw: &mut ClassWriter,
    target: &Target<'_>,
    bridge: &ThrowingMethod,
    erased_result: Ty,
) {
    let targets = target.parameters;
    cw.reserve_method_pool(bridge.name, bridge.descriptor, None, &[]);
    let words: u16 = bridge.parameters.iter().map(|ty| slot_words(*ty)).sum();
    let mut code = CodeBuilder::new(1 + words);
    code.aload(0);
    let mut slot = 1u16;
    for (&erased, &target) in bridge.parameters.iter().zip(targets) {
        load(erased, slot, &mut code);
        slot += slot_words(erased);
        if erased == target {
            continue;
        }
        assert!(
            erased.is_reference(),
            "a read-only collection bridge receives erased reference arguments, got {erased:?}"
        );
        if target.is_reference() {
            let cast = cw.class_ref(&crate::jvm::names::instanceof_internal_name(target));
            code.checkcast(cast);
        } else {
            unbox_prim_from(cw, &mut code, erased, target);
        }
    }
    let result = target.result;
    let owner = class.fq_name();
    let method = cw.methodref(&owner, target.name, &method_descriptor(targets, result));
    let arguments: i32 = targets.iter().map(|ty| i32::from(slot_words(*ty))).sum();
    code.invokevirtual(method, arguments, i32::from(slot_words(result)));
    if erased_result == Ty::Unit {
        if result != Ty::Unit {
            discard(result, &mut code);
        }
    } else if result != erased_result {
        assert!(
            erased_result.is_reference(),
            "a read-only collection bridge returns an erased reference, got {erased_result:?}"
        );
        if result.is_jvm_scalar() {
            box_prim_free(cw, &mut code, result);
        }
    }
    emit_return(erased_result, &mut code);
    finish_code_sig::<ACCESS>(
        cw,
        bridge.name,
        bridge.descriptor,
        &mut code,
        1 + words,
        None,
    );
    let line = if class.decl_start_line == 0 {
        class.decl_line
    } else {
        class.decl_start_line
    };
    let lines = (line != 0).then_some((0, line));
    cw.set_method_debug(
        bridge.name,
        bridge.descriptor,
        lines,
        &method_locals(class, bridge),
    );
}

fn method_locals(class: &IrClass, method: &ThrowingMethod) -> Vec<(String, String, u16)> {
    let mut locals = vec![(String::from("this"), format!("L{};", class.fq_name()), 0)];
    let mut slot = 1u16;
    for (parameter, name) in method.parameters.iter().zip(method.parameter_names) {
        locals.push((name.clone(), local_variable_desc(*parameter), slot));
        slot += slot_words(*parameter);
    }
    locals
}

struct ThrowingMethod<'a> {
    name: &'a str,
    parameters: &'a [Ty],
    parameter_names: &'a [String],
    descriptor: &'a str,
    signature: Option<&'a str>,
}

fn emit_throwing<const ACCESS: u16>(
    class: &IrClass,
    cw: &mut ClassWriter,
    method: &ThrowingMethod,
) {
    assert_eq!(
        method.parameters.len(),
        method.parameter_names.len(),
        "a read-only collection stub names every parameter"
    );
    // ASM visits the method header before its body, so the header's pool entries come first.
    cw.reserve_method_pool(method.name, method.descriptor, method.signature, &[]);
    let words: u16 = method.parameters.iter().map(|ty| slot_words(*ty)).sum();
    let mut code = CodeBuilder::new(1 + words);
    let exception = cw.class_ref(EXCEPTION);
    code.new_obj(exception);
    code.dup();
    code.push_string(MESSAGE, cw);
    let constructor = cw.methodref(EXCEPTION, "<init>", "(Ljava/lang/String;)V");
    code.invokespecial(constructor, 1, 0);
    code.athrow();
    finish_code_sig::<ACCESS>(
        cw,
        method.name,
        method.descriptor,
        &mut code,
        1 + words,
        method.signature,
    );
    cw.set_method_debug(
        method.name,
        method.descriptor,
        None,
        &method_locals(class, method),
    );
}
