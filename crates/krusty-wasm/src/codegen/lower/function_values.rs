//! Creating and calling function values (see `crate::function_values`).
//!
//! Each site that creates one, a lambda or a callable reference, gets its own struct, holding the
//! captures after the dispatch table, its own table and the entry that table names. The entry
//! unpacks the captures, converts each argument from the reference the caller handed over to the
//! type the body declares, calls the body and hands its result back as a reference.

use super::*;
use crate::callable_references::{AccessorShape, PropertyAccess};
use crate::encode::{CompositeType, SubType};
use crate::function_value_names;
use crate::objects::immutable;

/// The class of a new function value, the entry its table names and how many operands that entry
/// takes after the value itself.
pub(super) struct FunctionValueClass {
    pub(super) class: crate::interface_dispatch::ClassRef,
    pub(super) entry: u32,
    pub(super) arity: usize,
    pub(super) captures: CaptureLayout,
}

/// Where a function value's struct holds its captures, which another entry over the same value
/// unpacks the same way.
#[derive(Clone)]
pub(super) struct CaptureLayout {
    structure: u32,
    first: u32,
    carriers: Vec<ValType>,
}

/// What `==` on a function value compares.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FunctionEquality {
    /// A lambda is equal only to itself.
    Identity,
    /// The checked declaration/adaptation kind and the semantic receiver's capture slot.
    Declaration { kind: i32, receiver: Option<usize> },
}

impl Body<'_> {
    /// A new function value, created by the expression `site`, whose body is `function`, carrying
    /// `captures` as its leading arguments. With `sam`, the value is also an instance of that
    /// `fun interface`, whose member it implements. Answers the class the value is an instance of
    /// and the entry its table names.
    pub(super) fn function_value(
        &mut self,
        site: u32,
        function: FunId,
        captures: &[u32],
        equality: FunctionEquality,
        sam: Option<&krusty::ir::IrSamTarget>,
    ) -> Result<FunctionValueClass, Unsupported> {
        let instance = self.type_of(site)?;
        let declaration = self
            .ir
            .functions
            .get(function as usize)
            .ok_or("a function value with an invalid body identity")?;
        if declaration.dispatch_receiver.is_some() {
            return Err("a function value whose body is a member".into());
        }
        let params = declaration.params.clone();
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
                fields.push(immutable(ValType::I32));
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

        let layout = CaptureLayout {
            structure,
            first: first_capture,
            carriers: captured_types.clone(),
        };
        let invoke = self.function_value_entry(function, &layout)?;

        let [_, _, to_string_ty] = self.runtime.objects.any_slot_types;
        let equals = match equality {
            FunctionEquality::Identity => self.runtime.objects.identity_equals,
            FunctionEquality::Declaration { .. } => self.runtime.callable_references.equals,
        };
        let hash_code = match equality {
            FunctionEquality::Identity => self.runtime.identity.hash_code,
            FunctionEquality::Declaration { .. } => self.runtime.callable_references.hash_code,
        };
        let to_string = match function_value_names::named_shape(self.ir.expr(site), instance) {
            Some(shape) => {
                let codes: Vec<String> = params[..captures.len()]
                    .iter()
                    .enumerate()
                    .map(|(parameter, &ty)| {
                        let shared = self
                            .ir
                            .shared_capture_parameters
                            .contains_key(&(function, parameter as u32));
                        function_value_names::capture_code(ty, shared, self.classifiers)
                    })
                    .collect();
                let name = function_value_names::class_name(shape, &codes);
                self.runtime.function_value_to_string(self.module, &name)
            }
            None => match function_value_names::sam_class_name(self.ir, site) {
                Some(name) => self.runtime.function_value_to_string(self.module, &name),
                // kotlinc-wasm names this value's class otherwise, or prints it otherwise.
                None => self.runtime.objects.trap(self.module, to_string_ty),
            },
        };
        let callable = sam.is_none_or(|target| {
            classifier_and_supertypes(self.classifiers, target.classifier)
                .into_iter()
                .any(|classifier| self.function_arity(Ty::obj_name(classifier)) == Some(own.len()))
        });
        let suspend = sam.map_or_else(
            || self.ir.suspend_funs.contains(&function),
            |target| {
                classifier_and_supertypes(self.classifiers, target.classifier)
                    .into_iter()
                    .any(|classifier| self.is_suspend_function_type(Ty::obj_name(classifier)))
            },
        );
        let objects = &mut self.runtime.objects;
        let (class, itable) =
            objects
                .interfaces
                .class(self.module, objects.itable, vec![structure]);
        if callable {
            objects
                .interfaces
                .function_instance_of(class, own.len(), suspend);
            objects
                .interfaces
                .implement(class, shape.protocol, invoke)?;
        }
        // A value of a classifier type (`KProperty0<Int>`) is an instance of every classifier
        // above it, whose members it implements with nothing yet.
        if let Some(TyKind::Obj(classifier, _)) = instance.map(|ty| ty.non_null().kind()) {
            for supertype in classifier_and_supertypes(self.classifiers, classifier) {
                self.runtime
                    .objects
                    .interfaces
                    .instance_of(class, supertype);
            }
        }
        if let Some(target) = sam {
            self.implement_sam(class, invoke, target)?;
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
                self.code
                    .call(self.runtime.identity.allocate)
                    .i32_const(kind);
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
        Ok(FunctionValueClass {
            class,
            entry: invoke,
            arity: own.len(),
            captures: layout,
        })
    }

    /// The entry calling `function` on a function value whose captures `layout` places:
    /// `(this, arguments…) -> result`, every operand a reference. It unpacks the captures as
    /// `function`'s leading arguments, converts each other argument to the type `function`
    /// declares, calls it and hands its result back as a reference.
    fn function_value_entry(
        &mut self,
        function: FunId,
        layout: &CaptureLayout,
    ) -> Result<u32, Unsupported> {
        let declaration = self
            .ir
            .functions
            .get(function as usize)
            .ok_or("a function value with an invalid body identity")?;
        let own = declaration
            .params
            .get(layout.carriers.len()..)
            .ok_or("a function value capturing more than its body takes")?
            .to_vec();
        let ret = declaration.ret;
        for (parameter, &carrier) in layout.carriers.iter().enumerate() {
            if parameter_carrier(self.module, self.ir, function, parameter as u32)? != Some(carrier)
            {
                return Err("a function value entry whose captures differ from its value's".into());
            }
        }
        let body = self.emitted_function(function)?;
        let shape = self.runtime.function_shape(self.module, own.len());
        let mut entry = Code::default();
        let mut locals = Vec::new();
        for field in 0..layout.carriers.len() as u32 {
            entry
                .local_get(0)
                .ref_cast(false, HeapType::Concrete(layout.structure))
                .struct_get(layout.structure, field + layout.first);
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
        Ok(invoke)
    }

    /// A new function value for the callable reference `id`. Its adapter takes the referenced
    /// declaration's captures, then the bound receiver, then the caller's arguments.
    pub(super) fn callable_reference(
        &mut self,
        id: u32,
        reference: &krusty::ir::IrCallableReference,
    ) -> Result<(), Unsupported> {
        let captures: Vec<u32> = reference
            .captures
            .iter()
            .copied()
            .chain(reference.bound_receiver)
            .collect();
        let kind = self.callable_reference_kind(reference)?;
        let FunctionValueClass {
            class,
            entry,
            arity,
            captures: layout,
        } = self.function_value(
            id,
            reference.adapter,
            &captures,
            FunctionEquality::Declaration {
                kind,
                receiver: reference.bound_receiver.map(|_| reference.captures.len()),
            },
            None,
        )?;
        // A reference answers `KCallable.name` with its declaration's name, whatever type the
        // program reads it through. One that cannot say its name gets no `name` member.
        if let Some(name) = krusty::backend::reference_names::callable_reference_name(
            self.ir,
            self.callables,
            reference,
        ) {
            self.runtime.reference_name(self.module, class, &name)?;
        }
        // A `KProperty` value reads its property through `get` as its entry does. A mutable one
        // writes it through `set` with the entry of its write adapter, over the same captures.
        let krusty::ir::IrCallableReferenceTarget::Property(property) = &reference.target else {
            return Ok(());
        };
        if !property.reflective {
            return Ok(());
        }
        self.runtime.property_reference_accessor(
            self.module,
            class,
            AccessorShape {
                access: PropertyAccess::Read,
                operands: arity,
            },
            entry,
        )?;
        if let Some(writer) = property.writer {
            let write = self.function_value_entry(writer, &layout)?;
            self.runtime.property_reference_accessor(
                self.module,
                class,
                AccessorShape {
                    access: PropertyAccess::Write,
                    operands: arity + 1,
                },
                write,
            )?;
        }
        Ok(())
    }

    /// Make the function value of `record`, whose boxed entry is `invoke`, an instance of the
    /// `fun interface` `target` names. The object implements every declaration of the abstract
    /// method's override family the checker froze, this module's and its dependencies' alike, each
    /// by a thunk to the entry boxing each argument and unboxing the result to that declaration's
    /// own carriers, so a call through any interface of the family dispatches to the value.
    fn implement_sam(
        &mut self,
        record: crate::interface_dispatch::ClassRef,
        invoke: u32,
        target: &krusty::ir::IrSamTarget,
    ) -> Result<(), Unsupported> {
        if target.suspend {
            return Err("a suspend functional interface".into());
        }
        // The object answers `equals`, `hashCode` and `toString` with the function value's own
        // members, and implements only the abstract method: no body of an interface default
        // stands on this object yet.
        if target.requires_object_dispatch {
            return Err("a functional interface whose method is a `kotlin.Any` member".into());
        }
        if !target.default_methods.is_empty() {
            return Err("a functional interface with default members".into());
        }
        if target.method_target == krusty::ir::IrSamMethod::FunctionTypeInvoke {
            return Err("a functional interface implementing a function type's `invoke`".into());
        }
        if target.dispatch_methods.is_empty() {
            return Err("a functional interface without its method family".into());
        }
        for frame in &target.dispatch_methods {
            match frame.method {
                Some(krusty::ir::IrSamMethod::Module(method)) => {
                    let (member, params, result) = self.sam_member(method)?;
                    if !self.runtime.objects.interfaces.implements(record, member) {
                        self.implement_sam_member(record, invoke, member, (&params, result))?;
                    }
                }
                Some(krusty::ir::IrSamMethod::External(method)) => {
                    self.implement_sam_dependency_member(record, invoke, method, frame)?;
                }
                // The function value's own entry is the function type's `invoke`.
                Some(krusty::ir::IrSamMethod::FunctionTypeInvoke) => {}
                None => return Err("a functional interface method without its declaration".into()),
            }
        }
        let interfaces = &mut self.runtime.objects.interfaces;
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

    /// Implement the dependency declaration `method` of the family, by its declared `frame`: a
    /// call through it reaches the value by the member's identity, not by this module's number.
    /// A library member's thunk this generator cannot write declines only a call through that
    /// member, so it is recorded on the member instead.
    fn implement_sam_dependency_member(
        &mut self,
        record: crate::interface_dispatch::ClassRef,
        invoke: u32,
        method: krusty::fir::ExternalCallableId,
        frame: &krusty::ir::IrSamMethodFrame,
    ) -> Result<(), Unsupported> {
        let fact = self
            .callables
            .sam_declaration(method)
            .ok_or("a functional interface method without its frozen declaration")?;
        let member = crate::codegen::interface_members::dependency_member(
            &mut self.runtime.objects.interfaces,
            self.module,
            fact,
            library_members::MemberRole::Function,
        )
        .ok_or("a functional interface method a dependency declares without a member")?;
        if self
            .runtime
            .objects
            .interfaces
            .implements(record, member.member)
        {
            return Ok(());
        }
        let implemented = self.implement_sam_member(
            record,
            invoke,
            member.member,
            (&frame.parameters, frame.result),
        );
        match implemented {
            Err(reason) if member.library => {
                self.runtime
                    .objects
                    .interfaces
                    .unimplementable(member.member, reason);
                Ok(())
            }
            implemented => implemented,
        }
    }

    /// A function of type `ty` implementing a member taking `params` and answering `result` by
    /// forwarding to the boxed entry `invoke`: each argument is boxed, and the answer unboxed to
    /// the member's own carrier.
    fn sam_thunk(
        &mut self,
        invoke: u32,
        params: &[Ty],
        result: Ty,
        ty: u32,
    ) -> Result<u32, Unsupported> {
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
        let thunk = self.module.declare();
        self.module.define(thunk, Function { ty, locals, code });
        Ok(thunk)
    }

    /// Implement `member`, declared as taking `params` and answering `result`, for the function
    /// value of `record` through a thunk to its boxed entry `invoke`.
    fn implement_sam_member(
        &mut self,
        record: crate::interface_dispatch::ClassRef,
        invoke: u32,
        member: crate::interface_dispatch::MemberRef,
        (params, result): (&[Ty], Ty),
    ) -> Result<(), Unsupported> {
        let (carried_params, carried_results) =
            crate::codegen::interface_members::member_signature(params, result)?;
        let ty = self.runtime.objects.interfaces.ty(member);
        if self.module.func_type(carried_params, carried_results) != ty {
            return Err(
                "a functional interface member a dependency declares with other carriers".into(),
            );
        }
        let thunk = self.sam_thunk(invoke, params, result, ty)?;
        self.runtime
            .objects
            .interfaces
            .implement(record, member, thunk)
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
        self.expression(function)?;
        for (&arg, &param) in args.iter().zip(params) {
            self.value(arg, param)?;
            self.coerce_emitted_value(param, any)?;
        }
        let stub = self
            .runtime
            .objects
            .interfaces
            .stub(self.module, shape.protocol);
        self.code.call(stub);
        self.coerce_emitted_value(any, ret)
    }

    /// `startCoroutineUninterceptedOrReturnIntrinsicN(f, inputs.., completion)`: `f`'s suspend
    /// entry, which takes its inputs and then its continuation, called with `completion` as that
    /// continuation. It answers what the entry answers: `COROUTINE_SUSPENDED` or the value.
    pub(super) fn start_coroutine_unintercepted_or_return(
        &mut self,
        args: &[u32],
        params: &[Ty],
    ) -> Result<(), Unsupported> {
        let (Some((&function, operands)), Some((_, operand_params))) =
            (args.split_first(), params.split_first())
        else {
            return Err("a coroutine start without its suspend function".into());
        };
        let any = Ty::nullable(Ty::obj_name(krusty::types::wk::any()));
        self.invoke_function(function, operands, operand_params, any)
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
        let test = self
            .runtime
            .objects
            .interfaces
            .function_test(self.module, arity);
        self.value(arg, Ty::obj_name(krusty::types::wk::any()))?;
        let value = self.local(REFERENCE);
        self.code.local_tee(value);
        if !non_null {
            self.code.ref_is_null().if_(BlockType::Empty).else_();
            self.code.local_get(value);
        }
        self.code
            .call(test)
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

    /// Physical callable arity from the checked function type or frozen classifier role.
    pub(super) fn function_arity(&self, ty: Ty) -> Option<usize> {
        match ty.non_null().kind() {
            TyKind::Fun(signature) => Some(signature.params.len() + usize::from(signature.suspend)),
            TyKind::Obj(classifier, _) => match self.classifiers.classifier(classifier)?.role {
                Some(krusty::types::ClassifierRole::FunctionOfArity(arity)) => {
                    Some(usize::from(arity))
                }
                Some(krusty::types::ClassifierRole::SuspendFunctionOfArity(arity)) => {
                    Some(usize::from(arity) + 1)
                }
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn is_suspend_function_type(&self, ty: Ty) -> bool {
        match ty.non_null().kind() {
            TyKind::Fun(signature) => signature.suspend,
            TyKind::Obj(classifier, _) => {
                self.classifiers.classifier(classifier).is_some_and(|fact| {
                    matches!(
                        fact.role,
                        Some(krusty::types::ClassifierRole::SuspendFunctionOfArity(_))
                    )
                })
            }
            _ => false,
        }
    }

    /// Safe casts and `is` of suspend functions additionally require semantic suspension
    /// membership. Explicit casts above retain Kotlin's physical FunctionN check.
    pub(super) fn function_type_test(
        &mut self,
        arity: usize,
        nullable: bool,
        requires_suspend: bool,
    ) {
        let value = self.local(REFERENCE);
        self.code.local_set(value);
        let interfaces = &mut self.runtime.objects.interfaces;
        let test = if requires_suspend {
            interfaces.suspend_function_test(self.module, arity)
        } else {
            interfaces.function_test(self.module, arity)
        };
        self.code.local_get(value).call(test);
        if nullable {
            self.code
                .local_get(value)
                .ref_is_null()
                .op(super::super::super::runtime::OR32);
        }
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

/// `classifier` and every classifier above it, each once.
fn classifier_and_supertypes(
    classifiers: &dyn krusty::backend::BackendClassifierSource,
    classifier: krusty::types::TypeName,
) -> Vec<krusty::types::TypeName> {
    let mut found = vec![classifier];
    let mut next = 0;
    while let Some(&current) = found.get(next) {
        next += 1;
        let Some(fact) = classifiers.classifier(current) else {
            continue;
        };
        for &supertype in &fact.supertypes {
            if !found.contains(&supertype) {
                found.push(supertype);
            }
        }
    }
    found
}
