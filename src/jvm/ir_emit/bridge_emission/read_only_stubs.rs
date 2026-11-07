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

use super::super::value_class_adapters::emit_value_class_box_adapter;
use super::super::{
    box_prim_free, discard, emit_return, finish_code_sig, load, local_variable_desc,
    method_descriptor, slot_words, unbox_prim_from, ClassWriter, CodeBuilder, EmitEnv,
    JvmSignatureFormatter,
};
use crate::ir::{IrClass, IrFile};
use crate::jvm::bridge_adaptations::ValueClassAdapter;
use crate::jvm::collection_barriers::BarrierOutcome;
use crate::jvm::read_only_collection_stubs::{ReadOnlyStub, StubBarrier, StubBody};
use crate::types::{wk, Ty};

const MESSAGE: &str = "Operation is not supported for read-only collection";
const EXCEPTION: &str = "java/lang/UnsupportedOperationException";

/// The stubs follow the class's declared members, in the mutable declarations' member order. Each
/// is written once: a value class writes its stubs ahead of its bridges.
pub(super) fn emit_stubs(ir: &IrFile, class: &IrClass, cw: &mut ClassWriter, env: &EmitEnv<'_>) {
    let formatter = JvmSignatureFormatter::new(ir, env);
    let stubs = env
        .collection_members
        .read_only_stubs
        .for_class(class.fq_name_id());
    for stub in stubs.iter().filter(|stub| stub.body == StubBody::Throwing) {
        let parameters = &stub.realized.parameters;
        let descriptor = method_descriptor(parameters, stub.realized.result);
        // A value class's stubs are written before its representation members.
        if cw.has_method(&stub.realized.name, &descriptor) {
            continue;
        }
        let signature_parameters = stub.signature_parameters();
        let signature_result = stub.signature_result();
        let signature = if stub.declaration_site_wildcards {
            formatter.method_positions_with_declaration_site_wildcards(
                &signature_parameters,
                &signature_result,
            )
        } else {
            formatter.method_positions(&signature_parameters, &signature_result)
        }
        .filter(|signature| *signature != descriptor);
        let method = ThrowingMethod {
            name: &stub.realized.name,
            parameters,
            parameter_names: &stub.parameter_names,
            descriptor: &descriptor,
            signature: signature.as_deref(),
        };
        emit_throwing::<0x0001>(class, cw, &method);
        if stub.is_renamed() && stub.bridge.is_some() {
            emit_stub_bridge(class, cw, &formatter, stub);
        }
    }
}

/// The read-only bridges of one class, merged into its ordinary bridges in kotlinc's member order:
/// the bridge of a member the class declares sits with the ordinary bridges by that member's source
/// order, the bridge of an inherited member follows them, and the stubs' throwing bridges come last
/// because kotlinc appends the stubs after every other member.
pub(super) struct StubBridges<'a> {
    pending: Vec<&'a ReadOnlyStub>,
    formatter: JvmSignatureFormatter<'a>,
}

impl<'a> StubBridges<'a> {
    pub(super) fn of(ir: &'a IrFile, class: &IrClass, env: &'a EmitEnv<'_>) -> Self {
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
        Self {
            pending,
            formatter: JvmSignatureFormatter::new(ir, env),
        }
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
            emit_stub_bridge(class, cw, &self.formatter, stub);
        }
    }

    /// Emit every bridge not yet emitted.
    pub(super) fn emit_rest(self, class: &IrClass, cw: &mut ClassWriter) {
        for stub in &self.pending {
            emit_stub_bridge(class, cw, &self.formatter, stub);
        }
    }
}

fn emit_stub_bridge(
    class: &IrClass,
    cw: &mut ClassWriter,
    formatter: &JvmSignatureFormatter<'_>,
    stub: &ReadOnlyStub,
) {
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
            result_box: None,
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
                    None,
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
        let Some(barrier) = stub.barrier else {
            emit_forwarding::<{ 0x0001 | 0x0040 | 0x1000 }>(
                class, cw, &target, &method, *result, None,
            );
            return;
        };
        emit_type_safe_special_bridges(
            class,
            cw,
            &target,
            &method,
            *result,
            (barrier, &stub.override_parameters),
            None,
        );
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
        let target = Target {
            name: &stub.realized.name,
            parameters: &stub.realized.parameters,
            result: stub.realized.result,
            result_box: stub.realized.result_box,
        };
        // A stub renamed by its value-class result leaves the declaration's special bridges to
        // kotlinc's bridge lowering, which checks each argument against the stub's own `Any?`.
        // The final special bridge declares the stub's semantic result (`Z<Int>?`).
        if let Some(barrier) = stub
            .barrier
            .filter(|_| stub.realized.name != stub.physical_name)
        {
            let narrowed = method_descriptor(parameters, target.result);
            let signature = formatter
                .method_positions(&stub.parameters, &stub.signature_result())
                .filter(|signature| *signature != narrowed);
            emit_type_safe_special_bridges(
                class,
                cw,
                &target,
                &method,
                *result,
                (barrier, &stub.parameters),
                signature.as_deref(),
            );
            return;
        }
        emit_forwarding::<{ 0x0001 | 0x0040 | 0x1000 }>(class, cw, &target, &method, *result, None);
        return;
    }
    emit_throwing::<{ 0x0001 | 0x0040 | 0x1000 }>(class, cw, &method);
}

/// kotlinc's special bridge of a type-safe declaration is final, not synthetic, and tests each
/// checked argument against the target's declared parameter before the call. When the target's
/// reference result is narrower than the erased one, it is two methods: a final synthetic copy at
/// the erased descriptor, then the final bridge at the target's result (`Object remove(Object)`,
/// then `Integer remove(Object)`), the latter with its generic `signature`, if any.
fn emit_type_safe_special_bridges(
    class: &IrClass,
    cw: &mut ClassWriter,
    target: &Target<'_>,
    method: &ThrowingMethod<'_>,
    erased_result: Ty,
    (barrier, declared): (StubBarrier, &[Ty]),
    signature: Option<&str>,
) {
    let checked = Some((barrier, declared));
    let narrower = target.result != erased_result
        && target.result.is_reference()
        && erased_result.is_reference();
    if !narrower {
        emit_forwarding::<{ 0x0001 | 0x0010 | 0x0040 }>(
            class,
            cw,
            target,
            method,
            erased_result,
            checked,
        );
        return;
    }
    emit_forwarding::<{ 0x0001 | 0x0010 | 0x0040 | 0x1000 }>(
        class,
        cw,
        target,
        method,
        erased_result,
        checked,
    );
    let narrowed = method_descriptor(method.parameters, target.result);
    if !cw.has_method(method.name, &narrowed) {
        let special = ThrowingMethod {
            descriptor: &narrowed,
            signature,
            ..*method
        };
        emit_forwarding::<{ 0x0001 | 0x0010 | 0x0040 }>(
            class,
            cw,
            target,
            &special,
            target.result,
            checked,
        );
    }
}

/// The member a bridge forwards to, by its JVM name and physical descriptor, and the value class
/// whose carrier it returns where an erased reference result expects the box.
pub(super) struct Target<'a> {
    pub(super) name: &'a str,
    pub(super) parameters: &'a [Ty],
    pub(super) result: Ty,
    pub(super) result_box: Option<ValueClassAdapter>,
}

/// The bridge of an implemented member: each erased argument is cast or unboxed to the
/// implementation's physical parameter, the class's member at that descriptor is called, and its
/// result is discarded for a `void` bridge (a `Nothing` member's `Void`), boxed back to the
/// erased result when that is a reference, or unboxed when the bridge returns the scalar the
/// member boxed. A type-safe declaration's bridge first tests each
/// checked argument against its declared type.
pub(super) fn emit_forwarding<const ACCESS: u16>(
    class: &IrClass,
    cw: &mut ClassWriter,
    target: &Target<'_>,
    bridge: &ThrowingMethod,
    erased_result: Ty,
    barrier: Option<(StubBarrier, &[Ty])>,
) {
    let targets = target.parameters;
    cw.reserve_method_pool(bridge.name, bridge.descriptor, bridge.signature, &[]);
    let words: u16 = bridge.parameters.iter().map(|ty| slot_words(*ty)).sum();
    let mut code = CodeBuilder::new(1 + words);
    if let Some((barrier, declared)) = barrier {
        for &index in barrier.checked_parameters {
            let index = usize::from(index);
            let slot = 1 + bridge.parameters[..index]
                .iter()
                .map(|ty| slot_words(*ty))
                .sum::<u16>();
            emit_type_check(
                cw,
                &mut code,
                slot,
                declared[index],
                targets[index],
                barrier.outcome,
            );
        }
    }
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
    } else if result != erased_result && erased_result.is_jvm_scalar() && result.is_reference() {
        // A renamed bridge returns the scalar its abstract method boxed (`int remove(int)` calls
        // `Integer removeAt(int)` and unboxes).
        unbox_prim_from(cw, &mut code, result, erased_result);
    } else if result != erased_result {
        assert!(
            erased_result.is_reference(),
            "a read-only collection bridge returns an erased reference, got {erased_result:?}"
        );
        if let Some(value_class) = target.result_box {
            emit_value_class_box_adapter(
                cw,
                &mut code,
                value_class.owner,
                result,
                value_class.null_preserving,
            );
        } else if result.is_jvm_scalar() {
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
        bridge.signature,
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

/// kotlinc's test of one checked argument at `slot` against the declared type it reaches the
/// implementation as: `instanceof` of its class, which a `null` passes when the type admits it.
/// Every argument is an `Any?`: kotlinc lowers that test to `arg == null || arg != null` and its
/// codegen keeps only the null test, which reaches the call on both of its branches.
fn emit_type_check(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    slot: u16,
    declared: Ty,
    physical: Ty,
    outcome: BarrierOutcome,
) {
    if declared.is_nullable() && declared.non_null().obj_internal() == Some(wk::any()) {
        let next = code.new_label();
        code.aload(slot);
        code.ifnonnull(next);
        code.bind(next);
        return;
    }
    let class = cw.class_ref(&declared.jvm_boxed_ref().map_or_else(
        || crate::jvm::names::instanceof_internal_name(physical),
        crate::jvm::names::instanceof_internal_name,
    ));
    let next = code.new_label();
    code.aload(slot);
    if declared.is_nullable() {
        let present = code.new_label();
        let tested = code.new_label();
        code.dup();
        code.ifnonnull(present);
        code.pop();
        code.push_int(1, cw);
        code.goto(tested);
        code.bind(present);
        code.instance_of(class);
        code.bind(tested);
    } else {
        code.instance_of(class);
    }
    code.ifne(next);
    super::emit_barrier_outcome(outcome, cw, code);
    code.bind(next);
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

#[derive(Clone, Copy)]
/// A method a stub or bridge writes: its JVM name, parameters with their names, descriptor, and
/// generic signature.
pub(super) struct ThrowingMethod<'a> {
    pub(super) name: &'a str,
    pub(super) parameters: &'a [Ty],
    pub(super) parameter_names: &'a [String],
    pub(super) descriptor: &'a str,
    pub(super) signature: Option<&'a str>,
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
