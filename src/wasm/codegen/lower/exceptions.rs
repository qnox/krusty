//! Base Throwable realization and structured catch-only Kotlin exception transport.

use crate::backend::BackendCompilerIntrinsic as CompilerIntrinsic;
use crate::fir::ExternalCallableId;
use crate::ir::IrCatch;
use crate::types::TypeName;

use super::*;

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
            crate::libraries::RuntimeExceptionKind::NullPointer.classifier(),
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
        let operation = external
            .and_then(|target| self.callables.callable(target))
            .and_then(|fact| fact.compiler_operation());
        let Some(CompilerIntrinsic::ThrowableConstructor { message }) = operation else {
            return self.construction(site, internal, args, params);
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
