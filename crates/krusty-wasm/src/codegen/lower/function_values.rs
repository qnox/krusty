//! Creating and calling function values (see `crate::function_values`).
//!
//! Each site that creates one, a lambda or a callable reference, gets its own struct, holding the
//! captures after the dispatch table, its own table and the entry that table names. The entry
//! unpacks the captures, converts each argument from the reference the caller handed over to the
//! type the body declares, calls the body and hands its result back as a reference.

use super::*;
use crate::encode::{Code, CompositeType, SubType, ValType};
use crate::function_values::INVOKE_FIELD;
use crate::objects::immutable;
use krusty::types::TyKind;

/// The continuation object's context, after the dispatch table and the identity hash.
const CONTEXT_FIELD: u32 = crate::identity::PREFIX_FIELDS;
/// The continuation object's `resumeWith` function.
const RESUME_FIELD: u32 = crate::identity::PREFIX_FIELDS + 1;

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
    /// arguments. With `sam`, the value is also an instance of that `fun interface`, whose member
    /// it implements.
    pub(super) fn function_value(
        &mut self,
        function: FunId,
        captures: &[u32],
        equality: FunctionEquality,
        sam: Option<&krusty::ir::IrSamTarget>,
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
        let (record, itable) =
            objects
                .interfaces
                .class(self.module, objects.itable, vec![structure]);
        if let Some(target) = sam {
            self.implement_sam(record, invoke, target)?;
        }
        let mut init = Code::default();
        for function in [equals, hash_code, to_string] {
            self.module.reference_function(function);
            init.ref_func(function);
        }
        self.module.reference_function(invoke);
        init.global_get(itable);
        // A function value is no class a literal names: reading its class traps.
        self.runtime.objects.push_type_info(&mut init, None);
        init.ref_func(invoke).struct_new(shape.table);
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
            None,
        )
    }

    /// Make the function value of `record`, whose boxed entry is `invoke`, an instance of the
    /// `fun interface` `target` names: its member forwards to the entry, boxing each argument and
    /// unboxing the result to the member's own carriers.
    fn implement_sam(
        &mut self,
        record: crate::interface_dispatch::ClassRef,
        invoke: u32,
        target: &krusty::ir::IrSamTarget,
    ) -> Result<(), Unsupported> {
        if target.suspend {
            return Err("a suspend functional interface".into());
        }
        let krusty::ir::IrSamMethod::Module(method) = target.method_target else {
            return Err("a functional interface declared outside this module".into());
        };
        // The object implements only the abstract member. An interface default it would inherit
        // has no body this object could stand on yet.
        let interface = self
            .ir
            .class_id_by_name(target.classifier)
            .ok_or("a functional interface declared outside this module")?;
        let declares_default = self.ir.classes[interface as usize]
            .methods
            .iter()
            .any(|&function| self.ir.functions[function as usize].body.is_some());
        if declares_default
            || self
                .ir
                .inherited_defaults
                .get(&target.classifier)
                .is_some_and(|defaults| !defaults.is_empty())
        {
            return Err("a functional interface with default members".into());
        }
        let (member, params, result) = self.sam_member(method)?;
        let mut code = Code::default();
        let mut locals = Vec::new();
        code.local_get(0);
        for (index, &param) in params.iter().enumerate() {
            if carrier(param)?.is_none() {
                return Err("a functional interface member taking a `Unit` value".into());
            }
            code.local_get(index as u32 + 1);
            self.pack_result(&mut code, &mut locals, params.len() as u32 + 1, param)?;
        }
        code.call(invoke);
        if carrier(result)?.is_none() {
            code.drop_();
        } else {
            self.unpack_argument(&mut code, result)?;
        }
        let ty = self.runtime.objects.interfaces.ty(member);
        let thunk = self.module.declare();
        self.module.define(thunk, Function { ty, locals, code });
        let interfaces = &mut self.runtime.objects.interfaces;
        interfaces.implement(record, member, thunk)?;
        let hierarchy = self
            .ir
            .classifier_hierarchies
            .get(&target.classifier)
            .into_iter()
            .flatten()
            .map(|applied| applied.classifier);
        for interface in std::iter::once(target.classifier).chain(hierarchy) {
            interfaces.instance_of(record, interface);
        }
        Ok(())
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

    /// `receiver.startCoroutine(completion)` for a `suspend () -> T` value that takes no arguments.
    ///
    /// The receiver runs first, then the completion. The suspend lambda's implementation takes
    /// that completion as its trailing argument and returns `Any?`; the result is then delivered
    /// to the completion. A lambda whose body actually suspends declines where that suspension is
    /// lowered. Another shape declines rather than calling the serialized body.
    pub(super) fn parameterless_start_coroutine(
        &mut self,
        site: u32,
        node: &IrExpr,
    ) -> Option<Result<(), Unsupported>> {
        let IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } = node
        else {
            return None;
        };
        let Callee::External {
            target,
            params,
            ret,
            defaults,
            extension_receiver_parameter,
            ..
        } = &**callee
        else {
            return None;
        };
        if self.callables.callable(*target).is_none_or(|fact| {
            fact.compiler_operation() != Some(krusty::libraries::CompilerIntrinsic::StartCoroutine)
        }) {
            return None;
        }
        let Some(receiver) = *dispatch_receiver else {
            return Some(Err("a startCoroutine of another shape".into()));
        };
        if !defaults.is_empty()
            || extension_receiver_parameter.is_some()
            || *ret != Ty::Unit
            || params.len() != 1
            || args.len() != 1
        {
            return Some(Err("a startCoroutine of another shape".into()));
        }
        let Some(ty) = self
            .type_of(receiver)
            .ok()
            .flatten()
            .or_else(|| self.ir.ext_call_source_receiver.get(&site).copied())
        else {
            return Some(Err("a startCoroutine without a function type".into()));
        };
        if !parameterless_suspend(ty) {
            return Some(Err("a startCoroutine of another function".into()));
        }
        let completion = args[0];
        match self.foreign_completion(completion) {
            Ok(true) => {
                return Some(Err(
                    "a startCoroutine completion that is not the factory continuation".into(),
                ));
            }
            Ok(false) => {}
            Err(error) => return Some(Err(error)),
        }
        Some((|| {
            self.expression(receiver)?;
            let work = self.local(REFERENCE);
            self.code.local_set(work);
            self.expression(completion)?;
            let completion = self.local(REFERENCE);
            self.code.local_set(completion);
            // The lambda's result is what the completion receives: one that suspends declines
            // where that suspension is lowered.
            let any = Ty::obj_name(krusty::types::wk::any());
            self.call_function_value(work, completion, any)?;
            let result = self.local(REFERENCE);
            self.code.local_set(result);
            self.deliver_completion(completion, result)
        })())
    }

    /// `Continuation(context, resumeWith)`. The context runs, then the completion function. The
    /// result is an object that retains both and forwards `resumeWith` to that function.
    pub(super) fn continuation_factory(
        &mut self,
        node: &IrExpr,
    ) -> Option<Result<(), Unsupported>> {
        let IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } = node
        else {
            return None;
        };
        let Callee::External {
            target,
            params,
            defaults,
            extension_receiver_parameter,
            ..
        } = &**callee
        else {
            return None;
        };
        if self.callables.callable(*target).is_none_or(|fact| {
            fact.compiler_operation()
                != Some(krusty::libraries::CompilerIntrinsic::ContinuationFactory)
        }) {
            return None;
        }
        if dispatch_receiver.is_some()
            || !defaults.is_empty()
            || extension_receiver_parameter.is_some()
            || params.len() != 2
            || args.len() != 2
        {
            return Some(Err("a Continuation of another shape".into()));
        }
        let [context, resume] = args.as_slice() else {
            return Some(Err("a Continuation of another shape".into()));
        };
        Some((|| {
            let (structure, table) = self.continuation_items()?;
            self.continuation_operand(*context)?;
            let context = self.local(REFERENCE);
            self.code.local_set(context);
            self.continuation_operand(*resume)?;
            let resume = self.local(REFERENCE);
            self.code.local_set(resume);
            self.code.global_get(table);
            self.code.call(self.runtime.identity.allocate);
            self.code.local_get(context);
            self.code.local_get(resume);
            self.code.struct_new(structure);
            Ok(())
        })())
    }

    /// `c.context` and `c.resumeWith` on the factory's continuation.
    pub(super) fn continuation_member(&mut self, node: &IrExpr) -> Option<Result<(), Unsupported>> {
        match node {
            IrExpr::Checked(operation) => self.continuation_property(operation),
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } => self.continuation_call(callee, *dispatch_receiver, args),
            _ => None,
        }
    }

    /// Evaluate a factory operand through the ordinary expression path, so a dependency object is
    /// the instance its module exports.
    fn continuation_operand(&mut self, id: u32) -> Result<(), Unsupported> {
        self.expression(id)
    }

    /// Call the one-argument function value stored in `function` with the reference stored in
    /// `argument`.
    fn call_function_value(
        &mut self,
        function: u32,
        argument: u32,
        ret: Ty,
    ) -> Result<(), Unsupported> {
        let shape = self.runtime.function_shape(self.module, 1);
        self.code.local_get(function).local_get(argument);
        let object = self.runtime.objects.object;
        self.code
            .local_get(function)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, 0)
            .ref_cast(false, HeapType::Concrete(shape.table))
            .struct_get(shape.table, INVOKE_FIELD)
            .call_ref(shape.invoke);
        self.coerce_emitted_value(Ty::obj_name(krusty::types::wk::any()), ret)
    }

    /// Invoke the `resumeWith` function stored on the factory's continuation.
    ///
    /// A value whose static type is `Continuation` may still be another object. The cast is a test:
    /// a different object, including `null`, skips the call. A failing cast would trap, and reading
    /// the field of that object would not.
    fn deliver_completion(&mut self, completion: u32, result: u32) -> Result<(), Unsupported> {
        let (structure, _) = self.continuation_items()?;
        self.code
            .local_get(completion)
            .ref_test(false, HeapType::Concrete(structure));
        self.code.if_(BlockType::Empty);
        let resume = self.local(REFERENCE);
        self.code
            .local_get(completion)
            .ref_cast(false, HeapType::Concrete(structure))
            .struct_get(structure, RESUME_FIELD)
            .local_set(resume);
        self.call_function_value(resume, result, Ty::Unit)?;
        self.code.end();
        Ok(())
    }

    pub(super) fn continuation_items(&mut self) -> Result<(u32, u32), Unsupported> {
        if let Some(items) = self.runtime.library_objects.continuation {
            return Ok(items);
        }
        let any_table = self.runtime.objects.any_table;
        let parent = self.runtime.identity.object;
        let identity_equals = self.runtime.objects.identity_equals;
        let hash_code = self.runtime.identity.hash_code;
        let itable_type = self.runtime.objects.itable;
        let [_, _, to_string_ty] = self.runtime.objects.any_slot_types;
        // `toString` is the trap a lambda object uses. `hashCode` is that object's identity hash.
        let to_string = self.runtime.objects.trap(self.module, to_string_ty);
        let structure = self.module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(any_table)),
                immutable(ValType::I32),
                immutable(REFERENCE),
                immutable(REFERENCE),
            ]),
            supertype: Some(parent),
            open: false,
        });
        let (class, itable) =
            self.runtime
                .objects
                .interfaces
                .class(self.module, itable_type, vec![structure]);
        for owner in self
            .callables
            .continuation_factory()
            .names()
            .into_iter()
            .flatten()
        {
            self.runtime.objects.interfaces.instance_of(class, owner);
        }
        let mut init = Code::default();
        for function in [identity_equals, hash_code, to_string] {
            self.module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable).struct_new(any_table);
        let table = self
            .module
            .constant_global(ValType::reference(any_table), init);
        self.runtime.library_objects.continuation = Some((structure, table));
        Ok((structure, table))
    }

    fn continuation_property(
        &mut self,
        operation: &krusty::ir::IrCheckedOperation,
    ) -> Option<Result<(), Unsupported>> {
        let krusty::ir::IrCheckedOperation::ExternalPropertyRead {
            target,
            receiver,
            arguments,
            parameters,
            ..
        } = operation
        else {
            return None;
        };
        if !arguments.is_empty() || !parameters.is_empty() {
            return None;
        }
        let owners = self.callables.continuation_factory();
        let owner = self.callables.property(*target)?.owner;
        if !owners.declares(owner) {
            return None;
        }
        let Some(receiver) = *receiver else {
            return Some(Err("a continuation context without a receiver".into()));
        };
        Some(self.load_continuation_field(receiver, CONTEXT_FIELD))
    }

    fn continuation_call(
        &mut self,
        callee: &Callee,
        receiver: Option<u32>,
        args: &[u32],
    ) -> Option<Result<(), Unsupported>> {
        let Callee::External {
            target,
            ret,
            defaults,
            extension_receiver_parameter,
            ..
        } = callee
        else {
            return None;
        };
        let owners = self.callables.continuation_factory();
        let (declared, interface_member, parameters, result) = {
            let fact = self.callables.callable(*target)?;
            (
                fact.declaration_owner,
                fact.owner_is_interface,
                fact.params.len(),
                fact.ret,
            )
        };
        let owner = match declared {
            Some(krusty::types::SemanticCallableOwner::Classifier(name)) => Some(name),
            _ => None,
        };
        let on_continuation = owner.is_some_and(|owner| owners.declares(owner));
        let plain =
            interface_member && defaults.is_empty() && extension_receiver_parameter.is_none();
        if !(on_continuation && plain) {
            return None;
        }
        if parameters == 0 && args.is_empty() {
            let Some(receiver) = receiver else {
                return Some(Err("a continuation context without a receiver".into()));
            };
            return Some(self.load_continuation_field(receiver, CONTEXT_FIELD));
        }
        if parameters == 1 && args.len() == 1 && result == Ty::Unit && *ret == Ty::Unit {
            return Some(self.emit_continuation_resume(receiver, args[0]));
        }
        None
    }

    fn load_continuation_field(&mut self, receiver: u32, field: u32) -> Result<(), Unsupported> {
        let (structure, _) = self.continuation_items()?;
        self.expression(receiver)?;
        self.code
            .ref_cast(false, HeapType::Concrete(structure))
            .struct_get(structure, field);
        Ok(())
    }

    fn emit_continuation_resume(
        &mut self,
        receiver: Option<u32>,
        argument: u32,
    ) -> Result<(), Unsupported> {
        let Some(receiver) = receiver else {
            return Err("a continuation resumeWith without a receiver".into());
        };
        let (structure, _) = self.continuation_items()?;
        self.expression(receiver)?;
        let function = self.local(REFERENCE);
        self.code
            .ref_cast(false, HeapType::Concrete(structure))
            .struct_get(structure, RESUME_FIELD)
            .local_set(function);
        let any = Ty::obj_name(krusty::types::wk::any());
        let argument_ty = self.type_of(argument)?.unwrap_or(any);
        self.value(argument, argument_ty)?;
        self.coerce_emitted_value(argument_ty, any)?;
        let argument = self.local(REFERENCE);
        self.code.local_set(argument);
        let shape = self.runtime.function_shape(self.module, 1);
        let object = self.runtime.objects.object;
        self.code.local_get(function).local_get(argument);
        self.code
            .local_get(function)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, 0)
            .ref_cast(false, HeapType::Concrete(shape.table))
            .struct_get(shape.table, INVOKE_FIELD)
            .call_ref(shape.invoke);
        self.coerce_emitted_value(any, Ty::Unit)
    }

    /// A completion whose value is known not to be the object this factory allocates.
    fn foreign_completion(&self, id: u32) -> Result<bool, Unsupported> {
        let id = value_under_casts(self.ir, id);
        if factory_continuation(self, id) {
            return Ok(false);
        }
        if let IrExpr::New { internal, .. } = self.ir.expr(id) {
            return Ok(!self.callables.continuation_factory().declares(*internal));
        }
        let Some(ty) = self.type_of(id)? else {
            return Ok(false);
        };
        Ok(ty
            .non_null()
            .obj_internal()
            .is_some_and(|name| !self.callables.continuation_factory().declares(name)))
    }

    /// One stable instance of a dependency object whose published declaration stores nothing.
    pub(super) fn inert_instance(
        &mut self,
        classifier: krusty::types::TypeName,
    ) -> Result<u32, Unsupported> {
        if let Some(global) = self
            .runtime
            .library_objects
            .inert_objects
            .get(&classifier)
            .copied()
        {
            return Ok(global);
        }
        let (structure, table) = self.inert_layout()?;
        // A constant global cannot call the allocator. The hash is still per instance and stable.
        let hash = self.runtime.library_objects.inert_objects.len() as i32 + 1;
        let mut init = Code::default();
        init.global_get(table).i32_const(hash).struct_new(structure);
        let global = self
            .module
            .constant_global(ValType::reference(structure), init);
        self.runtime
            .library_objects
            .inert_objects
            .insert(classifier, global);
        Ok(global)
    }

    fn inert_layout(&mut self) -> Result<(u32, u32), Unsupported> {
        if let Some(layout) = self.runtime.library_objects.inert_layout {
            return Ok(layout);
        }
        let any_table = self.runtime.objects.any_table;
        let parent = self.runtime.identity.object;
        let identity_equals = self.runtime.objects.identity_equals;
        let hash_code = self.runtime.identity.hash_code;
        let itable_type = self.runtime.objects.itable;
        let [_, _, to_string_ty] = self.runtime.objects.any_slot_types;
        let to_string = self.runtime.objects.trap(self.module, to_string_ty);
        let structure = self.module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(any_table)),
                immutable(ValType::I32),
            ]),
            supertype: Some(parent),
            open: false,
        });
        let (_, itable) =
            self.runtime
                .objects
                .interfaces
                .class(self.module, itable_type, vec![structure]);
        let mut init = Code::default();
        for function in [identity_equals, hash_code, to_string] {
            self.module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable).struct_new(any_table);
        let table = self
            .module
            .constant_global(ValType::reference(any_table), init);
        self.runtime.library_objects.inert_layout = Some((structure, table));
        Ok((structure, table))
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
            Some(value) => {
                let form = self
                    .runtime
                    .box_form(self.module, ty, value)?
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
                let form = self
                    .runtime
                    .box_form(self.module, ty, value)?
                    .ok_or_else(|| {
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

fn parameterless_suspend(ty: Ty) -> bool {
    let TyKind::Fun(signature) = ty.non_null().kind() else {
        return false;
    };
    signature.suspend
        && signature.params.is_empty()
        && signature.context_count == 0
        && !signature.has_receiver
}

/// The expression under representation casts. A cast to the completion's static type does not
/// make a different object into the factory's.
fn value_under_casts(ir: &IrFile, mut id: u32) -> u32 {
    loop {
        match ir.expr(id) {
            IrExpr::TypeOp {
                op:
                    IrTypeOp::ImplicitCoercion
                    | IrTypeOp::Cast
                    | IrTypeOp::CastNonNull
                    | IrTypeOp::SafeCast,
                arg,
                ..
            } => {
                id = *arg;
            }
            _ => return id,
        }
    }
}

/// Whether `id` is a call of the continuation factory.
fn factory_continuation(body: &Body<'_>, id: u32) -> bool {
    let IrExpr::Call { callee, .. } = body.ir.expr(id) else {
        return false;
    };
    let Callee::External { target, .. } = &**callee else {
        return false;
    };
    body.callables.callable(*target).is_some_and(|fact| {
        fact.compiler_operation() == Some(krusty::libraries::CompilerIntrinsic::ContinuationFactory)
    })
}
