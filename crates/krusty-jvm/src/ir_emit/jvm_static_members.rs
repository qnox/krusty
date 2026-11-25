//! `@JvmStatic` on source member functions and properties.
//!
//! Common IR keeps a `@JvmStatic` member an ordinary member of its object or companion. Its only
//! input here is the resolved annotation identity `kotlin/jvm/JvmStatic` recorded on the
//! declaration (or on the module callable or property a sibling file uses). A property declares it
//! for both accessors, an accessor (`@get:JvmStatic`, `@JvmStatic get()`) for itself alone. The
//! JVM backend owns both realizations kotlinc produces:
//!
//! - A member of a named `object` is itself a `static` method of the object. Its body keeps the
//!   common-IR receiver numbering; the receiver value reads the singleton's instance field, as
//!   kotlinc reads `INSTANCE` for `this`. A call `invokestatic`s it, evaluating and discarding only
//!   a receiver that can run code. A property's accessors, compiler-default or written, are such
//!   members.
//! - A member of a companion stays an instance method of the companion, and every call keeps
//!   dispatching through the companion instance. The companion's classifier additionally declares a
//!   `static` bridge with the member's name, descriptor, signature and annotations, which loads the
//!   companion field and calls the member: the surface Java and frameworks see. A property's
//!   accessors get one bridge each.

use super::*;

const JVM_STATIC: &str = "kotlin/jvm/JvmStatic";

fn declares_jvm_static(annotations: Option<&krusty::ir::DeclarationAnnotations>) -> bool {
    annotations.is_some_and(|annotations| {
        annotations
            .applications()
            .any(|annotation| annotation.internal.matches(JVM_STATIC))
    })
}

/// Whether `property` of `class` declares `@JvmStatic` for its `side` accessor: on the property
/// itself, which covers both accessors, or on that accessor alone (`@get:JvmStatic`,
/// `@JvmStatic get()`). The property's own applications are the ones its annotation carrier holds;
/// an application written with an accessor's use-site target belongs to that accessor.
pub(crate) fn property_declares_jvm_static(
    class: &IrClass,
    property: &krusty::ir::IrProperty,
    side: PropertyAccessorSide,
) -> bool {
    class.property_annotations.iter().any(|carried| {
        carried.source_order == property.source_order
            && declares_jvm_static(Some(&carried.annotations))
    }) || declares_jvm_static(Some(match side {
        PropertyAccessorSide::Getter => &property.accessor_annotations.getter,
        PropertyAccessorSide::Setter => &property.accessor_annotations.setter,
    }))
}

/// The property of `class` that declares `function` as one of its accessors, with the side.
fn accessor_declaration(
    class: &IrClass,
    function: u32,
) -> Option<(&krusty::ir::IrProperty, PropertyAccessorSide)> {
    class.properties.iter().find_map(|property| {
        if property.getter == Some(function) {
            Some((property, PropertyAccessorSide::Getter))
        } else if property.setter == Some(function) {
            Some((property, PropertyAccessorSide::Setter))
        } else {
            None
        }
    })
}

/// Whether the `side` accessor of `property`, declared by `class`, is a `static` method of the
/// named object `class`. This holds for the compiler-default accessor and a source-written one.
pub(crate) fn singleton_static_accessor(
    class: &IrClass,
    property: &krusty::ir::IrProperty,
    side: PropertyAccessorSide,
) -> bool {
    object_static_storage(class) && property_declares_jvm_static(class, property, side)
}

/// The named object that declares `function` when the JVM realizes it as a static of that object.
pub(crate) fn singleton_static_owner(ir: &IrFile, function: u32) -> Option<TypeName> {
    let declaration = ir.functions.get(function as usize)?;
    if declaration.is_static {
        return None;
    }
    let owner = declaration.dispatch_receiver?;
    let class = &ir.classes[ir.class_id_by_name(owner)? as usize];
    (object_static_storage(class)
        && class.methods.contains(&function)
        && (declares_jvm_static(ir.function_annotations.get(&function))
            || accessor_declaration(class, function).is_some_and(|(property, side)| {
                property_declares_jvm_static(class, property, side)
            })))
    .then_some(owner)
}

/// [`singleton_static_owner`] for a member another source file of this module declares.
pub(crate) fn module_singleton_static_owner(
    ir: &IrFile,
    target: krusty::fir::CallableId,
) -> Option<TypeName> {
    let callable = ir.referenced_module_callables.get(&target)?;
    let owner = callable.owner?;
    if callable.owner_kind != Some(krusty::ir::IrClassifierKind::Object)
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

/// The named object whose `side` accessor of `target`, a property another source file of this
/// module declares, is a static of that object: [`singleton_static_accessor`] across files.
pub(crate) fn module_singleton_static_accessor(
    ir: &IrFile,
    target: krusty::fir::PropertyId,
    side: PropertyAccessorSide,
) -> Option<TypeName> {
    let property = ir.referenced_module_properties.get(&target)?;
    let owner = property.owner?;
    let accessor_annotations = match side {
        PropertyAccessorSide::Getter => &property.getter_annotations,
        PropertyAccessorSide::Setter => &property.setter_annotations,
    };
    let declares = property
        .property_annotations
        .iter()
        .any(|annotation| annotation.matches(JVM_STATIC))
        || accessor_annotations
            .iter()
            .any(|annotation| annotation.identity.matches(JVM_STATIC));
    if property.owner_kind != Some(krusty::ir::IrClassifierKind::Object) || !declares {
        return None;
    }
    let classifier = ir
        .referenced_module_classifiers
        .get(&owner)
        .expect("a referenced object member publishes its owner's singleton shape");
    classifier.companion_owner.is_none().then_some(owner)
}

/// Whether `receiver` of a static object member's call runs no code beyond naming the object,
/// which the `invokestatic` initializes anyway.
pub(crate) fn receiver_is_inert(ir: &IrFile, receiver: ExprId) -> bool {
    krusty::ir::expr_runs_no_code(ir, receiver)
        || matches!(
            ir.expr(receiver),
            IrExpr::SingletonValue { .. }
                | IrExpr::StaticInstance { .. }
                | IrExpr::ExternalStaticInstance { .. }
        )
}

/// One static bridge the outer class declares for a `@JvmStatic` member of its companion.
#[derive(Clone, Copy)]
pub(crate) enum CompanionBridgeTarget {
    /// A companion function, or a property accessor the companion declares as a function.
    Function(u32),
    /// The compiler-default accessor of a companion property the companion stores itself (an
    /// interface's companion does), which has no function of its own.
    DefaultAccessor {
        property: u32,
        side: PropertyAccessorSide,
    },
}

/// The companion members `outer` declares a static bridge for, in the companion's member order:
/// its `@JvmStatic` functions and the accessors of its `@JvmStatic` properties. A property's
/// accessors are one unit at the property's place, getter before setter.
pub(crate) fn companion_bridges(ir: &IrFile, outer: &IrClass) -> Vec<CompanionBridgeTarget> {
    use super::member_schedule::SourceOrderedMember;
    let Some(companion) = outer
        .companion_class
        .and_then(|companion| ir.class_id_by_name(companion))
    else {
        return Vec::new();
    };
    let companion = &ir.classes[companion as usize];
    let bridged_function = |function: u32| {
        companion.methods.contains(&function)
            && !ir.functions[function as usize].is_static
            && (declares_jvm_static(ir.function_annotations.get(&function))
                || accessor_declaration(companion, function).is_some_and(|(property, side)| {
                    property_declares_jvm_static(companion, property, side)
                }))
    };
    let mut bridges = Vec::new();
    for member in super::member_schedule::source_ordered_members(ir, companion, None) {
        match member {
            SourceOrderedMember::Function(function) if bridged_function(function) => {
                bridges.push(CompanionBridgeTarget::Function(function));
            }
            SourceOrderedMember::Property(property) => {
                let index = companion
                    .properties
                    .iter()
                    .position(|declared| std::ptr::eq(declared, property))
                    .expect("a scheduled property belongs to its class");
                for (side, function) in [
                    (PropertyAccessorSide::Getter, property.getter),
                    (PropertyAccessorSide::Setter, property.setter),
                ] {
                    match function {
                        Some(function) if bridged_function(function) => {
                            bridges.push(CompanionBridgeTarget::Function(function));
                        }
                        Some(_) => {}
                        None if property.backing_field.is_some()
                            && property_declares_jvm_static(companion, property, side) =>
                        {
                            bridges.push(CompanionBridgeTarget::DefaultAccessor {
                                property: u32::try_from(index).expect("too many class properties"),
                                side,
                            });
                        }
                        None => {}
                    }
                }
            }
            _ => {}
        }
    }
    bridges
}

/// The companion functions among [`companion_bridges`].
pub(crate) fn companion_bridge_functions(ir: &IrFile, outer: &IrClass) -> Vec<u32> {
    companion_bridges(ir, outer)
        .into_iter()
        .filter_map(|bridge| match bridge {
            CompanionBridgeTarget::Function(function) => Some(function),
            CompanionBridgeTarget::DefaultAccessor { .. } => None,
        })
        .collect()
}

/// The name and descriptor of `outer`'s bridge for a companion property's default accessor, with
/// the offset of the property a clash is reported at. `None` when the companion publishes no such
/// accessor (a private property, a `@JvmField`, a private default setter).
pub(crate) fn companion_default_accessor_bridge_signature(
    ir: &IrFile,
    outer: &IrClass,
    property: u32,
    side: PropertyAccessorSide,
    override_results: &crate::override_results::OverrideResults,
) -> Option<(
    super::declared_property_accessor::EmittedAccessorSignature,
    Option<u32>,
)> {
    let companion = &ir.classes[ir.class_id_by_name(outer.companion_class?)? as usize];
    let declared = companion.properties.get(property as usize)?;
    let signature = super::declared_property_accessor::declared_signature(
        ir,
        companion,
        declared,
        side,
        override_results,
    )?;
    Some((signature, declared.diagnostic_span.map(|span| span.lo)))
}

/// Where a clash of `outer`'s static bridge for companion member `function` is reported: the
/// member's signature, or for a property accessor the accessor, else its property.
pub(crate) fn companion_bridge_diagnostic_offset(
    ir: &IrFile,
    outer: &IrClass,
    function: u32,
) -> Option<u32> {
    if let Some(offset) = ir.function_position(function).signature_offset {
        return Some(offset);
    }
    let companion = &ir.classes[ir.class_id_by_name(outer.companion_class?)? as usize];
    let (property, side) = accessor_declaration(companion, function)?;
    let accessor = match side {
        PropertyAccessorSide::Getter => property.getter_diagnostic_span,
        PropertyAccessorSide::Setter => property.setter_diagnostic_span,
    };
    accessor.or(property.diagnostic_span).map(|span| span.lo)
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
    let bridges = companion_bridges(ir, outer);
    let Some(companion) = outer.companion_class.filter(|_| !bridges.is_empty()) else {
        return;
    };
    let formatter = &JvmSignatureFormatter::new(ir, env);
    let interface = is_jvm_interface(outer);
    for bridge in bridges {
        let shape = match bridge {
            CompanionBridgeTarget::Function(function) => {
                function_bridge_shape(ir, companion, function, formatter, env)
            }
            CompanionBridgeTarget::DefaultAccessor { property, side } => {
                let Some(shape) =
                    default_accessor_bridge_shape(ir, outer, property, side, formatter, env)
                else {
                    continue;
                };
                shape
            }
        };
        emit_companion_bridge(
            CompanionBridge {
                outer: outer.fq_name,
                companion,
                interface,
            },
            shape,
            cw,
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

/// The line of the bridge's return: the end of the member it calls. A compiler-default property
/// accessor spans no source of its own; kotlinc places it at its property's declaration line.
fn bridge_line(ir: &IrFile, companion: TypeName, function: u32) -> Option<u32> {
    let default_accessor = ir
        .class_id_by_name(companion)
        .and_then(|class| accessor_declaration(&ir.classes[class as usize], function))
        .filter(|(property, side)| match side {
            PropertyAccessorSide::Getter => !property.modifiers.declared_getter,
            PropertyAccessorSide::Setter => !property.modifiers.declared_setter,
        });
    match default_accessor {
        Some((property, _)) => Some(property.decl_line),
        None => ir.function_position(function).end_line,
    }
    .filter(|&line| line != 0)
}

struct CompanionBridge {
    outer: TypeName,
    companion: TypeName,
    interface: bool,
}

/// Everything a bridge copies from the companion member it calls.
struct BridgeShape {
    name: String,
    descriptor: String,
    signature: Option<String>,
    parameters: Vec<Ty>,
    result: Ty,
    annotations: krusty::ir::DeclarationAnnotations,
    parameter_annotations: Vec<krusty::ir::DeclarationAnnotations>,
    result_nullability: Option<&'static str>,
    parameter_nullability: Vec<Option<&'static str>>,
    nullability_annotated: bool,
    method_parameters: Vec<crate::method_parameters::MethodParameter>,
    local_names: Vec<Option<String>>,
    line: Option<u32>,
    /// The member's visibility and modifier flags; the bridge adds `static` and `final`.
    access: u16,
    /// The member is private: the bridge calls the companion's `access$<name>` instead.
    private: bool,
    deprecated: bool,
}

fn function_bridge_shape(
    ir: &IrFile,
    companion: TypeName,
    function: u32,
    formatter: &JvmSignatureFormatter<'_>,
    env: &EmitEnv,
) -> BridgeShape {
    let declaration = &ir.functions[function as usize];
    let parameters = jvm_function_params(ir, function);
    let annotations = ir
        .function_annotations
        .get(&function)
        .cloned()
        .unwrap_or_default();
    let declared_nullability::DeclaredNullability {
        result: result_nullability,
        parameters: parameter_nullability,
    } = override_result_emission::declared_nullability(ir, env, function);
    let nullability_annotated =
        function_annotations::nullability_annotated(ir, function, &annotations);
    let method_parameters = if env.java_parameters {
        crate::method_parameters::function(ir, function, &parameters, None)
    } else {
        Vec::new()
    };
    let local_names = crate::parameter_names::placed(
        ir,
        function,
        None,
        crate::parameter_names::function_locals(ir, function, &parameters),
    )
    .unwrap_or_default();
    BridgeShape {
        name: declaration.name.clone(),
        descriptor: declared_method_desc(ir, env.override_results, function),
        signature: declared_method_signature(formatter, ir, env.override_results, function),
        result: jvm_declared_ty(&env.override_results.physical_result(ir, function)),
        parameters,
        parameter_annotations: ir.declared_parameters(function).annotations.to_vec(),
        result_nullability,
        parameter_nullability,
        nullability_annotated,
        method_parameters,
        local_names,
        line: bridge_line(ir, companion, function),
        access: method_access::jvm_visibility(ir.method_visibility(function))
            | method_access::varargs_access(ir, function)
            | method_access::annotated_method_modifiers(Some(&annotations)),
        private: ir.method_visibility(function).is_private(),
        deprecated: ir.deprecated_methods.contains(&function),
        annotations,
    }
}

/// The bridge of a companion property's compiler-default accessor, shaped as that accessor is:
/// its declared annotations, its property's nullability, and the property's declaration line.
fn default_accessor_bridge_shape(
    ir: &IrFile,
    outer: &IrClass,
    property: u32,
    side: PropertyAccessorSide,
    formatter: &JvmSignatureFormatter<'_>,
    env: &EmitEnv,
) -> Option<BridgeShape> {
    let (signature, _) = companion_default_accessor_bridge_signature(
        ir,
        outer,
        property,
        side,
        env.override_results,
    )?;
    let companion = &ir.classes[ir.class_id_by_name(outer.companion_class?)? as usize];
    let declared = &companion.properties[property as usize];
    let field = companion.fields.get(declared.backing_field? as usize)?;
    let generic = super::declared_property_accessor::accessor_generic_signatures(
        ir, companion, field, formatter,
    );
    let accessor = super::declared_property_accessor::declared_property_accessor_jvm(
        ir, companion, property, field,
    );
    let (parameters, result) = match side {
        PropertyAccessorSide::Getter => {
            let result = if env
                .override_results
                .boxes_member_property(companion.fq_name, property)
            {
                jvm_declared_ty(&Ty::nullable(declared.ty))
            } else {
                accessor
            };
            (Vec::new(), result)
        }
        PropertyAccessorSide::Setter => (vec![accessor], Ty::Unit),
    };
    let nullability = parameters
        .first()
        .copied()
        .unwrap_or(result)
        .is_reference()
        .then(|| {
            nullability_annotation(
                crate::declared_type_nullability::declared_reference_nullability(declared.ty),
            )
        })
        .flatten();
    let (annotations, parameter_annotations, generic, visibility, local_names) = match side {
        PropertyAccessorSide::Getter => (
            declared.accessor_annotations.getter.clone(),
            Vec::new(),
            generic.getter,
            declared.visibility,
            Vec::new(),
        ),
        PropertyAccessorSide::Setter => (
            declared.accessor_annotations.setter.clone(),
            [&declared.accessor_annotations.setter_parameter]
                .into_iter()
                .filter(|annotations| annotations.declares_annotations())
                .cloned()
                .collect(),
            generic.setter,
            declared.setter_visibility,
            vec![Some("<set-?>".to_string())],
        ),
    };
    let (result_nullability, parameter_nullability) = match side {
        PropertyAccessorSide::Getter => (nullability, Vec::new()),
        PropertyAccessorSide::Setter => (None, vec![nullability]),
    };
    Some(BridgeShape {
        name: signature.name,
        descriptor: signature.descriptor,
        signature: generic,
        parameters,
        result,
        nullability_annotated: !annotations.deprecated_hidden(),
        access: method_access::jvm_visibility(visibility)
            | method_access::annotated_method_modifiers(Some(&annotations)),
        annotations,
        parameter_annotations,
        result_nullability,
        parameter_nullability,
        method_parameters: match side {
            PropertyAccessorSide::Setter if env.java_parameters => {
                vec![(Some("<set-?>".to_string()), 0)]
            }
            _ => Vec::new(),
        },
        local_names,
        line: Some(declared.decl_line).filter(|&line| line != 0),
        private: false,
        deprecated: false,
    })
}

fn emit_companion_bridge(bridge: CompanionBridge, shape: BridgeShape, cw: &mut ClassWriter) {
    let CompanionBridge {
        outer,
        companion,
        interface,
    } = bridge;
    let BridgeShape {
        name,
        descriptor,
        signature,
        parameters,
        result,
        annotations,
        parameter_annotations,
        result_nullability,
        parameter_nullability,
        nullability_annotated,
        method_parameters,
        local_names,
        line,
        access,
        private,
        deprecated,
    } = shape;
    let name = name.as_str();
    // kotlinc visits the header before the code, so its constants precede the body's.
    cw.reserve_method_pool_with_annotations(
        name,
        &descriptor,
        signature.as_deref(),
        &[],
        &annotations,
        &method_parameters,
    );
    cw.reserve_method_parameter_annotation_pool(
        result_nullability,
        &parameter_annotations,
        &parameter_nullability,
        nullability_annotated,
    );

    let argument_words = parameters.iter().map(|ty| slot_words(*ty)).sum::<u16>();
    let mut code = CodeBuilder::new(argument_words);
    let companion_internal = companion.render();
    let field = cw.fieldref(
        &outer.render(),
        crate::companion_storage::field_name(outer, companion),
        &format!("L{companion_internal};"),
    );
    code.getstatic(field, 1);
    let mut slot = 0u16;
    for ty in &parameters {
        load(*ty, slot, &mut code);
        slot += slot_words(*ty);
    }
    if private {
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
    if let Some(line) = line {
        code.mark_line(line);
    }
    emit_return(result, &mut code);
    let mut slot = 0u16;
    for (index, ty) in parameters.iter().enumerate() {
        if let Some(Some(local)) = local_names.get(index) {
            code.add_local_entry(0, None, slot, local, &local_variable_desc(*ty));
        }
        slot += slot_words(*ty);
    }
    code.ensure_locals(argument_words);
    code.link();

    // A static method of an interface cannot be final.
    let finality = if interface { 0 } else { 0x0010 };
    cw.add_method_sig(
        access | 0x0008 | finality,
        name,
        &descriptor,
        &code,
        signature.as_deref(),
    );
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
        cw.set_method_param_annotations(name, &descriptor, &parameter_annotations);
    }
    if deprecated {
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
        match self.ir.expr(expression) {
            IrExpr::GetValue(value) => self.singleton_receiver_value(*value).is_some(),
            // Naming the instance is loading it, as kotlinc does for the `this` it stands for.
            IrExpr::SingletonValue { .. } => true,
            _ => false,
        }
    }

    /// Whether `expression` reads a local value or parameter: no code a discarded receiver must
    /// keep.
    pub(super) fn reads_local_value(&self, expression: ExprId) -> bool {
        matches!(self.ir.expr(expression), IrExpr::GetValue(_))
            && !self.reads_singleton_receiver(expression)
    }

    /// Whether operand `expression` runs no code.
    pub(super) fn operand_runs_no_code(&self, expression: ExprId) -> bool {
        krusty::ir::expr_runs_no_code(self.ir, expression)
            && !self.reads_singleton_receiver(expression)
    }

    /// Load the published instance of singleton `classifier`.
    pub(super) fn emit_singleton_instance(&mut self, classifier: TypeName, code: &mut CodeBuilder) {
        let dependency = crate::singleton_storage::of(self.classifiers, classifier);
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
