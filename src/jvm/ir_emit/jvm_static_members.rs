//! `@JvmStatic` on source member functions.
//!
//! Common IR keeps a `@JvmStatic` function an ordinary member of its object or companion. Its only
//! input here is the resolved annotation identity `kotlin/jvm/JvmStatic` recorded on the
//! declaration (or on the module callable a sibling file calls). The JVM backend owns both
//! realizations kotlinc produces:
//!
//! - A member of a named `object` is itself a `static` method of the object. Its body keeps the
//!   common-IR receiver numbering; the receiver value reads the singleton's instance field, as
//!   kotlinc reads `INSTANCE` for `this`. A call `invokestatic`s it, evaluating and discarding only
//!   a receiver that can run code.
//! - A member of a companion stays an instance method of the companion, and every call keeps
//!   dispatching through the companion instance. The companion's classifier additionally declares a
//!   `static` bridge with the member's name, descriptor, signature and annotations, which loads the
//!   companion field and calls the member: the surface Java and frameworks see.

use super::*;

const JVM_STATIC: &str = "kotlin/jvm/JvmStatic";

fn declares_jvm_static(annotations: Option<&crate::ir::DeclarationAnnotations>) -> bool {
    annotations.is_some_and(|annotations| {
        annotations
            .applications()
            .any(|annotation| annotation.internal.matches(JVM_STATIC))
    })
}

/// The named object that declares `function` when the JVM realizes it as a static of that object.
pub(in crate::jvm) fn singleton_static_owner(ir: &IrFile, function: u32) -> Option<TypeName> {
    let declaration = ir.functions.get(function as usize)?;
    if declaration.is_static {
        return None;
    }
    let owner = declaration.dispatch_receiver?;
    let class = &ir.classes[ir.class_id_by_name(owner)? as usize];
    (object_static_storage(class)
        && class.methods.contains(&function)
        && declares_jvm_static(ir.function_annotations.get(&function)))
    .then_some(owner)
}

/// [`singleton_static_owner`] for a member another source file of this module declares.
pub(in crate::jvm) fn module_singleton_static_owner(
    ir: &IrFile,
    target: crate::fir::CallableId,
) -> Option<TypeName> {
    let callable = ir.referenced_module_callables.get(&target)?;
    let owner = callable.owner?;
    if callable.owner_kind != Some(crate::ir::IrClassifierKind::Object)
        || !callable
            .annotations
            .iter()
            .any(|annotation| annotation.identity.matches(JVM_STATIC))
    {
        return None;
    }
    // The declaring file publishes every object owner of a referenced member with its singleton
    // shape; a companion keeps its instance method.
    let classifier = ir
        .referenced_module_classifiers
        .get(&owner)
        .expect("a referenced object member publishes its owner's singleton shape");
    classifier.companion_owner.is_none().then_some(owner)
}

/// Whether `receiver` of a static object member's call runs no code beyond naming the object,
/// which the `invokestatic` initializes anyway.
pub(in crate::jvm) fn receiver_is_inert(ir: &IrFile, receiver: ExprId) -> bool {
    crate::ir::expr_runs_no_code(ir, receiver)
        || matches!(
            ir.expr(receiver),
            IrExpr::SingletonValue { .. }
                | IrExpr::StaticInstance { .. }
                | IrExpr::ExternalStaticInstance { .. }
        )
}

/// The companion members `outer` declares a static bridge for, in member order.
pub(in crate::jvm) fn companion_bridge_functions(ir: &IrFile, outer: &IrClass) -> Vec<u32> {
    let Some(companion) = outer
        .companion_class
        .and_then(|companion| ir.class_id_by_name(companion))
    else {
        return Vec::new();
    };
    ir.classes[companion as usize]
        .methods
        .iter()
        .copied()
        .filter(|&function| {
            !ir.functions[function as usize].is_static
                && declares_jvm_static(ir.function_annotations.get(&function))
        })
        .collect()
}

/// Emit the static bridges of `outer`'s `@JvmStatic` companion members.
///
/// Each forwards to the companion instance with its parameters and carries the member's generic
/// signature, nullability, parameter and declaration annotations (`@JvmStatic` included), as
/// kotlinc's `JvmStaticInCompanionLowering` copies them. It checks no parameter: the member it
/// calls does. Its only line is the member's end line, on the return.
pub(super) fn emit_companion_bridges(
    ir: &IrFile,
    outer: &IrClass,
    cw: &mut ClassWriter,
    env: &EmitEnv,
) {
    let functions = companion_bridge_functions(ir, outer);
    let Some(companion) = outer.companion_class.filter(|_| !functions.is_empty()) else {
        return;
    };
    let formatter = &JvmSignatureFormatter::new(ir, env);
    let interface = is_jvm_interface(outer);
    for function in functions {
        emit_companion_bridge(
            ir,
            CompanionBridge {
                outer: outer.fq_name,
                companion,
                interface,
                function,
            },
            cw,
            formatter,
            env,
        );
    }
}

/// Emit on `companion` the `access$<name>` accessors that the outer class's bridges of private
/// `@JvmStatic` members call. kotlinc adds them after the companion's other members, its marker
/// constructor included; one an ordinary outer call already planned is not repeated.
pub(super) fn emit_private_bridge_accessors(
    ir: &IrFile,
    companion: &IrClass,
    owner: &str,
    line: u32,
    cw: &mut ClassWriter,
) {
    let Some(outer) = ir
        .classes
        .iter()
        .find(|outer| outer.companion_class == Some(companion.fq_name))
    else {
        return;
    };
    for function in companion_bridge_functions(ir, outer) {
        let declaration = &ir.functions[function as usize];
        if !ir.method_visibility(function).is_private() {
            continue;
        }
        let parameters = jvm_function_params(ir, function);
        let mut accessor_parameters = vec![Ty::obj(owner)];
        accessor_parameters.extend(parameters);
        let descriptor = method_descriptor(&accessor_parameters, jvm_declared_ty(&declaration.ret));
        if cw.declares_method(&format!("access${}", declaration.name), &descriptor) {
            continue;
        }
        super::access_bridges::emit_private_member_access_bridge(
            ir, function, owner, cw, false, line,
        );
    }
}

struct CompanionBridge {
    outer: TypeName,
    companion: TypeName,
    interface: bool,
    function: u32,
}

fn emit_companion_bridge(
    ir: &IrFile,
    bridge: CompanionBridge,
    cw: &mut ClassWriter,
    formatter: &JvmSignatureFormatter<'_>,
    env: &EmitEnv,
) {
    let CompanionBridge {
        outer,
        companion,
        interface,
        function,
    } = bridge;
    let declaration = &ir.functions[function as usize];
    let name = declaration.name.as_str();
    let parameters = jvm_function_params(ir, function);
    let result = jvm_declared_ty(&env.override_results.physical_result(ir, function));
    let descriptor = declared_method_desc(ir, env.override_results, function);
    let signature = declared_method_signature(formatter, ir, env.override_results, function);
    let annotations = ir
        .function_annotations
        .get(&function)
        .cloned()
        .unwrap_or_default();
    let declared_nullability::DeclaredNullability {
        result: result_nullability,
        parameters: parameter_nullability,
    } = override_result_emission::declared_nullability(ir, env.override_results, function);
    let nullability_annotated =
        function_annotations::nullability_annotated(ir, function, &annotations);
    let parameter_annotations: &[crate::ir::DeclarationAnnotations] = ir
        .fn_param_annotations
        .get(&function)
        .map_or(&[], Vec::as_slice);
    let annotation_types = function_annotations::header_annotation_types(
        result_nullability,
        parameter_annotations,
        &parameter_nullability,
        nullability_annotated,
    );
    let annotation_types: Vec<&str> = annotation_types.iter().map(String::as_str).collect();
    let method_parameters = if env.java_parameters {
        crate::jvm::method_parameters::function(ir, function, &parameters, None)
    } else {
        Vec::new()
    };
    // kotlinc visits the header before the code, so its constants precede the body's.
    cw.reserve_method_pool_with_annotations(
        name,
        &descriptor,
        signature.as_deref(),
        &annotation_types,
        &annotations,
        &method_parameters,
    );

    let argument_words = parameters.iter().map(|ty| slot_words(*ty)).sum::<u16>();
    let mut code = CodeBuilder::new(argument_words);
    let companion_internal = companion.render();
    let field = cw.fieldref(
        &outer.render(),
        crate::jvm::companion_storage::field_name(outer, companion),
        &format!("L{companion_internal};"),
    );
    code.getstatic(field, 1);
    let mut slot = 0u16;
    for ty in &parameters {
        load(*ty, slot, &mut code);
        slot += slot_words(*ty);
    }
    if ir.method_visibility(function).is_private() {
        // The companion's private member is out of the outer class's reach: kotlinc calls the
        // companion's static `access$<name>` accessor, which `emit_private_bridge_accessors` adds.
        let accessor_descriptor = format!("(L{companion_internal};{}", &descriptor[1..]);
        let accessor = cw.methodref(
            &companion_internal,
            &format!("access${name}"),
            &accessor_descriptor,
        );
        code.invokestatic(
            accessor,
            i32::from(argument_words) + 1,
            physical_call_result_words(result),
        );
    } else {
        let member = cw.methodref(&companion_internal, name, &descriptor);
        code.invokevirtual(
            member,
            i32::from(argument_words) + 1,
            physical_call_result_words(result),
        );
    }
    if let Some(&line) = ir.fn_end_lines.get(&function).filter(|&&line| line != 0) {
        code.mark_line(line);
    }
    emit_return(result, &mut code);
    let local_names = crate::jvm::parameter_names::placed(
        ir,
        function,
        None,
        crate::jvm::parameter_names::function_locals(ir, function, &parameters),
    )
    .unwrap_or_default();
    let mut slot = 0u16;
    for (index, ty) in parameters.iter().enumerate() {
        if let Some(Some(local)) = local_names.get(index) {
            code.add_local_entry(0, None, slot, local, &local_variable_desc(*ty));
        }
        slot += slot_words(*ty);
    }
    code.ensure_locals(argument_words);
    code.link();

    let visibility = method_access::jvm_visibility(ir.method_visibility(function));
    // A static method of an interface cannot be final.
    let finality = if interface { 0 } else { 0x0010 };
    let access = visibility | 0x0008 | finality | method_access::varargs_access(ir, function);
    cw.add_method_sig(access, name, &descriptor, &code, signature.as_deref());
    cw.set_method_parameters(name, &descriptor, &method_parameters);
    if nullability_annotated
        && (result_nullability.is_some() || parameter_nullability.iter().any(Option::is_some))
    {
        cw.set_method_nullability(
            name,
            &descriptor,
            result_nullability,
            &parameter_nullability,
        );
    }
    if !parameter_annotations.is_empty() {
        cw.set_method_param_annotations(name, &descriptor, parameter_annotations);
    }
    if ir.deprecated_methods.contains(&function) {
        cw.mark_method_deprecated(name, &descriptor);
    }
    function_annotations::emit_declared(cw, &annotations, name, &descriptor);
}

impl Emitter<'_> {
    /// The singleton whose instance value `value` reads, in a named object's static member. Only
    /// the unbound receiver is the instance: a nested scope that binds value 0 to a local of its
    /// own (an inlined body's receiver) keeps that binding.
    pub(super) fn singleton_receiver_value(&self, value: u32) -> Option<TypeName> {
        self.singleton_receiver.filter(|_| {
            value == 0 && !self.slots.contains_key(&value) && !self.var_types.contains_key(&value)
        })
    }

    /// Whether `expression` reads a static member's receiver value: the singleton's instance
    /// field, which a `getstatic` loads, as kotlinc loads `INSTANCE` for `this`.
    pub(super) fn reads_singleton_receiver(&self, expression: ExprId) -> bool {
        matches!(
            self.ir.expr(expression),
            IrExpr::GetValue(value) if self.singleton_receiver_value(*value).is_some()
        )
    }

    /// Whether `expression` reads a local value or parameter: no code a discarded receiver must
    /// keep.
    pub(super) fn reads_local_value(&self, expression: ExprId) -> bool {
        matches!(self.ir.expr(expression), IrExpr::GetValue(_))
            && !self.reads_singleton_receiver(expression)
    }

    /// Whether operand `expression` runs no code.
    pub(super) fn operand_runs_no_code(&self, expression: ExprId) -> bool {
        crate::ir::expr_runs_no_code(self.ir, expression)
            && !self.reads_singleton_receiver(expression)
    }

    /// Load the published instance of singleton `classifier`.
    pub(super) fn emit_singleton_instance(&mut self, classifier: TypeName, code: &mut CodeBuilder) {
        let dependency = crate::jvm::singleton_storage::of(self.classifiers, classifier);
        let Some(published) =
            singleton_instance_load::published_singleton(self.ir, classifier, dependency)
        else {
            self.run.set_emit_error(format!(
                "missing JVM storage for singleton {}",
                classifier.render()
            ));
            return;
        };
        let (owner, field) =
            singleton_instance_load::loaded_instance(self.self_companion, classifier, published);
        let field = self.cw.fieldref(
            &owner.render(),
            &field,
            &format!("L{};", classifier.render()),
        );
        code.getstatic(field, 1);
    }

    /// Evaluate and discard the receiver of a call that `invokestatic`s a named object's member.
    /// Like kotlinc, keep only a receiver that can run code: the singleton read or a local is
    /// dropped, since the `invokestatic` initializes the object anyway.
    pub(super) fn emit_discarded_static_receiver(
        &mut self,
        receiver: ExprId,
        code: &mut CodeBuilder,
    ) {
        if !receiver_is_inert(self.ir, receiver) {
            self.emit_value(receiver, code);
            code.pop();
        }
    }

    /// Call `fid`, a named object's `@JvmStatic` member of this file: `invokestatic` the member,
    /// or its static `$default` stub when the call omits an argument.
    pub(super) fn emit_singleton_static_call(
        &mut self,
        e: ExprId,
        fid: u32,
        receiver: ExprId,
        args: &[Option<ExprId>],
        code: &mut CodeBuilder,
    ) {
        let declaration = &self.ir.functions[fid as usize];
        let name = declaration.name.clone();
        let owner = declaration
            .dispatch_receiver
            .expect("a static object member keeps its declaring object")
            .render();
        let parameters = jvm_function_params(self.ir, fid);
        let result = jvm_declared_ty(&self.override_results.physical_result(self.ir, fid));
        // A private member reached from another class goes through its receiverless
        // `access$<name>`, which the declaring object publishes.
        let bridged = self.ir.method_visibility(fid).is_private()
            && (self.owner != owner || self.export_private_calls);
        if bridged
            && !self
                .run
                .private_member_access_bridges
                .borrow()
                .contains(&fid)
        {
            self.run.set_emit_error(format!(
                "private @JvmStatic member {owner}.{name} has no access bridge for its caller"
            ));
            return;
        }
        self.emit_discarded_static_receiver(receiver, code);
        let (name, descriptor, argument_words) = if args.iter().any(Option::is_none) {
            if bridged {
                self.run.set_emit_error(format!(
                    "private @JvmStatic member {owner}.{name} omits an argument from another class"
                ));
                return;
            }
            let stub = self.emit_default_call_arguments(e, fid, args, &parameters, code);
            let stub_name = method_defaults::default_call_name(
                self.ir,
                self.export_private_calls,
                fid,
                &format!("{name}$default"),
            );
            let words = stub.iter().map(|ty| i32::from(slot_words(*ty))).sum();
            (stub_name, method_descriptor(&stub, result), words)
        } else {
            let operands: Vec<ExprId> = args.iter().flatten().copied().collect();
            if let Err(mismatch) =
                self.emit_call_descriptor_operands(e, 0, &operands, &parameters, code)
            {
                self.bail_descriptor_arity(&mismatch, result, code);
                return;
            }
            let words = parameters.iter().map(|ty| i32::from(slot_words(*ty))).sum();
            let name = if bridged {
                format!("access${name}")
            } else {
                name
            };
            (name, method_descriptor(&parameters, result), words)
        };
        let method = self.cw.methodref(&owner, &name, &descriptor);
        self.mark_call_start(e, code);
        code.invokestatic(method, argument_words, physical_call_result_words(result));
    }

    /// Call a named object's `@JvmStatic` member declared in another file of this module.
    pub(super) fn emit_module_singleton_static_call(
        &mut self,
        e: ExprId,
        call: ModuleStaticCall<'_>,
        code: &mut CodeBuilder,
    ) {
        let ModuleStaticCall {
            owner,
            name,
            parameters,
            result,
            receiver,
            args,
        } = call;
        let parameters = jvm_tys(parameters);
        let result = self.physical_call_result(e, jvm_declared_ty(result));
        self.emit_discarded_static_receiver(receiver, code);
        if let Err(mismatch) = self.emit_call_descriptor_operands(e, 0, args, &parameters, code) {
            self.bail_descriptor_arity(&mismatch, result, code);
            return;
        }
        let descriptor = method_descriptor(&parameters, result);
        let method = self.cw.methodref(&owner.render(), name, &descriptor);
        let words = parameters.iter().map(|ty| i32::from(slot_words(*ty))).sum();
        self.mark_call_start(e, code);
        code.invokestatic(method, words, physical_call_result_words(result));
    }
}

/// One checked call of a sibling file's static object member.
pub(super) struct ModuleStaticCall<'a> {
    pub(super) owner: TypeName,
    pub(super) name: &'a str,
    pub(super) parameters: &'a [Ty],
    pub(super) result: &'a Ty,
    pub(super) receiver: ExprId,
    pub(super) args: &'a [ExprId],
}
