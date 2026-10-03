//! A function value's invocation: `FunctionN.invoke` returns erased `Object`, which the consumer
//! materializes at the type it needs. A consumer that takes the erased slot as it is (see
//! `emit_consumed_operand`) skips the narrowing to the function's declared result.

use super::*;

pub(super) fn is_high_arity_function(arity: u8) -> bool {
    crate::jvm::names::uses_function_n(usize::from(arity))
}

pub(super) fn jvm_function_interface(arity: u8) -> String {
    crate::jvm::names::function_interface_internal_name(usize::from(arity))
}

/// `FunctionN.invoke`'s erased descriptor: one `Object` per parameter, or the array of them.
pub(super) fn jvm_function_invoke_descriptor(arity: u8) -> String {
    let parameters = if is_high_arity_function(arity) {
        "[Ljava/lang/Object;".to_string()
    } else {
        "Ljava/lang/Object;".repeat(usize::from(arity))
    };
    format!("({parameters})Ljava/lang/Object;")
}

impl Emitter<'_> {
    /// Push the function value and its boxed arguments, then call `invoke`, leaving its erased
    /// `Object` result.
    pub(super) fn emit_function_invocation(
        &mut self,
        e: u32,
        func: u32,
        args: &[u32],
        params: &[Ty],
        code: &mut CodeBuilder,
    ) {
        let n = args.len();
        let high_arity = is_high_arity_function(n as u8);
        let argument_array_type = Ty::array(Ty::nullable(Ty::obj("kotlin/Any")));
        if args.iter().any(|&a| self.spills_operand_prefix(a)) {
            // An argument that cannot carry the operand stack can't run with the function value
            // on it. Evaluate the function + args into temps first (in order), then load and box.
            let mut all = vec![func];
            all.extend(args.iter().copied());
            let temps = self.spill_to_temps(&all, code);
            load(temps[0].1, temps[0].0, code);
            self.cast_receiver_to_invocation_interface(func, n as u8, code);
            let argument_array = if high_arity {
                code.push_int(n as i32, self.cw);
                let object = self.cw.class_ref("java/lang/Object");
                code.anewarray(object);
                let temporary = self
                    .frame
                    .enter_temp(frame_map::TempRole::FunctionArguments, argument_array_type);
                let slot = temporary.slot();
                store(argument_array_type, slot, code);
                Some((
                    slot,
                    self.lease_frame_temporary(temporary, argument_array_type),
                ))
            } else {
                None
            };
            for (i, &(slot, t, _)) in temps[1..].iter().enumerate() {
                if let Some((array, _)) = argument_array {
                    load(argument_array_type, array, code);
                    code.push_int(i as i32, self.cw);
                }
                load(t, slot, code);
                let semantic = params.get(i).copied().unwrap_or(t);
                box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, t));
                if high_arity {
                    code.array_store(0x53, 1); // aastore
                }
            }
            if let Some((slot, lease)) = argument_array {
                load(argument_array_type, slot, code);
                self.release_temporary(lease);
            }
            self.release_operand_spills(&temps);
        } else {
            self.emit_value(func, code);
            self.cast_receiver_to_invocation_interface(func, n as u8, code);
            let argument_array = if high_arity {
                code.push_int(n as i32, self.cw);
                let object = self.cw.class_ref("java/lang/Object");
                code.anewarray(object);
                let temporary = self
                    .frame
                    .enter_temp(frame_map::TempRole::FunctionArguments, argument_array_type);
                let slot = temporary.slot();
                store(argument_array_type, slot, code);
                Some((
                    slot,
                    self.lease_frame_temporary(temporary, argument_array_type),
                ))
            } else {
                None
            };
            for (i, &arg) in args.iter().enumerate() {
                if let Some((array, _)) = argument_array {
                    load(argument_array_type, array, code);
                    code.push_int(i as i32, self.cw);
                }
                self.emit_value(arg, code);
                let at = self.value_ty(arg);
                let semantic = params.get(i).copied().unwrap_or(at);
                // `FunctionN` parameters are erased `Object`, but wrapper selection is a
                // semantic operation. Retaining `params` on the IR node prevents an unsigned
                // argument from being boxed as the signed wrapper of its shared carrier.
                box_prim_free(self.cw, code, semantic_scalar_adapter(semantic, at));
                if high_arity {
                    code.array_store(0x53, 1); // aastore
                }
            }
            if let Some((slot, lease)) = argument_array {
                load(argument_array_type, slot, code);
                self.release_temporary(lease);
            }
        }
        let iface = jvm_function_interface(n as u8);
        let m =
            self.cw
                .interface_methodref(&iface, "invoke", &jvm_function_invoke_descriptor(n as u8));
        // A function VALUE's invocation is a dispatch like any other: after the operands
        // have each marked their own line, the call's own line returns at the `invoke`.
        // A transformed suspension also takes the markers a direct suspend call takes.
        self.mark_call_start(e, code);
        code.invokeinterface(m, if high_arity { 1 } else { n as i32 }, 1);
    }

    /// `invokeinterface` dispatches on the selected `FunctionN`. A receiver already realized as
    /// that interface is invoked directly; every other realized representation is `checkcast`
    /// to it first.
    fn cast_receiver_to_invocation_interface(
        &mut self,
        func: u32,
        arity: u8,
        code: &mut CodeBuilder,
    ) {
        let interface = jvm_function_interface(arity);
        let realized = ir_ty_to_jvm(&self.value_ty(func));
        let already_interface = matches!(
            realized.non_null(),
            Ty::Obj(name, _) if name.matches(&interface)
        );
        if !already_interface {
            code.checkcast(self.cw.class_ref(&interface));
        }
    }

    /// Read an invocation's erased result as the function's declared return type `ret`.
    pub(super) fn narrow_invocation_result(&mut self, ret: Ty, code: &mut CodeBuilder) {
        // The interface returns `Object`; cast/unbox to the function's declared return type.
        // Select a scalar adapter from that semantic return before `ir_ty_to_jvm` reduces an
        // unsigned type to its signed carrier. This is the common consumer for real lambdas,
        // callable references, and property references, independent of which producer object
        // supplied the `FunctionN` implementation.
        let rt = ir_ty_to_jvm(&ret);
        if rt.is_jvm_scalar() {
            unbox_prim_from(
                self.cw,
                code,
                Ty::obj("java/lang/Object"),
                semantic_scalar_adapter(ret, rt),
            );
        } else {
            match rt {
                Ty::Unit | Ty::Nothing => code.pop(),
                Ty::String => {
                    let ci = self.cw.class_ref("java/lang/String");
                    code.checkcast(ci);
                }
                _ if rt.is_array() => {
                    let ci = self.cw.class_ref(&type_descriptor(rt));
                    code.checkcast(ci);
                }
                Ty::Obj(internal, _) => {
                    let ci = self.cw.class_ref(&internal.render());
                    code.checkcast(ci);
                }
                _ => {}
            }
        }
    }
}
