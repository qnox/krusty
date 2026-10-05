//! JVM method-entry guards.

use super::{bridge_emission, CodeBuilder, EmitEnv, Emitter};
use crate::ir::FunId;
use crate::types::{Ty, TypeName};

pub(super) fn emit(
    function: FunId,
    parameter_types: &[Ty],
    instance: bool,
    holder_receiver: Option<TypeName>,
    env: &EmitEnv<'_>,
    emitter: &mut Emitter<'_>,
    code: &mut CodeBuilder,
) {
    let ir = emitter.ir;
    // A non-null `Any` collection parameter shares the Java method's descriptor, so `null` never
    // reaches a bridge. The recorded plan returns the neutral result; that parameter has no
    // further `checkNotNullParameter`, because the guard already proved it present.
    let mut covered = None;
    if instance && holder_receiver.is_none() {
        if let Some(plan) = env.collection_method_entry_barriers.plan(function) {
            let value = u32::try_from(plan.parameter).expect("a barrier parameter index fits")
                + u32::from(instance);
            if let Some(&(slot, _)) = emitter.slots.get(&value) {
                let present = code.new_label();
                code.aload(slot);
                code.ifnonnull(present);
                bridge_emission::emit_barrier_outcome(plan.outcome, emitter.cw, code);
                code.bind(present);
                covered = Some(plan.parameter);
            }
        }
    }

    // kotlinc guards each non-null reference parameter of a visible function with
    // `Intrinsics.checkNotNullParameter(param, "name")` at method entry.
    let declared = &ir.functions[function as usize];
    let parameter_identities = ir.function_parameter_identities(function);
    let assertion_names = crate::jvm::parameter_names::placed(
        ir,
        function,
        holder_receiver,
        crate::jvm::parameter_names::function_assertions(ir, function, parameter_types),
    );
    for (index, check) in declared.param_checks.iter().enumerate() {
        if check.is_none() || covered == Some(index) {
            continue;
        }
        let _identity = parameter_identities
            .and_then(|identities| identities.get(index))
            .expect("a checked parameter carries an exact identity");
        let name = assertion_names
            .as_ref()
            .and_then(|names| names.get(index))
            .and_then(Clone::clone)
            .expect("a checked parameter carries an assertion spelling");
        let value = index as u32 + u32::from(instance);
        if let Some(&(slot, _)) = emitter.slots.get(&value) {
            emitter.checked_parameters.insert(value);
            code.aload(slot);
            code.push_string(&name, emitter.cw);
            let method = emitter.cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNullParameter",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            );
            code.invokestatic(method, 2, 0);
        }
    }
}
