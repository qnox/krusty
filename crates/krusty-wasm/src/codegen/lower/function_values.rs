//! Creating and calling function values (see `crate::function_values`).
//!
//! Each site that creates one, a lambda or a callable reference, gets its own struct, holding the
//! captures after the dispatch table, its own table and the entry that table names. The entry
//! unpacks the captures, converts each argument from the reference the caller handed over to the
//! type the body declares, calls the body and hands its result back as a reference.

use super::*;
use crate::encode::{CompositeType, SubType};
use crate::function_values::INVOKE_FIELD;
use crate::objects::immutable;

/// Why a lambda or callable reference converted to a `fun interface` declines: SAM conversion
/// has no Wasm representation yet.
pub(super) const SAM_CONVERSION: &str = "a lambda converted to a `fun interface`";

/// What `==` on a function value compares.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FunctionEquality {
    /// A lambda is equal only to itself.
    Identity,
    /// The checked declaration/adaptation kind and the semantic receiver's capture slot.
    Declaration { kind: i32, receiver: Option<usize> },
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
        if self
            .ir
            .shared_capture_parameters
            .keys()
            .any(|&(owner, parameter)| owner == function && parameter as usize >= captures.len())
        {
            return Err("a function value with a shared parameter outside its captures".into());
        }
        let body = self.emitted_function(function)?;
        let shape = self.runtime.function_shape(self.module, own.len());

        let mut fields = vec![
            immutable(ValType::reference(self.runtime.objects.any_table)),
            immutable(ValType::I32),
        ];
        let (parent, first_capture) = match equality {
            FunctionEquality::Identity => {
                (self.runtime.identity.object, crate::identity::PREFIX_FIELDS)
            }
            FunctionEquality::Declaration { receiver, .. } => {
                if receiver.is_some_and(|slot| slot >= captures.len()) {
                    return Err("a callable reference with an invalid bound receiver slot".into());
                }
                fields.push(immutable(REFERENCE));
                (
                    self.runtime.callable_references.object,
                    crate::callable_references::PREFIX_FIELDS,
                )
            }
        };
        let mut captured_types = Vec::with_capacity(captures.len());
        for parameter in 0..captures.len() {
            let value = parameter_carrier(self.module, self.ir, function, parameter as u32)?
                .ok_or("a function value capturing a `Unit` value")?;
            captured_types.push(value);
            fields.push(immutable(value));
        }
        let structure = self.module.subtype(SubType {
            composite: CompositeType::Struct(fields),
            supertype: Some(parent),
            open: false,
        });

        // The entry: `(this, arguments…) -> result`, every operand a reference.
        let mut entry = Code::default();
        let mut locals = Vec::new();
        for field in 0..captures.len() as u32 {
            entry
                .local_get(0)
                .ref_cast(false, HeapType::Concrete(structure))
                .struct_get(structure, field + first_capture);
        }
        for (index, &param) in own.iter().enumerate() {
            entry.local_get(index as u32 + 1);
            self.unpack_argument(&mut entry, param)?;
        }
        entry.call(body);
        let scratch = own.len() as u32 + 1;
        self.pack_result(&mut entry, &mut locals, scratch, ret)?;
        let invoke = self.module.declare();
        self.module.define(
            invoke,
            Function {
                ty: shape.invoke,
                locals,
                code: entry,
            },
        );

        let [_, _, to_string_ty] = self.runtime.objects.any_slot_types;
        let equals = match equality {
            FunctionEquality::Identity => self.runtime.objects.identity_equals,
            FunctionEquality::Declaration { .. } => self.runtime.callable_references.equals,
        };
        let hash_code = match equality {
            FunctionEquality::Identity => self.runtime.identity.hash_code,
            FunctionEquality::Declaration { .. } => self.runtime.callable_references.hash_code,
        };
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

        // Ordinary captures precede the bound receiver. Save each once so equality metadata
        // and the adapter's physical receiver reuse the same evaluated value.
        let mut saved = Vec::with_capacity(captures.len());
        for (parameter, &capture) in captures.iter().enumerate() {
            let local = self.local(captured_types[parameter]);
            self.function_argument(function, parameter as u32, capture)?;
            self.code.local_set(local);
            saved.push(local);
        }
        self.code.global_get(table);
        match equality {
            FunctionEquality::Identity => {
                self.code.call(self.runtime.identity.allocate);
            }
            FunctionEquality::Declaration { kind, receiver } => {
                self.code.i32_const(kind);
                if let Some(slot) = receiver {
                    self.code.local_get(saved[slot]);
                    self.coerce_emitted_value(
                        params[slot],
                        Ty::obj_name(krusty::types::wk::any()),
                    )?;
                } else {
                    self.code.ref_null(HeapType::None);
                }
            }
        }
        for local in saved {
            self.code.local_get(local);
        }
        self.code.struct_new(structure);
        Ok(())
    }

    /// A new function value for a callable reference. Its adapter takes the referenced
    /// declaration's captures, then the bound receiver, then the caller's arguments.
    pub(super) fn callable_reference(
        &mut self,
        reference: &krusty::ir::IrCallableReference,
    ) -> Result<(), Unsupported> {
        let captures: Vec<u32> = reference
            .captures
            .iter()
            .copied()
            .chain(reference.bound_receiver)
            .collect();
        let kind = self.callable_reference_kind(reference)?;
        self.function_value(
            reference.adapter,
            &captures,
            FunctionEquality::Declaration {
                kind,
                receiver: reference.bound_receiver.map(|_| reference.captures.len()),
            },
        )
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
        let any = Ty::obj_name(krusty::types::wk::any());
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

    /// `value as F` for a function type `F`: the value, which must be a function value invoked
    /// with `F`'s arguments. A suspend function type is invoked with its caller's continuation as
    /// one more argument, so `suspend () -> R` and `(Continuation<R>) -> Any?` share a shape, as
    /// kotlinc's `SuspendFunction0` is a `Function1`. `null` passes a nullable target.
    pub(super) fn function_cast(
        &mut self,
        arg: u32,
        target: Ty,
        non_null: bool,
    ) -> Result<(), Unsupported> {
        let TyKind::Fun(signature) = target.non_null().kind() else {
            return Err("a function cast to a type that is not a function type".into());
        };
        let arity = signature.params.len() + usize::from(signature.suspend);
        let shape = self.runtime.function_shape(self.module, arity);
        let object = self.runtime.objects.object;
        self.value(arg, Ty::obj_name(krusty::types::wk::any()))?;
        let value = self.local(REFERENCE);
        self.code.local_tee(value);
        if !non_null {
            self.code.ref_is_null().if_(BlockType::Empty).else_();
            self.code.local_get(value);
        }
        self.code
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, 0)
            .ref_test(false, HeapType::Concrete(shape.table))
            .if_(BlockType::Empty)
            .else_()
            .unreachable()
            .end();
        if !non_null {
            self.code.end();
        }
        self.code.local_get(value);
        Ok(())
    }

    /// Convert the reference on top of `code`'s stack to the carrier of `ty`.
    fn unpack_argument(&mut self, code: &mut Code, ty: Ty) -> Result<(), Unsupported> {
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
    fn pack_result(
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
