//! Base Throwable realization and structured catch-only Kotlin exception transport.

use crate::backend::BackendCompilerIntrinsic as CompilerIntrinsic;
use crate::fir::ExternalCallableId;
use crate::ir::IrCatch;
use crate::types::TypeName;

use super::*;

impl Body<'_> {
    pub(super) fn new_object(
        &mut self,
        internal: TypeName,
        args: &[u32],
        params: Option<&[Ty]>,
        external: Option<ExternalCallableId>,
        defaults: &[u32],
    ) -> Result<(), Unsupported> {
        if !defaults.is_empty() {
            return Err("a constructor call with omitted arguments".to_string());
        }
        let operation = external
            .and_then(|target| self.callables.callable(target))
            .and_then(|fact| fact.compiler_operation());
        let Some(CompilerIntrinsic::ThrowableConstructor { message }) = operation else {
            return self.construction(internal, args, params);
        };
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
        self.runtime
            .exceptions
            .construct(&mut self.code, message, self.runtime.string);
        Ok(())
    }

    pub(super) fn is_throwable_message(&self, target: ExternalCallableId) -> bool {
        self.callables.callable(target).is_some_and(|fact| {
            fact.compiler_operation() == Some(CompilerIntrinsic::ThrowableMessage)
        })
    }

    pub(super) fn is_throwable_message_property(
        &self,
        target: crate::fir::ExternalPropertyId,
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
        if finally.is_some() {
            return Err("a `try` with `finally`".into());
        }
        let [catch] = catches else {
            return Err("a `try` without exactly one Throwable catch".into());
        };
        let catch_layout = catch
            .ty
            .obj_internal()
            .and_then(|identity| self.classifiers.classifier(identity))
            .and_then(|fact| fact.runtime_exception);
        if catch.ty.is_nullable()
            || !matches!(catch_layout, Some(layout) if layout.kind == crate::libraries::RuntimeExceptionKind::Throwable)
        {
            return Err(format!("a catch of `{}`", describe(catch.ty)));
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
        let previous = self.values.insert(catch.var, (caught, catch.ty));
        self.value(catch.body, result)?;
        match previous {
            Some(previous) => {
                self.values.insert(catch.var, previous);
            }
            None => {
                self.values.remove(&catch.var);
            }
        }
        self.close();
        Ok(())
    }
}
