//! Creating and calling function values (see `crate::wasm::function_values`).
//!
//! Each site that creates one, a lambda or a callable reference, gets its own struct, holding the
//! captures after the dispatch table, its own table and the entry that table names. The entry
//! unpacks the captures, converts each argument from the reference the caller handed over to the
//! type the body declares, calls the body and hands its result back as a reference.

use super::*;
use crate::wasm::encode::{CompositeType, SubType};
use crate::wasm::function_values::INVOKE_FIELD;
use crate::wasm::objects::immutable;

/// What `==` on a function value compares.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FunctionEquality {
    /// A lambda is equal only to itself.
    Identity,
    /// Kotlin makes two references to one declaration equal; this generator does not compare
    /// declarations yet, so such an `==` traps rather than answer `false`.
    Declaration,
}

impl Body<'_> {
    /// A new function value whose body is `function`, carrying `captures` as its leading
    /// arguments.
    pub(super) fn function_value(
        &mut self,
        function: FunId,
        captures: &[u32],
        equality: FunctionEquality,
    ) -> Result<(), Unsupported> {
        let declaration = self
            .ir
            .functions
            .get(function as usize)
            .ok_or("a function value with an invalid body identity")?;
        if declaration.dispatch_receiver.is_some() {
            return Err("a function value whose body is a member".into());
        }
        let params = declaration.params.clone();
        let ret = declaration.ret;
        let own = params
            .get(captures.len()..)
            .ok_or("a function value capturing more than its body takes")?
            .to_vec();
        let body = self.emitted_function(function)?;
        let shape = self.runtime.function_shape(self.module, own.len());

        let mut fields = vec![immutable(ValType::reference(
            self.runtime.objects.any_table,
        ))];
        for &captured in &params[..captures.len()] {
            let value = carrier(captured)?.ok_or("a function value capturing a `Unit` value")?;
            fields.push(immutable(value));
        }
        let structure = self.module.subtype(SubType {
            composite: CompositeType::Struct(fields),
            supertype: Some(self.runtime.objects.object),
            open: false,
        });

        // The entry: `(this, arguments…) -> result`, every operand a reference.
        let mut entry = Code::default();
        let mut locals = Vec::new();
        for field in 0..captures.len() as u32 {
            entry
                .local_get(0)
                .ref_cast(false, HeapType::Concrete(structure))
                .struct_get(structure, field + 1);
        }
        for (index, &param) in own.iter().enumerate() {
            entry.local_get(index as u32 + 1);
            self.from_reference(&mut entry, param)?;
        }
        entry.call(body);
        let scratch = own.len() as u32 + 1;
        self.to_reference(&mut entry, &mut locals, scratch, ret)?;
        let invoke = self.module.declare();
        self.module.define(
            invoke,
            Function {
                ty: shape.invoke,
                locals,
                code: entry,
            },
        );

        let [equals_ty, hash_code_ty, to_string_ty] = self.runtime.objects.any_slot_types;
        let equals = match equality {
            FunctionEquality::Identity => self.runtime.objects.identity_equals,
            FunctionEquality::Declaration => self.runtime.objects.trap(self.module, equals_ty),
        };
        let hash_code = self.runtime.objects.trap(self.module, hash_code_ty);
        let to_string = self.runtime.objects.trap(self.module, to_string_ty);
        let objects = &mut self.runtime.objects;
        let (_, itable) = objects
            .interfaces
            .class(self.module, objects.itable, vec![structure]);
        let mut init = Code::default();
        for function in [equals, hash_code, to_string] {
            self.module.reference_function(function);
            init.ref_func(function);
        }
        self.module.reference_function(invoke);
        init.global_get(itable)
            .ref_func(invoke)
            .struct_new(shape.table);
        let table = self
            .module
            .constant_global(ValType::reference(shape.table), init);

        self.code.global_get(table);
        for (&capture, &captured) in captures.iter().zip(&params) {
            self.value(capture, captured)?;
        }
        self.code.struct_new(structure);
        Ok(())
    }

    /// A new function value for a callable reference. Its adapter takes the referenced
    /// declaration's captures, then the bound receiver, then the caller's arguments.
    pub(super) fn callable_reference(
        &mut self,
        reference: &crate::ir::IrCallableReference,
    ) -> Result<(), Unsupported> {
        let captures: Vec<u32> = reference
            .captures
            .iter()
            .copied()
            .chain(reference.bound_receiver)
            .collect();
        self.function_value(reference.adapter, &captures, FunctionEquality::Declaration)
    }

    /// `function(args)`, where `function` is a function value taking `params` and answering
    /// `ret`.
    pub(super) fn invoke_function(
        &mut self,
        function: u32,
        args: &[u32],
        params: &[Ty],
        ret: Ty,
    ) -> Result<(), Unsupported> {
        if params.len() != args.len() {
            return Err("a function value called with arguments that do not match it".into());
        }
        let any = Ty::obj_name(crate::types::wk::any());
        let shape = self.runtime.function_shape(self.module, args.len());
        let callee = self.local(REFERENCE);
        self.expression(function)?;
        self.code.local_tee(callee);
        for (&arg, &param) in args.iter().zip(params) {
            self.value(arg, param)?;
            self.coerce_emitted_value(param, any)?;
        }
        self.code.local_get(callee);
        let object = self.runtime.objects.object;
        self.code
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, 0)
            .ref_cast(false, HeapType::Concrete(shape.table))
            .struct_get(shape.table, INVOKE_FIELD)
            .call_ref(shape.invoke);
        self.coerce_emitted_value(any, ret)
    }

    /// Convert the reference on top of `code`'s stack to the carrier of `ty`.
    fn from_reference(&mut self, code: &mut Code, ty: Ty) -> Result<(), Unsupported> {
        match carrier(ty)? {
            None => Err("a function value taking a `Unit` value".into()),
            Some(REFERENCE) => Ok(()),
            Some(ValType::Ref { heap, .. }) => {
                code.ref_cast(true, heap);
                Ok(())
            }
            Some(_) => {
                let form = self
                    .runtime
                    .box_form(self.module, ty)
                    .ok_or_else(|| format!("a function value taking a `{}` value", describe(ty)))?;
                form.emit_unbox(code);
                Ok(())
            }
        }
    }

    /// Convert the `ty` value on top of `code`'s stack to a reference, `Unit` to `kotlin.Unit`,
    /// through locals from `first` on, which this records in `locals`.
    fn to_reference(
        &mut self,
        code: &mut Code,
        locals: &mut Vec<ValType>,
        first: u32,
        ty: Ty,
    ) -> Result<(), Unsupported> {
        match carrier(ty)? {
            None => {
                let unit = self.runtime.unit_object(self.module);
                code.global_get(unit);
            }
            Some(ValType::Ref { .. }) => {}
            Some(value) => {
                let form = self.runtime.box_form(self.module, ty).ok_or_else(|| {
                    format!("a function value answering a `{}` value", describe(ty))
                })?;
                let scratch = form.needs_scratch().then(|| {
                    locals.push(value);
                    first + locals.len() as u32 - 1
                });
                form.emit_box(code, scratch);
            }
        }
        Ok(())
    }
}
