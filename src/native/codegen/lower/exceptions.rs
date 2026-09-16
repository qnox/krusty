//! How an exception travels: the pending slot, the check after every call, and `try`/`catch`.
//!
//! There is no unwinder. A `throw` records the exception in one runtime slot and RETURNS the
//! frame's zero value; every call site loads that slot and branches — to the innermost enclosing
//! `try`'s dispatch block, or out of the frame with the slot still set, which IS the propagation.
//! A caller never reads the value a throwing call returned, because it checks first.
//!
//! `docs/BUILD_AND_NATIVE_PLAN.md` records why this and not the alternatives. Table-driven
//! unwinding needs Cranelift emitting `.eh_frame`, the linker placing it, and a DWARF CFI
//! interpreter in the runtime — a phase, not an increment. `setjmp`/`longjmp` is far smaller and
//! is rejected on CORRECTNESS: it returns twice, which nothing in Cranelift's SSA can express, so
//! a value the generator is entitled to keep in a register across the `try` is stale after the
//! jump, silently and data-dependently.
//!
//! The choice does not leak into the IR. `IrExpr::Try` and `IrExpr::Throw` are unchanged, so
//! moving to table-driven unwinding later is a change to this module and the runtime — not to the
//! common IR, and not to a single test written against it.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// Emit a call, then the check that turns a callee's `throw` into propagation.
    ///
    /// Every call in a function body goes through here. A `throw` stores the exception in the
    /// runtime's one pending slot and RETURNS, so a caller that did not look would carry on with a
    /// zero value as though nothing had happened; looking is the whole mechanism. See "How an
    /// exception propagates" in `docs/BUILD_AND_NATIVE_PLAN.md` for why this and not unwind tables
    /// (they need `.eh_frame` and a DWARF interpreter, which is a phase) or `setjmp`/`longjmp`
    /// (it returns twice, which Cranelift's SSA cannot express without silently stale registers).
    ///
    /// The result stays usable afterwards even though the check moves the builder to a new block:
    /// the call's block branches to that one, so it dominates it.
    pub(super) fn emit_call(
        &mut self,
        func_ref: FuncRef,
        arguments: &[Value],
    ) -> Result<Inst, Unsupported> {
        let call = self.builder.ins().call(func_ref, arguments);
        self.check_pending()?;
        Ok(call)
    }

    /// Call a runtime function WITHOUT the propagation check.
    ///
    /// For the handler machinery itself, and only for it. Deciding which clause takes the
    /// exception happens while one is pending by construction, so a check there reads the very
    /// slot being examined, finds it set, and leaves — the `try` would then swallow nothing and
    /// dispatch would be dead code reached once and abandoned. None of these three can throw:
    /// `kt_pending_exception` and `kt_clear_pending` touch one word, and `kt_is_instance` walks a
    /// descriptor chain.
    fn runtime_call_unchecked(
        &mut self,
        symbol: &str,
        params: &[Ty],
        ret: Ty,
        arguments: &[Value],
    ) -> Result<Option<Value>, Unsupported> {
        let id = self.file.import(symbol, params, ret)?;
        let func_ref = self.func_ref(id);
        let call = self.builder.ins().call(func_ref, arguments);
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// Where an exception in flight goes from here: the innermost enclosing `try`'s dispatch
    /// block, or the block that leaves this frame with it still pending.
    pub(super) fn unwind_target(&mut self) -> Block {
        if let Some(&handler) = self.handlers.last() {
            return handler;
        }
        *self
            .propagate
            .get_or_insert_with(|| self.builder.create_block())
    }

    /// The check after a call: if the callee left an exception pending, go where it goes.
    ///
    /// Emitted after EVERY call, because which callees can throw is not a question this backend
    /// can answer — a cross-file or dependency callee is opaque, and a runtime one may raise the
    /// exception Kotlin specifies for it. The stated cost is one load and one perfectly predicted
    /// branch per call; what it buys is a mechanism correct under Cranelift's SSA on all three
    /// architectures, because it is ordinary control flow rather than something the machine has to
    /// agree to.
    pub(super) fn check_pending(&mut self) -> Result<(), Unsupported> {
        // A LOAD from the runtime's slot, not a call to an accessor for it. A call clobbers the
        // caller-saved registers, so one after every call doubles what a frame has to keep alive
        // across a call boundary — and the frames grow. The corpus priced that exactly: a
        // 100,000-deep recursion overflowed its stack when this was a call and does not when it is
        // a load. It is also what the design was costed at: one load, one predicted branch.
        let slot = self.file.import_data("kt_pending")?;
        let address = self.data_address(slot);
        let pending = self
            .builder
            .ins()
            .load(types::I64, objects::trusted(), address, 0);
        let target = self.unwind_target();
        let continuation = self.builder.create_block();
        self.builder
            .ins()
            .brif(pending, target, &[], continuation, &[]);
        self.builder.switch_to_block(continuation);
        self.builder.seal_block(continuation);
        Ok(())
    }

    /// Leave the frame with the exception still pending: return the frame's zero value, which no
    /// caller reads because every caller checks first.
    pub(super) fn seal_propagation(&mut self) {
        let Some(propagate) = self.propagate.take() else {
            return;
        };
        self.builder.switch_to_block(propagate);
        self.builder.seal_block(propagate);
        match self.result {
            Carrier::Void => {
                self.builder.ins().return_(&[]);
            }
            carrier => {
                let clif = carrier
                    .clif()
                    .expect("a non-void carrier has a Cranelift type");
                let zero = if clif == types::F32 {
                    self.builder.ins().f32const(0.0)
                } else if clif == types::F64 {
                    self.builder.ins().f64const(0.0)
                } else {
                    self.builder.ins().iconst(clif, 0)
                };
                self.builder.ins().return_(&[zero]);
            }
        }
    }

    /// `throw e`.
    ///
    /// A store followed by a jump: `kt_throw` records the exception and RETURNS, so what remains
    /// is to go where an exception in flight goes from here — the innermost enclosing `try`, or
    /// out of the frame. `throw` is `Nothing`, so nothing reads a value from it.
    ///
    /// Deliberately NOT through [`Self::runtime_call`]: the check that helper emits would branch
    /// on a slot this very call just set, which is the jump below written twice.
    pub(super) fn throw(&mut self, operand: u32) -> Result<Option<Value>, Unsupported> {
        let thrown = self.expression(operand)?;
        if self.terminated {
            return Ok(None);
        }
        let Some(thrown) = thrown else {
            return Err("a `throw` of a `Unit` value".to_string());
        };
        let id = self.file.import("kt_throw", &[any()], Ty::Unit)?;
        let func_ref = self.func_ref(id);
        self.builder.ins().call(func_ref, &[thrown]);
        let target = self.unwind_target();
        self.builder.ins().jump(target, &[]);
        self.terminate();
        Ok(None)
    }

    /// `assertFailsWith<T> { … }` — run the block, and answer the exception it had to throw.
    ///
    /// The reified `T` never reaches here as a type argument: kotlinc resolves it into the call's
    /// RETURN type, so `assertFailsWith<IllegalStateException> { … }` arrives as a call answering
    /// `IllegalStateException`. That is the class to test against, and the descriptor it already
    /// wears is the test — the same `kt_is_instance` a `catch` clause uses, so a SUPERTYPE matches
    /// exactly as it does there (`assertFailsWith<RuntimeException>` takes an
    /// `IllegalStateException`, which kotlinc confirms).
    ///
    /// The block is the LAST argument, never the first: `message` is declared before it and
    /// defaulted, so a call that omits it passes one argument and a call that supplies it passes
    /// two. Reading the first would take the message for the block wherever one was given.
    ///
    /// A block that throws the WRONG type is an assertion failure, not a propagation — kotlin-test
    /// catches `Throwable` and fails the assertion with what it caught, so the original exception
    /// is REPLACED rather than allowed past. Verified against kotlinc 2.4.10 rather than assumed;
    /// letting it propagate is the plausible reading and it is wrong.
    pub(super) fn assert_fails_with(
        &mut self,
        args: &[u32],
        params: &[Ty],
        expected: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let Some((&block, leading)) = args.split_last() else {
            return Err("`assertFailsWith` with no block".to_string());
        };
        let Some(descriptor) = self.file.type_descriptor(expected.non_null())? else {
            return Err(format!(
                "`assertFailsWith` of `{}`, which wears no runtime descriptor",
                super::objects::type_name_of(expected)
            ));
        };
        // The caller's message prefix, when there is one. `params` always declares it; `args` only
        // carries it when the call did not leave it defaulted.
        let message = match leading {
            [] => None,
            [only] if params.len() == args.len() => Some(*only),
            _ => return Err("`assertFailsWith` with an unexpected argument shape".to_string()),
        };
        let message = match message {
            Some(arg) => self.reference(arg)?,
            None => self.builder.ins().iconst(types::I64, 0),
        };
        if self.terminated {
            return Ok(None);
        }
        let function = self.reference(block)?;
        if self.terminated {
            return Ok(None);
        }
        let descriptor = self.data_address(descriptor);

        let dispatch = self.builder.create_block();
        let join = self.builder.create_block();
        self.builder.append_block_param(join, types::I64);

        self.handlers.push(dispatch);
        let ran = self.invoke_value(function, &[], Ty::Unit);
        self.handlers.pop();
        ran?;

        // The block completed. Kotlin fails the assertion here, and the failure is itself a throw,
        // so what follows is the ordinary propagation every raise gets.
        if !self.terminated {
            let nothing = self.builder.ins().iconst(types::I64, 0);
            self.runtime_call(
                "kt_assert_failed_to_throw",
                &[any(), any(), any()],
                Ty::Unit,
                &[message, descriptor, nothing],
            )?;
            if !self.terminated {
                let onward = self.unwind_target();
                self.builder.ins().jump(onward, &[]);
                self.terminate();
            }
        }

        self.continue_in(dispatch);
        self.builder.seal_block(dispatch);
        let thrown = self
            .runtime_call_unchecked("kt_pending_exception", &[], any(), &[])?
            .expect("the pending exception is a reference");
        let matched = self
            .runtime_call_unchecked(
                "kt_is_instance",
                &[any(), any()],
                Ty::Boolean,
                &[thrown, descriptor],
            )?
            .expect("`kt_is_instance` answers a Boolean");
        let took = self.builder.create_block();
        let wrong = self.builder.create_block();
        self.builder.ins().brif(matched, took, &[], wrong, &[]);

        self.continue_in(took);
        self.builder.seal_block(took);
        self.runtime_call_unchecked("kt_clear_pending", &[], Ty::Unit, &[])?;
        self.builder.ins().jump(join, &[BlockArg::Value(thrown)]);

        // The wrong type: clear it first, because the assertion failure REPLACES it rather than
        // joining it, and a raise onto a slot that is already set would lose the new one.
        self.continue_in(wrong);
        self.builder.seal_block(wrong);
        self.runtime_call_unchecked("kt_clear_pending", &[], Ty::Unit, &[])?;
        self.runtime_call(
            "kt_assert_failed_to_throw",
            &[any(), any(), any()],
            Ty::Unit,
            &[message, descriptor, thrown],
        )?;
        if !self.terminated {
            let onward = self.unwind_target();
            self.builder.ins().jump(onward, &[]);
            self.terminate();
        }

        self.continue_in(join);
        self.builder.seal_block(join);
        Ok(Some(self.builder.block_params(join)[0]))
    }

    /// `try { … } catch (e: T) { … }`.
    ///
    /// The body is lowered with a DISPATCH block pushed on the handler stack, so the check after
    /// every call inside it — and any `throw` written there — branches to that block instead of
    /// leaving the frame. Dispatch loads the pending exception and tests each clause's type in
    /// source order, which is Kotlin's order and the JVM's; a clause that matches CLEARS the slot
    /// before running, because from there the exception is handled and an allocation in the
    /// handler must not see it still in flight. No clause matching falls through to wherever an
    /// exception would have gone without this `try` at all, which is the propagation this `try`
    /// simply does not participate in.
    ///
    /// `finally` declines. Its rule is that the block runs on EVERY way out — normal completion,
    /// each handler, propagation, and a `return` written inside the body — so it is a separate
    /// piece of work rather than one more edge, and answering it half-way would be the kind of
    /// silent wrong answer this backend declines instead of giving.
    pub(super) fn try_catch(
        &mut self,
        body: u32,
        catches: &[crate::ir::IrCatch],
        finally: Option<u32>,
        result: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        if finally.is_some() {
            return Err("a `try` with a `finally`".to_string());
        }
        let merge = self.builder.create_block();
        let result = Some(result).filter(|ty| carrier(*ty) != Carrier::Void);
        if let Some(ty) = result {
            let clif = carrier(ty).clif().expect("non-void carrier");
            self.builder.append_block_param(merge, clif);
        }
        let dispatch = self.builder.create_block();

        let mut reaches_merge = false;
        self.handlers.push(dispatch);
        let guarded = self.arm(body, result, merge, &mut reaches_merge);
        self.handlers.pop();
        guarded?;

        // Dispatch is reached only from inside the body, and the body is fully lowered, so every
        // edge into it exists by now.
        self.continue_in(dispatch);
        self.builder.seal_block(dispatch);
        let thrown = self
            .runtime_call_unchecked("kt_pending_exception", &[], any(), &[])?
            .expect("the pending exception is a reference");
        for catch in catches {
            let Some(descriptor) = self
                .file
                .type_descriptor(Ty::obj_name(catch.exc_internal))?
            else {
                return Err(format!(
                    "a `catch` of `{}`, which wears no runtime descriptor",
                    catch.exc_internal.render()
                ));
            };
            let descriptor = self.data_address(descriptor);
            let matched = self
                .runtime_call_unchecked(
                    "kt_is_instance",
                    &[any(), any()],
                    Ty::Boolean,
                    &[thrown, descriptor],
                )?
                .expect("`kt_is_instance` answers a Boolean");
            let handle = self.builder.create_block();
            let next = self.builder.create_block();
            self.builder.ins().brif(matched, handle, &[], next, &[]);

            self.continue_in(handle);
            self.builder.seal_block(handle);
            // Cleared BEFORE the handler runs: from here the exception is this clause's value, not
            // something in flight, and the handler's own calls check the slot like any others.
            self.runtime_call_unchecked("kt_clear_pending", &[], Ty::Unit, &[])?;
            let variable = self.declare_value(catch.var, Ty::obj_name(catch.exc_internal))?;
            self.builder.def_var(variable, thrown);
            self.arm(catch.body, result, merge, &mut reaches_merge)?;

            self.continue_in(next);
            self.builder.seal_block(next);
        }
        // No clause named it: it keeps going where it was going.
        let onward = self.unwind_target();
        self.builder.ins().jump(onward, &[]);
        self.terminate();

        if !reaches_merge {
            // Body and every handler left. Nothing follows the `try`.
            self.terminated = true;
            return Ok(None);
        }
        self.continue_in(merge);
        self.builder.seal_block(merge);
        Ok(result.map(|_| self.builder.block_params(merge)[0]))
    }
}
