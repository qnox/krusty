//! Calls selected by checked common IR, realized through native symbols and dispatch.
//!
//! This module owns the call boundary: local/direct/virtual calls, compiler intrinsics, and the
//! mapping from a frozen dependency-callable fact to the native runtime operation that implements
//! it. Name-based runtime tables remain explicit migration debt inside `native::intrinsics`.

mod c_calls;
mod dependency_exports;

use super::*;
use crate::backend::class_tables::module_slots::PlanKey;

/// Whether an authenticated source-module dependency export can realize this call directly.
///
/// An absent symbol is not an error here: platform KLIB declarations have no preceding source
/// object and continue to their compiler-intrinsic/runtime realization below. Once an exact source
/// export exists, however, it owns the call. An omission cannot fall through to a name-based
/// realization, and the ordinary export cannot receive fewer operands than its declared ABI.
fn direct_dependency_export<'a>(
    export: Option<&'a super::super::DependencyExport>,
    defaults: &[u32],
) -> Result<Option<&'a super::super::DependencyExport>, Unsupported> {
    let Some(export) = export else {
        return Ok(None);
    };
    if !defaults.is_empty() {
        return Err(declined!(
            "a top-level dependency call with omitted arguments and no exported default bridge"
        ));
    }
    Ok(Some(export))
}

use crate::backend::{DependencyCallShape, DependencyExportCall};

#[cfg(test)]
fn dependency_export_call(
    is_top_level_function: bool,
    is_extension: bool,
    source_receiver: Option<Ty>,
    context_count: usize,
    emitted_params: &[Ty],
    call_params: &[Ty],
    extension_receiver_parameter: Option<u32>,
    has_dispatch_receiver: bool,
    unread_receiver: Option<Ty>,
    companion_receiver_position: Option<usize>,
    member_receiver: Option<Ty>,
    receiver_is_exact_singleton: bool,
) -> Result<DependencyExportCall, Unsupported> {
    DependencyCallShape {
        is_top_level_function,
        is_extension,
        source_receiver,
        context_count,
        emitted_params,
        call_params,
        extension_receiver_parameter,
        has_dispatch_receiver,
        unread_receiver,
        companion_receiver_position,
        member_receiver,
        receiver_is_exact_singleton,
    }
    .authenticate()
    .map_err(Unsupported::from)
}

/// Unit-test entry for shapes that have no retained ordinary member receiver. Keeping that frozen
/// ABI fact out of the source-extension slot prevents the two receiver roles from satisfying each
/// other's validation.
#[cfg(test)]
fn dependency_export_call_without_member(
    is_top_level_function: bool,
    is_extension: bool,
    source_receiver: Option<Ty>,
    context_count: usize,
    emitted_params: &[Ty],
    call_params: &[Ty],
    extension_receiver_parameter: Option<u32>,
    has_dispatch_receiver: bool,
    unread_receiver: Option<Ty>,
    companion_receiver_position: Option<usize>,
    receiver_is_exact_singleton: bool,
) -> Result<DependencyExportCall, Unsupported> {
    dependency_export_call(
        is_top_level_function,
        is_extension,
        source_receiver,
        context_count,
        emitted_params,
        call_params,
        extension_receiver_parameter,
        has_dispatch_receiver,
        unread_receiver,
        companion_receiver_position,
        None,
        receiver_is_exact_singleton,
    )
}

impl BodyLowering<'_, '_, '_> {
    /// A top-level function defined in another file of this module: an import of the symbol
    /// `module_function_symbol` and a direct call. The defining file exports that same symbol.
    fn module_call(
        &mut self,
        target: crate::fir::CallableId,
        params: &[Ty],
        ret: Ty,
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let symbol =
            super::super::super::symbols::module_function_symbol(self.file.module_name, target);
        if dispatch_receiver.is_some() {
            return Err(declined!("a cross-file call with a receiver (`{symbol}`)"));
        }
        let id = self.file.import(&symbol, params, ret)?;
        let arguments = self.arguments(args, params)?;
        if self.terminated {
            return Ok(None);
        }
        // Kotlin evaluates every operand before the invocation itself triggers initialization of
        // the declaring file. Running the initializer before `arguments` would reverse observable
        // side effects (and exception priority) between an argument and a top-level initializer.
        self.initialize_defining_file(target)?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// A member function of a class declared in another file of this module, where dispatch has
    /// only one body to select: an import of the member's `module_function_symbol` and a direct
    /// call, the receiver first. The declaring file defines that symbol for the member. A member
    /// an override could replace dispatches instead, and declines here.
    pub(super) fn module_member_call(
        &mut self,
        owner: TypeName,
        target: crate::fir::CallableId,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let Some(record) = self.file.ir.referenced_module_callables.get(&target) else {
            return Err(declined!(
                "a member call into `{}` with no module record",
                owner.render()
            ));
        };
        if record.owner != Some(owner) {
            // An inherited member sits at the number the plan gave its declaration, which every
            // table below the declaring class keeps.
            return self.planned_member_call(owner, target, receiver, args);
        }
        use crate::fir::DeclarationFlags as Flags;
        let overridable = record.flags.has(Flags::OPEN)
            || record.flags.has(Flags::ABSTRACT)
            || (record.flags.has(Flags::OVERRIDE) && !record.flags.has(Flags::FINAL));
        // An `object` has no subclass, so each of its members has exactly one body.
        let single_body = match record.owner_kind {
            Some(crate::ir::IrClassifierKind::Object) => true,
            Some(crate::ir::IrClassifierKind::Class) => {
                !overridable || self.file.values.is_value_class(owner)
            }
            _ => false,
        };
        if !single_body {
            return self.planned_member_call(owner, target, receiver, args);
        }
        // The receiver crosses as the declaring file's member takes it: a value class's member
        // takes the value, every other member the object.
        let receiver_ty = if self.file.values.is_value_class(owner) {
            Ty::obj_name(owner)
        } else {
            any()
        };
        let mut params = Vec::with_capacity(record.parameters.len() + 1);
        params.push(receiver_ty);
        params.extend(record.parameters.iter().copied());
        let ret = record.result;
        let symbol =
            super::super::super::symbols::module_function_symbol(self.file.module_name, target);
        let id = self.file.import(&symbol, &params, ret)?;
        let mut operands = Vec::with_capacity(args.len() + 1);
        operands.push(receiver);
        operands.extend_from_slice(args);
        let arguments = self.arguments(&operands, &params)?;
        if self.terminated {
            return Ok(None);
        }
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// A call dispatched through the table of `owner`, a class another file of the module
    /// declares: the slot is the one the module plan gave `target`, which the declaring file
    /// fills, and the call crosses with the declaration's carriers as every override's body takes
    /// them.
    fn planned_member_call(
        &mut self,
        owner: TypeName,
        target: crate::fir::CallableId,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let qualified = || owner.render().replace('/', ".");
        let numbered = self.file.slot_plan.interfaces();
        let through = if let Some(interface) = numbered.interface(owner) {
            // An interface of another file: its members are reached by their module-wide numbers,
            // which every implementor's interface table agrees on.
            if let Err(reason) = interface {
                return Err(declined!(
                    "a call through `{}`, which has no module-wide numbering: {reason}",
                    qualified()
                ));
            }
            match (
                numbered.fixed_slot(target),
                numbered.number(PlanKey::Callable(target)),
            ) {
                // A redeclared `kotlin.Any` member stands in its fixed class slot in every table.
                (Some(slot), _) => model::Dispatch::Class(slot),
                (None, Some(number)) => model::Dispatch::Interface(number),
                (None, None) => {
                    return Err(declined!(
                        "a call to a member `{}` does not number",
                        qualified()
                    ))
                }
            }
        } else {
            model::Dispatch::Class(self.planned_class_slot(owner, target)?)
        };
        let record = &self.file.ir.referenced_module_callables[&target];
        let params = record.parameters.to_vec();
        let ret = record.result;
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let arguments = self.arguments(args, &params)?;
        if self.terminated {
            return Ok(None);
        }
        self.dispatch_through(object, through, &params, ret, &arguments)
    }

    /// The slot the module plan gives `target` in the table of `owner`, a class of another file.
    fn planned_class_slot(
        &self,
        owner: TypeName,
        target: crate::fir::CallableId,
    ) -> Result<u32, Unsupported> {
        let qualified = || owner.render().replace('/', ".");
        let table = match self.file.slot_plan.table(owner) {
            Some(Ok(table)) => table,
            Some(Err(reason)) => {
                return Err(declined!(
                    "a virtual call through `{}`, which has no module-wide layout: {reason}",
                    qualified()
                ))
            }
            None => {
                return Err(declined!(
                    "a virtual call to a member of `{}`, which is not declared in this file",
                    qualified()
                ))
            }
        };
        table
            .slots
            .get(&PlanKey::Callable(target))
            .copied()
            .ok_or_else(|| declined!("a virtual call to a member `{}` does not plan", qualified()))
    }

    /// Run the defining file's top-level initializers before using anything it exports.
    pub(super) fn initialize_defining_file(
        &mut self,
        target: crate::fir::CallableId,
    ) -> Result<(), Unsupported> {
        let Some(record) = self.file.ir.referenced_module_callables.get(&target) else {
            return Err("a cross-file call with no recorded source file".into());
        };
        let init = super::super::super::symbols::file_init_symbol(
            self.file.module_name,
            record.source.source,
        );
        let id = self.file.import(&init, &[], Ty::Unit)?;
        if self.terminated {
            return Ok(());
        }
        let func_ref = self.func_ref(id);
        self.emit_call(func_ref, &[])?;
        Ok(())
    }

    pub(super) fn call(
        &mut self,
        site: u32,
        callee: &Callee,
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        match callee {
            Callee::LocalWithDefaults { function, defaults }
            | Callee::ClassStaticWithDefaults {
                function, defaults, ..
            } => self.defaulted_call(*function, defaults, dispatch_receiver, args, site),
            // A top-level function in another file: that file exports the entry that fills the
            // omitted arguments, because only it holds their defaults.
            Callee::ModuleWithDefaults {
                target,
                default_provider: crate::fir::ResolvedFunctionOverrideTarget::Module(provider),
                params,
                ret,
                defaults,
                dispatch_receiver_ty: None,
                ..
            } if dispatch_receiver.is_none() => {
                self.module_defaulted_call(*provider, *target, params, *ret, defaults, args)
            }
            Callee::ModuleWithDefaults { defaults, .. } => {
                match self.file.inherited_default_provider(callee) {
                    Some(provider) => {
                        self.defaulted_call(provider, defaults, dispatch_receiver, args, site)
                    }
                    None => Err(declined!("a {} call", callee_kind(callee))),
                }
            }
            // A static method owned by a class is, to this generator, a function with a symbol —
            // the owner is a JVM placement fact, and there is no flat facade here for it to be
            // placed differently from. A local function declared inside a member is the shape that
            // arrives this way.
            Callee::Local(function) | Callee::ClassStatic { function, .. } => {
                if dispatch_receiver.is_some() {
                    return Err("a static call with a receiver".into());
                }
                let params =
                    super::super::super::captures::carried_parameters(self.file.ir, *function);
                let arguments = self.arguments(args, &params)?;
                if self.terminated {
                    return Ok(None);
                }
                let Some(id) = self.file.functions[*function as usize] else {
                    return Err(declined!(
                        "a call to `{}`, which has no body",
                        self.file.ir.functions[*function as usize].name
                    ));
                };
                let func_ref = self.func_ref(id);
                let call = self.emit_call(func_ref, &arguments)?;
                Ok(self.builder.inst_results(call).first().copied())
            }
            Callee::Super {
                owner,
                name,
                kind,
                declaration,
                ..
            } => {
                let Some(receiver) = dispatch_receiver else {
                    return Err(declined!("a `super` call without a receiver (`{name}`)"));
                };
                let target = dispatch::SuperTarget {
                    owner: *owner,
                    name,
                    kind: *kind,
                    declaration: *declaration,
                };
                self.direct_call(target, receiver, args)
            }
            Callee::Virtual {
                owner,
                name,
                params,
                module_target,
                target,
                ..
            } => {
                let Some(receiver) = dispatch_receiver else {
                    return Err(declined!("a virtual call without a receiver (`{name}`)"));
                };
                if let (None, Some(target)) =
                    (self.file.ir.class_id_by_name(*owner), *module_target)
                {
                    return self.module_member_call(*owner, target, receiver, args);
                }
                self.virtual_call(*owner, name, params.as_ref(), *target, receiver, args)
            }
            Callee::Special {
                owner,
                name,
                source,
                ..
            } => {
                let Some(receiver) = dispatch_receiver else {
                    return Err(declined!("a `super` call without a receiver (`{name}`)"));
                };
                // A diamond `super.f()` to a superinterface's DEFAULT METHOD. `Callee::Special`
                // carries no accessor kind because it never names one.
                let target = dispatch::SuperTarget {
                    owner: *owner,
                    name,
                    kind: crate::ir::IrSuperCallKind::Function,
                    declaration: source.map(crate::fir::ResolvedFunctionOverrideTarget::Module),
                };
                self.direct_call(target, receiver, args)
            }
            Callee::Intrinsic { operation, ret } => {
                self.intrinsic(*operation, *ret, dispatch_receiver, args)
            }
            Callee::External {
                target,
                params,
                ret,
                defaults,
                extension_receiver_parameter,
                substitutions,
                ..
            } => {
                let Some(realization) = self.file.callables.callable(*target) else {
                    return Err("an unresolvable dependency call".into());
                };
                // The target stdlib's own `typeOf` body only says it is a compiler intrinsic.
                if realization.compiler_intrinsic
                    == Some(crate::backend::BackendCompilerIntrinsic::TypeOf)
                {
                    return self.type_of(*target, substitutions);
                }
                let export = realization
                    .dependency_callable
                    .as_ref()
                    .and_then(|identity| self.file.dependency_symbols.get(identity));
                if let Some(export) = direct_dependency_export(export, defaults)? {
                    return self.emitted_dependency_call(
                        site,
                        export,
                        realization,
                        callee,
                        dispatch_receiver,
                        args,
                    );
                }
                if let Some(function) = &realization.direct_c_function {
                    return self.direct_c_call(
                        function.symbol(),
                        params,
                        *ret,
                        defaults,
                        dispatch_receiver,
                        args,
                    );
                }
                // A callable reference reaches this ordinary dependency-call path, while retaining
                // the exact compiler intrinsic the source-form operation uses.
                // A member's operation travels as its realization; a package function's as its
                // intrinsic. Either is the same exact provider fact.
                if let Some(realized) = self.compiler_intrinsic_call(
                    site,
                    realization
                        .compiler_intrinsic
                        .or(realization.member_realization.compiler_intrinsic())
                        .or_else(|| self.unsigned_array_carrier_access(realization)),
                    dispatch_receiver,
                    args,
                    *extension_receiver_parameter,
                    *ret,
                ) {
                    return realized;
                }
                let owner = super::super::super::intrinsics::DeclarationOwner::callable(
                    realization.physical_owner,
                    realization.declaration_owner,
                );
                // The Kotlin name the declaration PUBLISHES, not the spelling it is realized under.
                // A physical name is an emit handle: a JVM realization may RENAME a member, and
                // where the signature mentions a value class kotlinc appends a hash of the erasure
                // (`UInt.compareTo` is realized as `compareTo-WZ4Q5Ns`). Neither is recoverable
                // from the spelling, and neither has to be: the contract carries the Kotlin name
                // beside it. Where it does not, the member declines rather than being guessed at.
                let name = realization
                    .reflection_name
                    .clone()
                    .unwrap_or_else(|| realization.name.clone());
                // Intrinsic identity is the declaration's signature, not the types produced by
                // call-site substitution. `compareTo<String>(...)`, for example, still names the
                // declaration whose parameter is `T`; using the specialized `params`/`ret` here
                // would make one declaration acquire a different intrinsic key at every call.
                let generic_signature = realization.generic_sig.as_deref();
                let declared_receiver = generic_signature
                    .and_then(|signature| signature.receiver)
                    .or(realization.source_receiver);
                let declared_params = if let Some(signature) = generic_signature {
                    std::borrow::Cow::Borrowed(signature.params.as_slice())
                } else {
                    let parameters = realization
                        .declared_params
                        .as_deref()
                        .unwrap_or(&realization.params);
                    if realization.is_extension() && parameters.len() > realization.context_count {
                        let mut parameters = parameters.to_vec();
                        parameters.remove(realization.context_count);
                        std::borrow::Cow::Owned(parameters)
                    } else {
                        std::borrow::Cow::Borrowed(parameters)
                    }
                };
                let declared_ret = generic_signature
                    .map(|signature| signature.ret)
                    .or(realization.declared_ret)
                    .unwrap_or(realization.ret);
                let signature = super::super::super::intrinsics::FunctionSignature::new(
                    owner,
                    &name,
                    declared_params.as_ref(),
                    declared_ret,
                )
                .with_receiver(declared_receiver)
                .with_overridden(&realization.overridden_declarations);
                match dispatch_receiver {
                    // A member: the receiver is the runtime function's first argument, and
                    // everything crosses as a reference.
                    Some(receiver) => {
                        // A receiver typed by a RUNTIME-KNOWN type this file puts a class of its
                        // own behind may be one of those objects, and every table below answers
                        // only for the ones the runtime MAKES. No static type tells the two apart
                        // — that is why those answers are the runtime's at all — so the member
                        // declines by name, with the type it was asked of still in sight.
                        if let Some(internal) = self
                            .checked_type(receiver)
                            .map(Ty::non_null)
                            .and_then(|ty| ty.obj_internal())
                        {
                            if self.file.implements_dependency(internal)
                                && !super::super::super::intrinsics::is_any_runtime_member(
                                    signature,
                                )
                            {
                                if let Some(realized) = self.implemented_member(
                                    internal, *target, signature, receiver, args, *ret,
                                ) {
                                    return realized;
                                }
                                if let Some(realized) =
                                    self.iteration_member(signature, receiver, args, *ret)
                                {
                                    return realized;
                                }
                                if let Some(realized) = self
                                    .collection_algorithm_member(signature, receiver, args, *ret)
                                {
                                    return realized;
                                }
                                if super::classifier_shapes::is_read_only_property(internal) {
                                    if let Some(realized) = self.read_only_property_member(
                                        internal, *target, signature, receiver, args, *ret,
                                    ) {
                                        return realized;
                                    }
                                }
                                return Err(declined!(
                                    "the member `{}.{name}` of a type this file implements itself",
                                    internal.render().replace('/', ".")
                                ));
                            }
                        }
                        // `x.isNaN()` and its two siblings are one comparison each. Realizing them
                        // here rather than in the runtime keeps the operand unboxed — the member
                        // path below crosses everything as a reference, which for a `Double` would
                        // mean allocating a box to ask a question about its bits.
                        if let Some(predicate) =
                            super::super::super::intrinsics::float_predicate(owner, &name)
                        {
                            return self.float_predicate(predicate, receiver);
                        }
                        if let Some(realized) = self.range_member(
                            signature,
                            realization.compiler_intrinsic,
                            receiver,
                            args,
                            *ret,
                        ) {
                            return realized;
                        }
                        // The unsigned integers: a value class the erasure made look like the
                        // signed number sharing its bits, so every member where that difference
                        // shows is answered on purpose rather than by the signed instruction.
                        if let Some(element) = self.checked_type(receiver) {
                            if let Some(realized) =
                                self.unsigned_member(element, &name, params, *ret, receiver, args)
                            {
                                return realized;
                            }
                        }
                        if let Some(realized) =
                            self.reference_member(realization.semantic_role, receiver, args, *ret)
                        {
                            return realized;
                        }
                        if let Some(realized) =
                            self.reference_delegate(realization.semantic_role, receiver, args, *ret)
                        {
                            return realized;
                        }
                        if let Some(realized) =
                            self.iteration_member(signature, receiver, args, *ret)
                        {
                            return realized;
                        }
                        if let Some(realized) =
                            self.collection_algorithm_member(signature, receiver, args, *ret)
                        {
                            return realized;
                        }
                        if let Some(realized) = self.list_member_declared(
                            signature,
                            receiver,
                            args,
                            *ret,
                            &realization.physical_params,
                        ) {
                            return realized;
                        }
                        if let Some(realized) = self.map_member(signature, receiver, args, *ret) {
                            return realized;
                        }
                        if let Some(realized) = self.pair_construction(signature, receiver, args) {
                            return realized;
                        }
                        // `x++` where `x` is an `Int?`: the member is the primitive's, and so is
                        // the value, whatever it arrived carried as.
                        if let Some(realized) = self.boxed_step(owner, &name, params, receiver) {
                            return realized;
                        }
                        // `42.toUInt()`: a SIGNED receiver converted to an unsigned type.
                        // Nothing is called — Kotlin defines the conversion as the ordinary signed
                        // one to the target's width with those bits reinterpreted, and this
                        // backend already carries an unsigned value as the machine integer it
                        // wraps, so the reinterpretation is not an operation at all.
                        //
                        // The receiver is read at ITS OWN type, which is what makes the answer
                        // right: `convert` resizes by the SOURCE's signedness, so a negative
                        // `Int` widening to `ULong` sign-extends (kotlinc answers
                        // 18446744073709551615, not 4294967295) while a wide source narrowing
                        // truncates. Only an integer source is taken; a float one saturates
                        // instead and `unsigned_conversion` says why it is not here.
                        if let Some(target) =
                            super::super::super::intrinsics::unsigned_conversion(owner, &name)
                        {
                            let source = self.checked_type(receiver).map(Ty::non_null);
                            if let Some(source @ (Ty::Byte | Ty::Short | Ty::Int | Ty::Long)) =
                                source
                            {
                                let Some(value) = self.coerce(receiver, source)? else {
                                    return Ok(None);
                                };
                                if self.terminated {
                                    return Ok(None);
                                }
                                let Some(produced) = self.convert(value, Some(source), target)?
                                else {
                                    return Ok(None);
                                };
                                return self.convert(produced, Some(target), *ret);
                            }
                        }
                        // `a.mod(b)`: the remainder brought onto the DIVISOR's sign. Both
                        // operands are read at the width the declaration answers in, which is what
                        // makes `Int.mod(Long)` a `Long` question rather than a truncated one.
                        let receiver_type = self.checked_type(receiver).map(Ty::non_null);
                        if let Some((symbol, operand)) = receiver_type.and_then(|receiver_type| {
                            super::super::super::intrinsics::floor_mod(
                                owner,
                                &name,
                                receiver_type,
                                params,
                            )
                        }) {
                            let [argument] = args else {
                                return Err("a `mod` with more than one operand".into());
                            };
                            let Some(left) = self.coerce(receiver, operand)? else {
                                return Err("a `Unit` receiver for `mod`".into());
                            };
                            let Some(right) = self.coerce(*argument, operand)? else {
                                return Err("a `Unit` operand for `mod`".into());
                            };
                            if self.terminated {
                                return Ok(None);
                            }
                            let produced = self.runtime_call(
                                symbol,
                                &[operand, operand],
                                operand,
                                &[left, right],
                            )?;
                            let Some(produced) = produced else {
                                return Ok(None);
                            };
                            return self.convert(produced, Some(operand), *ret);
                        }
                        // `kotlin.experimental`'s bit operations on the narrow integers. Kotlin
                        // gives `Int` and `Long` the same four as members and these as extensions,
                        // which is where the library put them rather than a difference in what
                        // they mean — so they are instructions here, not a call, which is also why
                        // they are not in `scalar_member`: that table boxes its receiver.
                        if let Some(op) = super::super::super::intrinsics::experimental_bitwise(
                            owner, &name, params,
                        ) {
                            return self.experimental_bitwise(op, receiver, args, *ret);
                        }
                        if let Some((symbol, carried, answer)) =
                            super::super::super::intrinsics::array_member(owner, &name, params)
                        {
                            if self
                                .checked_type(receiver)
                                .map(Ty::non_null)
                                .is_some_and(Ty::is_array)
                            {
                                let mut operands = vec![self.reference(receiver)?];
                                for argument in args {
                                    operands.push(self.reference(*argument)?);
                                }
                                if self.terminated {
                                    return Ok(None);
                                }
                                let produced =
                                    self.runtime_call(symbol, &carried, answer, &operands)?;
                                let Some(produced) = produced else {
                                    return Ok(None);
                                };
                                return self.convert(produced, Some(answer), *ret);
                            }
                        }
                        // `s.startsWith(t)`, `s.endsWith(t)` and `t in s`, whose last parameter
                        // is Kotlin's `ignoreCase`. The default reaches here as a CONSTANT
                        // argument rather than as an absent one, so the case-sensitive form — the
                        // only one the runtime answers — is recognizable right here: a literal
                        // `false` and nothing else. Anything else asks about Unicode case folding,
                        // which the runtime holds no table for, and declines below by name.
                        if let Some(symbol) =
                            super::super::super::intrinsics::case_sensitive_text_member(
                                owner, &name, params,
                            )
                        {
                            // `ignoreCase` has a DEFAULT, and the two providers hand that over
                            // differently: a klib call materializes the default as a constant
                            // argument, a jar call leaves the argument out. Both mean the same
                            // thing — the case-sensitive form — and reading the argument list
                            // rather than the signature is what makes them the same answer.
                            let sensitive = match args.len() {
                                given if given + 1 == params.len() => true,
                                given if given == params.len() => args.last().is_some_and(|flag| {
                                    matches!(
                                        self.file.ir.expr(*flag),
                                        IrExpr::Const(IrConst::Boolean(false))
                                    )
                                }),
                                _ => false,
                            };
                            if sensitive {
                                let arguments =
                                    vec![self.reference(receiver)?, self.reference(args[0])?];
                                if self.terminated {
                                    return Ok(None);
                                }
                                let produced = self.runtime_call(
                                    symbol,
                                    &[any(), any()],
                                    Ty::Boolean,
                                    &arguments,
                                )?;
                                let Some(produced) = produced else {
                                    return Ok(None);
                                };
                                return self.convert(produced, Some(Ty::Boolean), *ret);
                            }
                        }
                        // `x.compareTo(y)` where the static type says only `Comparable`. The
                        // receiver's DESCRIPTOR says what to compare, exactly as `equals` and
                        // `toString` on such a receiver already read it — a boxed primitive at its
                        // own width, with Kotlin's TOTAL order for the floating ones, and a string
                        // by UTF-16 unit. A file that declares a `Comparable` of its own keeps
                        // declining: an object of the program's could stand behind that type and
                        // the runtime has no order for it.
                        if super::super::super::intrinsics::is_comparable_compare_to(signature)
                            && !self.file.declares_its_own_comparable
                        {
                            let operands =
                                vec![self.reference(receiver)?, self.reference(args[0])?];
                            if self.terminated {
                                return Ok(None);
                            }
                            let produced = self.runtime_call(
                                "kt_compare_any",
                                &[any(), any()],
                                Ty::Int,
                                &operands,
                            )?;
                            let Some(produced) = produced else {
                                return Ok(None);
                            };
                            return self.convert(produced, Some(Ty::Int), *ret);
                        }
                        // A member that asks about a NUMBER rather than an object, carried as one:
                        // `s[i]` must not box its index to reach the runtime.
                        if let Some((symbol, carried, answer)) =
                            super::super::super::intrinsics::scalar_member(owner, &name, params)
                        {
                            let mut arguments = vec![self.reference(receiver)?];
                            for (argument, ty) in args.iter().zip(&carried[1..]) {
                                let Some(value) = self.coerce(*argument, *ty)? else {
                                    return Ok(None);
                                };
                                arguments.push(value);
                            }
                            if self.terminated {
                                return Ok(None);
                            }
                            let produced =
                                self.runtime_call(symbol, &carried, answer, &arguments)?;
                            // The table says what the runtime function PHYSICALLY answers; the
                            // call node says what the site expects. Reconciling the two is this
                            // boundary's job rather than something to assume: they are different
                            // sources and nothing here made them agree.
                            //
                            // They disagree today for `Number.toByte`/`toShort`, which the
                            // provider provider types `Int` because `desc_to_ty` reads the JVM
                            // descriptors `B` and `S` as `Int` — a core defect with its own fix,
                            // invisible to the JVM backend because a byte and an int share a stack
                            // slot there. Without this, the i8 the runtime answers reaches a box
                            // helper that takes an i32 and Cranelift's verifier rejects the
                            // function. When the provider is fixed the coercion becomes a no-op.
                            let Some(produced) = produced else {
                                return Ok(None);
                            };
                            return self.convert(produced, Some(answer), *ret);
                        }
                        // A companion member the runtime realizes takes its arguments alone:
                        // the receiver is an object carrying nothing, and it is not evaluated.
                        if let Some(symbol) =
                            super::super::super::intrinsics::runtime_companion_member(
                                owner, &name, params,
                            )
                        {
                            let mut arguments = Vec::with_capacity(args.len());
                            for argument in args {
                                arguments.push(self.reference(*argument)?);
                            }
                            if self.terminated {
                                return Ok(None);
                            }
                            let signature = vec![any(); arguments.len()];
                            let produced =
                                self.runtime_call(symbol, &signature, any(), &arguments)?;
                            let Some(produced) = produced else {
                                return Ok(None);
                            };
                            return self.convert(produced, Some(any()), *ret);
                        }
                        let Some(symbol) =
                            super::super::super::intrinsics::runtime_member(signature)
                        else {
                            return Err(declined!(
                                "the member `{}.{name}`",
                                realization.physical_owner.render().replace('/', ".")
                            ));
                        };
                        let mut arguments = vec![self.reference(receiver)?];
                        for argument in args {
                            arguments.push(self.reference(*argument)?);
                        }
                        if self.terminated {
                            return Ok(None);
                        }
                        let signature = vec![any(); arguments.len()];
                        self.runtime_call(symbol, &signature, *ret, &arguments)
                    }
                    None => {
                        let packs_a_vararg = realization
                            .physical_params
                            .first()
                            .is_some_and(|ty| ty.non_null().is_reference_array());
                        if let Some(realized) =
                            self.map_construction(signature, packs_a_vararg, args)
                        {
                            return realized;
                        }
                        if let Some(realized) = self.lazy_construction(signature, args) {
                            return realized;
                        }
                        if let Some(operation) =
                            compiler_intrinsics::console_intrinsic(realization.compiler_intrinsic)
                        {
                            let Some(symbol) = super::super::super::intrinsics::console_intrinsic(
                                operation, params,
                            ) else {
                                return Err(declined!("a malformed `{operation:?}` call"));
                            };
                            let arguments = self.arguments(args, params)?;
                            if self.terminated {
                                return Ok(None);
                            }
                            return self.runtime_call(&symbol, params, *ret, &arguments);
                        }
                        // `kotlin.test`'s assertions. Their operands cross as REFERENCES rather
                        // than at their own widths: `assertEquals` is generic, so a call with
                        // `Int` arguments arrives typed `Int`, and the comparison Kotlin makes is
                        // `==` on whatever the values are.
                        if let Some((symbol, compared)) =
                            super::super::super::intrinsics::assertion_call(owner, &name, params)
                        {
                            return self.assertion(symbol, compared, args);
                        }
                        if super::super::super::intrinsics::is_assert_fails_with(
                            owner, &name, params,
                        ) {
                            return self.assert_fails_with(args, params, *ret);
                        }
                        if let Some(builder) =
                            super::super::super::intrinsics::builder_scope(owner, &name, params)
                        {
                            return self.builder_scope_function(builder, args);
                        }
                        // `require`, `check`, `requireNotNull`, `checkNotNull`, `error`. Not a
                        // runtime call: the message block runs only when the check fails, so the
                        // shape is a branch around a raise rather than a call with operands.
                        if let Some(precondition) =
                            super::super::super::intrinsics::precondition(owner, &name, params)
                        {
                            return self.precondition(precondition, args, *ret);
                        }
                        if let Some(symbol) =
                            super::super::super::intrinsics::progression_last_element(signature)
                        {
                            let arguments = self.arguments(args, params)?;
                            if self.terminated {
                                return Ok(None);
                            }
                            return self.runtime_call(symbol, params, *ret, &arguments);
                        }
                        let Some(symbol) =
                            super::super::super::intrinsics::runtime_function(signature)
                        else {
                            return Err(declined!(
                                "the declaration `{}.{name}` ({}; receiver {:?}; parameters {:?}; \
                                 result {:?})",
                                realization.physical_owner.render().replace('/', "."),
                                signature.owner.diagnostic_name(),
                                signature.receiver,
                                signature.params,
                                signature.ret,
                            ));
                        };
                        let arguments = self.arguments(args, params)?;
                        if self.terminated {
                            return Ok(None);
                        }
                        self.runtime_call(&symbol, params, *ret, &arguments)
                    }
                }
            }
            Callee::Module {
                target,
                params,
                ret,
                ..
            } => self.module_call(*target, params, *ret, dispatch_receiver, args),
            // A JVM realization may already have rewritten the same edge. The callable id is still
            // the symbol; the facade name is not.
            Callee::CrossFile {
                module_target: Some(target),
                params,
                ret,
                module_default_call: false,
                ..
            } => self.module_call(*target, params, *ret, dispatch_receiver, args),
            other => Err(declined!("a {} call", callee_kind(other))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        dependency_export_call as dependency_export_call_with_member,
        dependency_export_call_without_member as dependency_export_call, direct_dependency_export,
        DependencyExportCall,
    };
    use crate::native::codegen::DependencyExport;
    use crate::types::Ty;

    #[test]
    fn an_instantiated_dependency_call_cannot_change_its_parameter_types() {
        for (top_level, member, dispatch) in [
            (true, None, false),
            (false, Some(Ty::obj("sample/Owner")), true),
        ] {
            assert_eq!(
                dependency_export_call_with_member(
                    top_level,
                    false,
                    None,
                    0,
                    &[Ty::String],
                    &[Ty::Int],
                    None,
                    dispatch,
                    None,
                    None,
                    member,
                    false
                )
                .expect_err("checked instantiation fixes every operand, not just its carrier")
                .construct(),
                "an emitted dependency callable whose checked parameter types changed"
            );
        }
    }

    #[test]
    fn an_exact_dependency_export_never_receives_too_few_arguments() {
        let export = DependencyExport {
            symbol: "kt_klib_export_0".to_string(),
            signature: {
                let mut ir = crate::ir::IrFile::default();
                let function = ir.add_fun(crate::ir::IrFunction {
                    name: "sample".to_owned(),
                    params: vec![Ty::Int],
                    ret: Ty::Int,
                    param_checks: vec![None],
                    body: None,
                    is_static: true,
                    dispatch_receiver: None,
                });
                crate::backend::DependencyFunctionSignature::from_ir(&ir, function, None).unwrap()
            },
            member_receiver: None,
            singleton_member_receiver: None,
            singleton_extension_receiver: None,
        };
        assert_eq!(
            direct_dependency_export(Some(&export), &[])
                .expect("a full call uses the authenticated export"),
            Some(&export)
        );
        assert_eq!(
            direct_dependency_export(Some(&export), &[0])
                .expect_err("an omission needs a provider-owned default bridge")
                .construct(),
            "a top-level dependency call with omitted arguments and no exported default bridge"
        );
        assert_eq!(
            direct_dependency_export(None, &[0])
                .expect("a platform declaration may have an authenticated intrinsic realization"),
            None
        );
    }

    #[test]
    fn an_authenticated_singleton_member_uses_its_receiver_erased_export() {
        assert_eq!(
            dependency_export_call(
                false,
                false,
                None,
                0,
                &[Ty::Int],
                &[Ty::Int],
                None,
                true,
                None,
                None,
                true,
            )
            .expect("the exact per-call singleton fact authenticates the receiver-erased export"),
            DependencyExportCall::SingletonMember,
        );
        assert_eq!(
            dependency_export_call(
                false,
                false,
                None,
                0,
                &[Ty::Int],
                &[Ty::Int],
                None,
                true,
                None,
                None,
                false,
            )
            .expect_err("a member receiver cannot be erased without the exact per-call fact")
            .construct(),
            "an emitted dependency callable whose checked receiver shape changed",
        );
        assert_eq!(
            dependency_export_call_with_member(
                false,
                false,
                None,
                0,
                &[Ty::Int],
                &[Ty::Int],
                None,
                true,
                None,
                None,
                Some(Ty::obj("demo/Member")),
                false,
            )
            .expect("an ordinary member requires its independently exported dispatch receiver"),
            DependencyExportCall::Member,
        );
    }

    #[test]
    fn an_extension_export_keeps_its_provider_recorded_receiver_coordinate() {
        let emitted = [Ty::String, Ty::Float, Ty::Int];
        let source_call = [Ty::String, Ty::Int];
        assert_eq!(
            dependency_export_call(
                false,
                true,
                Some(Ty::String),
                1,
                &[Ty::Int, Ty::String, Ty::Float],
                &[Ty::Int, Ty::Float],
                None,
                true,
                Some(Ty::String),
                None,
                true,
            )
            .expect("an exact unread singleton receiver gets an inert ABI slot"),
            DependencyExportCall::SourceSingletonExtension {
                receiver: Ty::String,
                receiver_position: 1,
            }
        );
        assert_eq!(
            dependency_export_call(
                false,
                true,
                Some(Ty::String),
                1,
                &[Ty::Int, Ty::String, Ty::Float],
                &[Ty::Int, Ty::Float],
                None,
                true,
                Some(Ty::Float),
                None,
                true,
            )
            .expect("a mismatched body effect retains ordinary receiver evaluation"),
            DependencyExportCall::SourceExtension {
                receiver: Ty::String,
                receiver_position: 1,
            }
        );
        assert_eq!(
            dependency_export_call(
                false,
                true,
                Some(Ty::String),
                1,
                &[Ty::Int, Ty::String, Ty::Float],
                &[Ty::Int, Ty::Float],
                None,
                true,
                Some(Ty::String),
                None,
                false,
            )
            .expect("a non-singleton expression retains ordinary receiver evaluation"),
            DependencyExportCall::SourceExtension {
                receiver: Ty::String,
                receiver_position: 1,
            }
        );
        assert_eq!(
            dependency_export_call(
                false,
                true,
                Some(Ty::Float),
                1,
                &emitted,
                &source_call,
                None,
                true,
                None,
                None,
                false,
            )
            .expect("source receiver is inserted after its context"),
            DependencyExportCall::SourceExtension {
                receiver: Ty::Float,
                receiver_position: 1,
            }
        );
        assert_eq!(
            dependency_export_call(
                false,
                true,
                Some(Ty::Float),
                1,
                &emitted,
                &emitted,
                Some(1),
                false,
                None,
                None,
                false,
            )
            .expect("serialized body already carries the receiver"),
            DependencyExportCall::SerializedExtension,
        );
        assert_eq!(
            dependency_export_call(
                false,
                true,
                Some(Ty::Float),
                1,
                &emitted,
                &source_call,
                None,
                false,
                None,
                None,
                false,
            )
            .expect_err("an extension cannot silently become top-level")
            .construct(),
            "an emitted dependency callable whose checked receiver shape changed",
        );
    }

    #[test]
    fn a_member_extension_export_keeps_both_receiver_roles() {
        let params = [Ty::String, Ty::Int];

        assert_eq!(
            dependency_export_call_with_member(
                false,
                false,
                Some(Ty::String),
                0,
                &params,
                &params,
                Some(0),
                true,
                None,
                None,
                Some(Ty::obj("kotlin/Any")),
                false,
            )
            .expect("the class dispatch receiver and extension receiver stay independent"),
            DependencyExportCall::MemberExtension,
        );
    }

    #[test]
    fn a_classifier_associated_export_requires_both_body_and_target_authority() {
        let source_call = [Ty::Int];
        assert_eq!(
            dependency_export_call(
                true,
                false,
                None,
                0,
                &source_call,
                &source_call,
                None,
                false,
                Some(Ty::String),
                Some(0),
                false,
            )
            .expect("the two frozen contracts authorize the retained ABI slot"),
            DependencyExportCall::ClassifierAssociated {
                receiver: Ty::String,
                receiver_position: 0,
            }
        );
        assert_eq!(
            dependency_export_call(
                true,
                false,
                None,
                0,
                &source_call,
                &source_call,
                None,
                false,
                Some(Ty::String),
                None,
                false,
            )
            .expect_err("an export effect alone cannot change the selected ABI")
            .construct(),
            "a classifier-associated dependency receiver without matching body authority",
        );
        assert_eq!(
            dependency_export_call(
                true,
                false,
                None,
                0,
                &source_call,
                &source_call,
                None,
                false,
                None,
                Some(0),
                false,
            )
            .expect_err("a body contract alone cannot invent a target ABI effect")
            .construct(),
            "a classifier-associated dependency receiver without matching body authority",
        );
    }

    #[test]
    fn a_serialized_classifier_associated_call_keeps_its_retained_receiver() {
        let source_declaration = [Ty::Int];
        let serialized_call = [Ty::String, Ty::Int];
        assert_eq!(
            dependency_export_call(
                true,
                false,
                None,
                0,
                &source_declaration,
                &serialized_call,
                Some(0),
                false,
                Some(Ty::String),
                Some(0),
                false,
            )
            .expect("the serialized call already supplies the authenticated ABI receiver"),
            DependencyExportCall::SerializedClassifierAssociated {
                receiver: Ty::String,
            }
        );
        assert_eq!(
            dependency_export_call(
                true,
                false,
                None,
                0,
                &source_declaration,
                &[Ty::Float, Ty::Int],
                Some(0),
                false,
                Some(Ty::String),
                Some(0),
                false,
            )
            .expect_err("a serialized receiver type cannot disagree with its emitted effect")
            .construct(),
            "a classifier-associated dependency receiver whose declaration disagrees",
        );
        assert_eq!(
            dependency_export_call(
                true,
                false,
                None,
                0,
                &source_declaration,
                &serialized_call,
                Some(1),
                false,
                Some(Ty::String),
                Some(0),
                false,
            )
            .expect_err("the serialized receiver coordinate is exact")
            .construct(),
            "a classifier-associated dependency receiver whose declaration disagrees",
        );
    }
}
