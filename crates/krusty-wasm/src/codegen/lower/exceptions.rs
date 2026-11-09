//! Authenticated Throwable realization and ordered Kotlin exception handlers.

use krusty::backend::BackendCompilerIntrinsic as CompilerIntrinsic;
use krusty::fir::ExternalCallableId;
use krusty::ir::IrCatch;
use krusty::types::TypeName;

use super::*;

#[cfg(test)]
mod tests;

/// The selected constructor's complete source frame, independent of a target descriptor.
fn constructor_frame(
    internal: TypeName,
    selected: Option<&[Ty]>,
    argument_count: usize,
    kind: krusty::libraries::RuntimeExceptionKind,
    message: bool,
    declaration: &krusty::backend::BackendCallableFact,
) -> Result<(), Unsupported> {
    let message_parameter = [Ty::nullable(Ty::String)];
    let parameters = if message { &message_parameter[..] } else { &[] };
    if internal != kind.classifier()
        || declaration.declaration_owner
            != Some(krusty::types::SemanticCallableOwner::Classifier(internal))
        || declaration.kind != krusty::libraries::ExternalCallableKind::Constructor
        || declaration.params != parameters
        || selected != Some(parameters)
        || argument_count != parameters.len()
        || declaration.ret != Ty::Unit
        || declaration.context_count != 0
        || declaration.source_receiver.is_some()
        || declaration.generic_sig.is_some()
    {
        return Err(
            "an authenticated exception constructor with a mismatched declaration frame".into(),
        );
    }
    Ok(())
}

/// A jump out of protected code: the function's `return`, or a `br` to the structured instruction
/// opened at an absolute depth (a loop's `break` or `continue` target).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Transfer {
    Return,
    Branch(u32),
}

/// A `finally` region whose protected code (its `try` body and catch clauses) is being emitted.
///
/// A transfer that leaves the region records itself here, stores its ordinal in `pending` and
/// branches to `exit`, a block outside the region's exception handler. That exit runs one copy of
/// the cleanup and then resumes the recorded transfer, which an enclosing region may intercept
/// again. This is the structured form of kotlinc's per-exit `finally` copies: the cleanup never
/// runs under its own handler, so a throw from it is never cleaned up twice.
pub(super) struct CleanupFrame {
    /// The depth of the region's outermost block: a branch to anything shallower leaves it.
    base: u32,
    exit: u32,
    pending: u32,
    /// The value of the pending `return`, once protected code returns one.
    return_slot: Option<u32>,
    transfers: Vec<Transfer>,
}

impl CleanupFrame {
    fn intercepts(&self, transfer: Transfer) -> bool {
        match transfer {
            Transfer::Return => true,
            Transfer::Branch(target) => target < self.base,
        }
    }
}

// A cleanup frame is only needed when protected code can transfer control. Check the complete
// evaluated subtree before emitting anything, conservatively including nested loops and inline
// bodies; a transfer that stays inside the region records nothing and costs only the exit block.
fn finally_has_transfer(exprs: &[IrExpr], roots: impl IntoIterator<Item = u32>) -> bool {
    let mut pending: Vec<_> = roots.into_iter().collect();
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        if matches!(
            exprs[id as usize],
            IrExpr::Return(_) | IrExpr::Break { .. } | IrExpr::Continue { .. }
        ) {
            return true;
        }
        krusty::ir::for_each_child(exprs, id, &mut |child| pending.push(child));
    }
    false
}

// Validate the actual use, not the logical Nothing type: discarded bottom values may complete.
fn finally_input(
    ir: &IrFile,
    declared: &HashMap<u32, Ty>,
    input: u32,
    discarded: bool,
) -> Result<bool, Unsupported> {
    let diverges = if discarded {
        ir.expr_discarding_diverges_by(input, &|_, _| false)
    } else {
        ir.expr_diverges_by(input, &|_, _| false)
    };
    if expression_type(ir, declared, input)?.is_none() && !diverges {
        return Err("a `finally` region without a proven completion carrier".into());
    }
    Ok(diverges)
}

impl Body<'_> {
    /// A reference assertion keeps its precise heap carrier and evaluates its input once. Its
    /// failed edge throws the authenticated Kotlin NPE through the ordinary exception tag.
    pub(super) fn assert_nonnull_reference(
        &mut self,
        operand: u32,
        reference: ValType,
    ) -> Result<(), Unsupported> {
        let structure = self.runtime.exceptions.ensure_class(
            self.module,
            self.classifiers,
            krusty::libraries::RuntimeExceptionKind::NullPointer.classifier(),
        )?;
        let value = self.local(reference);
        self.expression(operand)?;
        self.code
            .local_set(value)
            .local_get(value)
            .ref_is_null()
            .if_(BlockType::Empty);
        self.runtime.exceptions.construct_class(
            &mut self.code,
            structure,
            None,
            self.runtime.string,
        );
        self.code
            .throw(self.runtime.exceptions.tag)
            .end()
            .local_get(value)
            .ref_as_non_null();
        Ok(())
    }

    pub(super) fn new_object(
        &mut self,
        site: u32,
        internal: TypeName,
        args: &[u32],
        params: Option<&[Ty]>,
        external: Option<ExternalCallableId>,
        defaults: &[u32],
    ) -> Result<(), Unsupported> {
        // The shared default-argument lowering rewrites every construction with omitted
        // arguments it can realize to construct through its constructor's entry.
        if !defaults.is_empty() {
            return Err("a constructor call with omitted arguments".to_string());
        }
        let Some((declaration, kind, message)) = external
            .and_then(|target| self.callables.callable(target))
            .and_then(|fact| match fact.compiler_operation() {
                Some(CompilerIntrinsic::ThrowableConstructor { kind, message }) => {
                    Some((fact, kind, message))
                }
                _ => None,
            })
        else {
            if let Some(constructed) =
                external.and_then(|target| self.dependency_construction(target, internal, args))
            {
                return constructed;
            }
            return self.construction(site, internal, args, params);
        };
        constructor_frame(internal, params, args.len(), kind, message, declaration)?;
        let structure =
            self.runtime
                .exceptions
                .ensure_class(self.module, self.classifiers, internal)?;
        let message = match (message, args) {
            (false, []) => None,
            (true, [message]) => {
                let slot = self.local(STRING);
                self.value(*message, Ty::nullable(Ty::String))?;
                self.code.local_set(slot);
                Some(slot)
            }
            _ => {
                return Err("an authenticated Throwable constructor with malformed operands".into())
            }
        };
        self.runtime.exceptions.construct_class(
            &mut self.code,
            structure,
            message,
            self.runtime.string,
        );
        Ok(())
    }

    pub(super) fn is_throwable_message(&self, target: ExternalCallableId) -> bool {
        self.callables.callable(target).is_some_and(|fact| {
            fact.compiler_operation() == Some(CompilerIntrinsic::ThrowableMessage)
        })
    }

    pub(super) fn is_throwable_message_property(
        &self,
        target: krusty::fir::ExternalPropertyId,
    ) -> bool {
        self.callables
            .property(target)
            .is_some_and(|property| self.is_throwable_message(property.getter))
    }

    pub(super) fn throwable_message(
        &mut self,
        receiver: Option<u32>,
        args: &[u32],
        result: Ty,
    ) -> Result<(), Unsupported> {
        let (Some(receiver), []) = (receiver, args) else {
            return Err("an authenticated Throwable.message with malformed operands".into());
        };
        if result != Ty::nullable(Ty::String) {
            return Err(
                "an authenticated Throwable.message with a non-nullable-String result".into(),
            );
        }
        self.value(receiver, Ty::obj("kotlin/Throwable"))?;
        self.runtime.exceptions.message(&mut self.code);
        Ok(())
    }

    /// The struct a caught exception class's instances are: this file's class, or the runtime
    /// exception class.
    fn exception_structure(&mut self, identity: TypeName) -> Result<u32, Unsupported> {
        if let Some(class) = self.classes.and(self.ir.class_id_by_name(identity)) {
            return Ok(self.file_classes()?.structure(class));
        }
        self.runtime
            .exceptions
            .ensure_class(self.module, self.classifiers, identity)
    }

    pub(super) fn throw_value(&mut self, operand: u32) -> Result<(), Unsupported> {
        self.value(operand, Ty::obj("kotlin/Throwable"))?;
        self.code
            .ref_as_non_null()
            .throw(self.runtime.exceptions.tag);
        Ok(())
    }

    pub(super) fn try_catch(
        &mut self,
        body: u32,
        catches: &[IrCatch],
        finally: Option<u32>,
        result: Ty,
    ) -> Result<(), Unsupported> {
        if let Some(cleanup) = finally {
            return self.try_finally(body, catches, cleanup, result);
        }
        if catches.is_empty() {
            return Err("a `try` without a catch".into());
        }
        let mut handlers = Vec::with_capacity(catches.len());
        for catch in catches {
            let identity = catch
                .ty
                .obj_internal()
                .filter(|_| !catch.ty.is_nullable())
                .ok_or_else(|| format!("a catch of `{}`", describe(catch.ty)))?;
            // The frozen declaration and its complete authenticated superclass chain own the
            // physical subtype. Source spelling and handler position never infer its identity.
            let structure = self.exception_structure(identity)?;
            handlers.push((catch, structure));
        }
        let result_block = carrier(result)?.map_or(BlockType::Empty, BlockType::Value);
        self.code.block(result_block);
        let done = self.open();
        self.code.block(BlockType::Value(REFERENCE));
        let handler = self.open();
        self.code.try_table(
            BlockType::Empty,
            self.runtime.exceptions.tag,
            self.relative(handler),
        );
        self.open();
        self.value(body, result)?;
        self.code.br(self.relative(done));
        self.close();
        // Every normal exit above branches past the handler. Only an exception can leave the
        // payload block; traps and foreign host exceptions do not acquire a Kotlin catch.
        self.code.unreachable();
        self.close();
        let caught = self.local(REFERENCE);
        self.code.local_set(caught);
        for (catch, structure) in handlers {
            self.code
                .local_get(caught)
                .ref_test(false, HeapType::Concrete(structure))
                .if_(BlockType::Empty);
            self.open();
            let previous = self.values.insert(catch.var, (caught, catch.ty));
            let answer = self.value(catch.body, result);
            match previous {
                Some(previous) => {
                    self.values.insert(catch.var, previous);
                }
                None => {
                    self.values.remove(&catch.var);
                }
            }
            answer?;
            // A selected handler leaves the whole try expression. Later siblings cannot catch
            // exceptions thrown by a handler or steal a value from an earlier matching clause.
            self.code.br(self.relative(done));
            self.close();
        }
        // No Kotlin handler matched. Re-throw the original payload through the same tag so an
        // enclosing handler observes the original identity; host exceptions never enter here.
        self.code
            .local_get(caught)
            .throw(self.runtime.exceptions.tag);
        self.close();
        Ok(())
    }

    /// Leave the current position by `transfer`. The value of a `return` is on the stack, or in
    /// `parked`, the slot of the region whose cleanup just ran and is now resuming the return.
    pub(super) fn control_transfer(
        &mut self,
        transfer: Transfer,
        parked: Option<u32>,
    ) -> Result<(), Unsupported> {
        let carried = match transfer {
            Transfer::Return => carrier(self.result)?,
            Transfer::Branch(_) => None,
        };
        let intercepted = self
            .cleanups
            .last()
            .is_some_and(|frame| frame.intercepts(transfer));
        if !intercepted {
            match transfer {
                Transfer::Return => {
                    if let (Some(_), Some(parked)) = (carried, parked) {
                        self.code.local_get(parked);
                    }
                    self.code.return_();
                }
                Transfer::Branch(target) => {
                    let depth = self.relative(target);
                    self.code.br(depth);
                }
            }
            return Ok(());
        }
        if let Some(ty) = carried {
            // Each region owns the value of the return it holds pending. A return that a nested
            // region's cleanup starts and then cancels writes only that region's slot, so this
            // region resumes its own value.
            let slot = match self.cleanups.last().and_then(|frame| frame.return_slot) {
                Some(slot) => slot,
                None => {
                    let slot = self.local(ty);
                    self.cleanups
                        .last_mut()
                        .expect("an intercepting frame")
                        .return_slot = Some(slot);
                    slot
                }
            };
            if let Some(parked) = parked {
                self.code.local_get(parked);
            }
            self.code.local_set(slot);
        }
        let frame = self.cleanups.last_mut().expect("an intercepting frame");
        let ordinal = match frame.transfers.iter().position(|known| *known == transfer) {
            Some(ordinal) => ordinal,
            None => {
                frame.transfers.push(transfer);
                frame.transfers.len() - 1
            }
        };
        let (pending, exit) = (frame.pending, frame.exit);
        let depth = self.relative(exit);
        self.code
            .i32_const(ordinal as i32)
            .local_set(pending)
            .br(depth);
        Ok(())
    }

    fn try_finally(
        &mut self,
        body: u32,
        catches: &[IrCatch],
        cleanup: u32,
        result: Ty,
    ) -> Result<(), Unsupported> {
        let transfers = finally_has_transfer(
            &self.ir.exprs,
            std::iter::once(body).chain(catches.iter().map(|catch| catch.body)),
        );
        let carried = carrier(result)?;
        let cleanup_diverges = finally_input(self.ir, &self.declared, cleanup, true)?;
        let mut protected_diverges =
            finally_input(self.ir, &self.declared, body, carried.is_none())?;
        for catch in catches {
            protected_diverges &=
                finally_input(self.ir, &self.declared, catch.body, carried.is_none())?;
        }
        let saved = carried.map(|ty| self.local(ty));
        self.code.block(BlockType::Empty);
        let done = self.open();
        let exit = transfers.then(|| {
            self.code.block(BlockType::Empty);
            (self.open(), self.local(ValType::I32))
        });
        self.code.block(BlockType::Value(REFERENCE));
        let handler = self.open();
        self.code.try_table(
            BlockType::Empty,
            self.runtime.exceptions.tag,
            self.relative(handler),
        );
        self.open();
        if let Some((exit, pending)) = exit {
            self.cleanups.push(CleanupFrame {
                base: done,
                exit,
                pending,
                return_slot: None,
                transfers: Vec::new(),
            });
        }
        let protected = if catches.is_empty() {
            self.value(body, result)
        } else {
            self.try_catch(body, catches, None, result)
        };
        let frame = exit.and_then(|_| self.cleanups.pop());
        protected?;
        if protected_diverges {
            self.code.unreachable();
        }
        if let Some(saved) = saved {
            self.code.local_set(saved);
        }
        self.code.br(self.relative(done));
        self.close();
        self.code.unreachable();
        self.close();
        let caught = self.local(REFERENCE);
        self.code.local_set(caught);
        // Every copy of the cleanup is outside the try_table. A cleanup throw replaces the saved
        // exception; only completed cleanup rethrows the identical original Kotlin payload. Host
        // traps and foreign exceptions do not enter the Kotlin tag handler.
        self.statement(cleanup)?;
        self.code
            .local_get(caught)
            .ref_as_non_null()
            .throw(self.runtime.exceptions.tag);
        if let Some(frame) = frame {
            // Only a recorded transfer reaches the exit block's end.
            self.close();
            if frame.transfers.is_empty() {
                self.code.unreachable();
            } else {
                self.statement(cleanup)?;
                for (ordinal, transfer) in frame.transfers.iter().enumerate() {
                    self.code
                        .local_get(frame.pending)
                        .i32_const(ordinal as i32)
                        .op(EQ32)
                        .if_(BlockType::Empty);
                    self.open();
                    self.control_transfer(*transfer, frame.return_slot)?;
                    self.close();
                }
                self.code.unreachable();
            }
        }
        self.close();
        self.statement(cleanup)?;
        if let Some(saved) = saved {
            self.code.local_get(saved);
        }
        // A structured join must not turn proven noncompletion into a reachable empty stack.
        // Logical Nothing alone is insufficient after inline control-flow rewriting.
        if cleanup_diverges || protected_diverges {
            self.code.unreachable();
        }
        Ok(())
    }
}
