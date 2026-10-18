//! The `$serializer` class kotlinx.serialization generates for a plain serializable class.
//!
//! [`declare`] runs at the frontend/backend handoff and adds only what Kotlin declares: the class,
//! its supertypes, members, constructor parameters and `descriptor` property, and the serialized
//! class's deserialization constructor. Every generated body is a placeholder. [`complete`] runs
//! in the target backend and adds the rest: bodies, bridges, the descriptor initializer, the
//! serialized class's `write$Self` (kotlinc makes it in IR only, so it is no declaration of a
//! library), annotation markers and caches.

use super::*;
use crate::ir::{IrGeneratedClassRole, IrGeneratedFunctionRole};
use deserialization_constructor::{
    complete_deserialization_constructor, declare_deserialization_constructor,
};

/// What both phases derive from the serialized class's checked declaration.
struct SerializedClass {
    class_name: TypeName,
    serializer_name: TypeName,
    serializer_type_parameters: Vec<crate::ir::IrTypeParameter>,
    type_params: Vec<String>,
    type_parameter_tys: Vec<Ty>,
    serialized_ty: Ty,
    type_serializers: Vec<Ty>,
    owner_start_line: u32,
    owner_header_line: u32,
    owner_end_line: u32,
    foo_fields: Vec<(String, Ty)>,
    elements: SerialElements,
    element_fields: Vec<(String, Ty)>,
}

impl SerializedClass {
    fn of(ir: &IrFile, class_id: ClassId) -> Self {
        let class_name = ir.classes[class_id as usize].fq_name_id();
        let source_type_parameters = ir
            .class_signature_name(class_name)
            .map(|signature| signature.type_params.clone())
            .unwrap_or_default();
        let type_params = source_type_parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let type_parameter_tys = source_type_parameters
            .iter()
            .map(|parameter| {
                let bound = parameter
                    .bounds
                    .first()
                    .map(|(bound, _)| *bound)
                    .unwrap_or_else(|| Ty::nullable(class_ty("kotlin/Any")));
                Ty::ty_param(&parameter.semantic_name, bound)
            })
            .collect::<Vec<_>>();
        // kotlinc's generated `$serializer` repeats the source parameter names but not their
        // bounds. The source bounds still shape the serialized type and generated bodies above;
        // they are not declarations of the generated classifier itself.
        let serializer_type_parameters = source_type_parameters
            .into_iter()
            .map(|mut parameter| {
                parameter.bounds.clear();
                parameter
            })
            .collect();
        let serialized_ty = if type_parameter_tys.is_empty() {
            Ty::obj_name(class_name)
        } else {
            Ty::obj_args_name(class_name, &type_parameter_tys)
        };
        let type_serializers = type_parameter_tys
            .iter()
            .map(|parameter| kserializer_of(*parameter))
            .collect::<Vec<_>>();
        let owner = &ir.classes[class_id as usize];
        let foo_fields: Vec<(String, Ty)> = owner
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        // A `@Transient` property is a field but not an element (`serial_elements`).
        let elements = SerialElements::of(ir, class_id);
        let element_fields = elements.select(&foo_fields);
        Self {
            serializer_name: serializer_name(class_name),
            class_name,
            serializer_type_parameters,
            type_params,
            type_parameter_tys,
            serialized_ty,
            type_serializers,
            owner_start_line: owner.decl_start_line,
            owner_header_line: owner.decl_line,
            owner_end_line: owner.decl_end_line,
            foo_fields,
            elements,
            element_fields,
        }
    }
}

/// Whether kotlinc gives the class `write$Self` and the deserialization constructor: a plain data
/// class does, whereas a value class (inline serializer, `constructor-impl` only) and an object
/// are compiled differently and get neither.
fn plain_data_class(ir: &IrFile, class_id: ClassId) -> bool {
    !ir.classes[class_id as usize].is_value && !ir.classes[class_id as usize].is_object
}

/// Declare `class_id`'s `$serializer` and the members the plugin adds to `class_id` itself.
pub(super) fn declare(
    ir: &mut IrFile,
    class_id: ClassId,
    statuses: crate::types::SerializationGeneratedMemberStatuses,
) {
    let SerializedClass {
        class_name,
        serializer_name,
        serializer_type_parameters,
        type_params,
        type_parameter_tys,
        serialized_ty,
        owner_start_line,
        owner_header_line,
        owner_end_line,
        foo_fields,
        elements,
        ..
    } = SerializedClass::of(ir, class_id);
    let members = add_serializer_members(
        ir,
        serializer_name,
        serialized_ty,
        owner_start_line,
        owner_end_line,
        !type_params.is_empty(),
    );
    statuses.record(ir, members);
    let GeneratedSerializerMembers {
        descriptor,
        serialize,
        deserialize,
        child_serializers: child,
        type_parameter_serializers: type_params_ser,
    } = members;
    // A generic `$serializer` stores one `KSerializer` per type parameter; a non-generic
    // serializer keeps the singleton-object form.
    let n_tp = type_params.len();
    let is_generic = n_tp > 0;
    let mut ser = crate::plugins::synthetic_class_name(serializer_name);
    // The generated class stands where the annotated declaration does. Its member debug
    // shape is producer-published; its constructor and class initializer retain the class
    // start/header/end lines used by their separate JVM emission paths.
    ser.decl_line = owner_header_line;
    ser.decl_start_line = owner_start_line;
    ser.decl_end_line = ir.classes[class_id as usize].decl_end_line;
    ser.applied_annotations = generated_serializer_annotations();
    ser.is_object = !is_generic;
    // `GeneratedSerializer` supplies the default type-parameter serializer member.
    ser.interfaces = vec![crate::types::type_name(GENERATED_SERIALIZER_FQ)].into();
    ser.type_params = type_params.clone();
    ser.supertypes = vec![kserializer_of(serialized_ty)];
    // Field 0 is the final descriptor; generic serializer fields 1..=N hold the final
    // type-parameter serializers supplied to the constructor.
    let descriptor_field = ser.fields.len() as u32;
    ser.fields.push(
        crate::ir::IrField::new(
            "descriptor".to_string(),
            class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
        )
        .with_is_final(true),
    );
    // kotlinc keeps each type-parameter serializer in compiler-generated storage typed
    // `KSerializer<*>`; only the public constructor that fills it names the parameter.
    for k in 0..n_tp {
        ser.fields.push(
            crate::ir::IrField::new(format!("typeSerial{k}"), star_kserializer())
                .with_is_final(true)
                .with_compiler_generated(true),
        );
    }
    // kotlinc's `$serializer` member order is `<init>`, `serialize`, `deserialize`,
    // `getDescriptor`, `childSerializers`, `typeParametersSerializers`, then the bridges.
    // `getDescriptor` is DECLARED first — the descriptor field it returns is built in
    // `<init>` — but EMITTED fourth.
    ser.methods = vec![serialize, deserialize, descriptor, child, type_params_ser];
    // What `@Metadata` says about those members is a different list in a different order.
    // kotlinc DECLARES `childSerializers`, `deserialize`, `serialize`; it describes
    // `typeParametersSerializers` only when there are type-parameter serializers to pass
    // along; and `getDescriptor` is described as the accessor of a `descriptor` PROPERTY,
    // registered below rather than as a function.
    let descriptor_order = 3 + u32::from(is_generic);
    ser.properties.push(descriptor_property(
        descriptor,
        descriptor_field,
        descriptor_order,
    ));
    let serializer_identity = ser.fq_name_id();
    ir.mark_synthetic_class(serializer_identity);
    ir.mark_deprecated_class(serializer_identity);
    let serializer_class = ir.add_class(ser);
    if is_generic {
        // A generic serializer's primary constructor only builds the descriptor and is private;
        // the public constructor takes the type-parameter serializers and delegates to it.
        ir.ctor_visibilities
            .insert(serializer_identity, crate::types::Visibility::Private);
        declare_type_serializer_constructor(
            ir,
            serializer_class,
            &type_parameter_tys,
            (owner_start_line, owner_end_line),
        );
    }
    ir.record_generated_class(
        class_name,
        IrGeneratedClassRole::SerializationSerializer,
        serializer_class,
    );
    // `Foo.$serializer` is nameable Kotlin, so kotlinc records it under the serialized
    // class's `Class.nestedClassName` — ahead of the `Companion`, which the metadata writer
    // appends last. Without the record a reader of `Foo`'s metadata cannot reach the
    // serializer as a member of the type it serializes.
    ir.classes[class_id as usize]
        .published_nested_classifiers
        .push(SERIALIZER_OBJECT_NAME.to_string());
    ir.insert_class_signature_name(
        serializer_identity,
        generated_serializer_signature(serializer_type_parameters, serialized_ty),
    );

    let plain_data_class = plain_data_class(ir, class_id);
    if plain_data_class {
        declare_deserialization_constructor(ir, class_id, &foo_fields, &elements);
    }
}

/// Complete what [`declare`] declared for `class_id`. Returns the class's pending
/// `$childSerializers` cache, which is built once every serializer is complete.
pub(super) fn complete(
    plugin: &SerializationPlugin,
    ir: &mut IrFile,
    ctx: &PluginContext,
    class_id: ClassId,
) -> Option<PendingChildSerializerCache> {
    let SerializedClass {
        serializer_name,
        serializer_type_parameters,
        type_params,
        serialized_ty,
        type_serializers,
        owner_start_line,
        owner_end_line,
        foo_fields,
        elements,
        element_fields,
        ..
    } = SerializedClass::of(ir, class_id);
    let ser_id = ir
        .generated_class(
            ir.classes[class_id as usize].fq_name_id(),
            IrGeneratedClassRole::SerializationSerializer,
        )
        .expect("a plain serializable class has its declared $serializer");
    assert_eq!(ir.classes[ser_id as usize].fq_name_id(), serializer_name);
    if plugin.serializer_type_parameters_are_bounded() && !type_params.is_empty() {
        let bounded = ir
            .class_signature_name(ir.classes[class_id as usize].fq_name_id())
            .map(|signature| signature.type_params.clone())
            .unwrap_or_default();
        ir.insert_lowered_class_signature(
            serializer_name,
            generated_serializer_signature(bounded, serialized_ty),
        );
    }
    let GeneratedSerializerMembers {
        serialize,
        deserialize,
        type_parameter_serializers,
        ..
    } = GeneratedSerializerMembers::declared(ir, serializer_name);
    let n_tp = type_params.len();
    let is_generic = n_tp > 0;
    if !is_generic {
        install_inherited_type_parameter_serializers(
            ir,
            serializer_name,
            type_parameter_serializers,
        );
    }
    // A property is optional when it declares a default: a constructor parameter's default
    // or a body property's initializer. Optionality is independent of whether that
    // expression is a constant; the constant payload is used separately during body
    // generation for equality/fill operations that can represent it directly.
    let foo_optional: Vec<bool> = elements
        .fields()
        .iter()
        .map(|&field| property_default::checked_default(ir, class_id, field).is_some())
        .collect();
    // Erased generic bridges the `KSerializer<Foo>` interface requires: the JVM sees
    // `serialize(Encoder, Object)` / `deserialize(Decoder): Object`; each adapts args/return
    // and delegates to the concrete `Foo`-typed override.
    ir.classes[ser_id as usize].bridges = vec![
        crate::ir::Bridge {
            kind: crate::ir::BridgeKind::Function,
            target_function: Some(serialize),
            overridden_owner: None,
            collection_barrier: None,
            parameters: vec![
                crate::ir::BridgeParameter {
                    identity: crate::fir::ResolvedParameterIdentity::Source("encoder".into()),
                    semantic: class_ty("kotlinx/serialization/encoding/Encoder"),
                },
                crate::ir::BridgeParameter {
                    identity: crate::fir::ResolvedParameterIdentity::Source("value".into()),
                    semantic: class_ty("kotlin/Any"),
                },
            ],
            name: "serialize".to_string(),
            erased_params: vec![
                class_ty("kotlinx/serialization/encoding/Encoder"),
                class_ty("kotlin/Any"),
            ],
            erased_ret: unit(),
            concrete_params: vec![
                class_ty("kotlinx/serialization/encoding/Encoder"),
                serialized_ty,
            ],
            concrete_ret: unit(),
            target_ret: None,
            barrier_plan: None,
            special: false,
            module_name_bridge: false,
            target_name: None,
            property_implementation: None,
        },
        crate::ir::Bridge {
            kind: crate::ir::BridgeKind::Function,
            target_function: Some(deserialize),
            overridden_owner: None,
            collection_barrier: None,
            parameters: vec![crate::ir::BridgeParameter {
                identity: crate::fir::ResolvedParameterIdentity::Source("decoder".into()),
                semantic: class_ty("kotlinx/serialization/encoding/Decoder"),
            }],
            name: "deserialize".to_string(),
            erased_params: vec![class_ty("kotlinx/serialization/encoding/Decoder")],
            erased_ret: class_ty("kotlin/Any"),
            concrete_params: vec![class_ty("kotlinx/serialization/encoding/Decoder")],
            concrete_ret: serialized_ty,
            target_ret: None,
            barrier_plan: None,
            special: false,
            module_name_bridge: false,
            target_name: None,
            property_implementation: None,
        },
    ];

    // Build the `descriptor` field in <init>: `descriptor = PluginGeneratedSerialDescriptor(
    // "<fqname>", INSTANCE, <n>)` then `descriptor.addElement("<prop>", false)` per property.
    // Build in a local typed as the descriptor class (so `addElement` has a correctly-typed
    // receiver), then store to the field:
    //   val d = PluginGeneratedSerialDescriptor("<fq>", INSTANCE, n)   [local 1, this=0]
    //   d.addElement("<prop>", false) ...
    //   this.descriptor = d
    let is_value = ir.classes[class_id as usize].is_value;
    let pgsd_internal = "kotlinx/serialization/internal/PluginGeneratedSerialDescriptor";
    // The `descriptor` local follows `this`: the constructor that builds it takes no parameters.
    let desc_local = 1;
    // A value class's descriptor is an `InlineClassDescriptor`, a `PluginGeneratedSerialDescriptor`
    // that reports `isInline` and takes no element count. It is built and populated the same way.
    let (descriptor_internal, descriptor_constructor) = if is_value {
        (
            "kotlinx/serialization/internal/InlineClassDescriptor",
            "(Ljava/lang/String;Lkotlinx/serialization/internal/GeneratedSerializer;)V",
        )
    } else {
        (
            pgsd_internal,
            "(Ljava/lang/String;Lkotlinx/serialization/internal/GeneratedSerializer;I)V",
        )
    };
    let pgsd_name = ir.add_expr(IrExpr::Const(IrConst::String(
        annotations::class_serial_name(ir, class_id),
    )));
    // Pass the `$serializer` (a `GeneratedSerializer`) so the descriptor can derive element
    // descriptors from `childSerializers()` (`getElementDescriptor`/introspection).
    //
    // A SINGLETON serializer names itself by its own `INSTANCE`, read at the use site.
    // Reading `this` instead made the JVM emitter hoist `INSTANCE` into a local for the
    // class initializer, which is one store, one load and one extra local that kotlinc
    // does not have. A GENERIC serializer is a real instance built by its constructor,
    // where `this` IS the value. Any JVM reference-boundary cast remains an emission fact.
    let pgsd_self = if is_generic {
        ir.add_expr(IrExpr::GetValue(0))
    } else {
        ir.add_expr(IrExpr::ExternalStaticInstance {
            owner: serializer_name,
            ty: serializer_name,
            field: "INSTANCE".to_string(),
        })
    };
    let mut arguments = vec![pgsd_name, pgsd_self];
    if !is_value {
        arguments.push(ir.add_expr(IrExpr::Const(IrConst::Int(element_fields.len() as i32))));
    }
    let pgsd = ir.new_external(descriptor_internal, descriptor_constructor, arguments);
    let dvar = ir.add_expr(IrExpr::Variable {
        index: desc_local,
        ty: class_ty(descriptor_internal),
        init: Some(pgsd),
        named: false,
    });
    let mut init_stmts = vec![dvar];
    for (i, (pname, _)) in element_fields.iter().enumerate() {
        let d = ir.add_expr(IrExpr::GetValue(desc_local));
        let element_name = serial_name_of(ctx, ir, class_id, pname)
            .unwrap_or_else(|| KtString::from(pname.clone()));
        let nm = ir.add_expr(IrExpr::Const(IrConst::String(element_name)));
        let is_optional = foo_optional.get(i).copied().unwrap_or(false);
        let opt = ir.add_expr(IrExpr::Const(IrConst::Boolean(is_optional)));
        init_stmts.push(ir.add_expr(IrExpr::Call {
            callee: Callee::realized_virtual(
                type_name(descriptor_internal),
                "addElement".to_string(),
                "(Ljava/lang/String;Z)V".to_string(),
                None,
                false,
            ),
            dispatch_receiver: Some(d),
            args: vec![nm, opt],
        }));
    }
    let this0 = ir.add_expr(IrExpr::GetValue(0));
    let dval = ir.add_expr(IrExpr::GetValue(desc_local));
    init_stmts.push(ir.add_expr(IrExpr::SetField {
        receiver: this0,
        class: ser_id,
        index: 0,
        value: dval,
    }));
    let init = ir.add_expr(IrExpr::Block {
        stmts: init_stmts,
        value: None,
    });
    // A generic serializer builds its descriptor in its private constructor, as one initializer
    // spanning the class declaration: it closes on the declaration's last line, and the
    // constructor returns on its first.
    let init = if is_generic {
        ir.initializer_blocks.insert(init);
        ir.expr_source_lines.insert(init, owner_start_line);
        ir.expr_end_lines.insert(init, owner_end_line);
        ir.add_expr(IrExpr::Block {
            stmts: vec![init],
            value: None,
        })
    } else {
        init
    };
    let ser_idx = ser_id as usize;
    ir.classes[ser_idx].init_body = Some(init);
    if is_generic {
        complete_type_serializer_constructor(ir, ser_id, n_tp);
        add_type_serializer_getters(ir, ser_id, n_tp, owner_start_line);
    }

    // `serializer()` accessor. Non-generic returns the `$serializer` singleton
    // (`Foo$serializer.INSTANCE`). Generic form creates a fresh serializer with type
    // serializers as constructor arguments.
    let acc_body = if is_generic {
        let args: Vec<ExprId> = (0..n_tp)
            // Ordinary classes expose this frontend declaration on their companion value,
            // so slot 0 is the companion receiver and declared arguments start at 1.
            .map(|k| ir.add_expr(IrExpr::GetValue(k as u32 + 1)))
            .collect();
        let ser_internal = ir.classes[ser_id as usize].fq_name_id();
        let new_ser = ir.add_expr(IrExpr::New {
            internal: ser_internal,
            args,
            ctor_params: Some(type_serializers),
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        let ret = ir.add_expr(IrExpr::Return(Some(new_ser)));
        ir.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        })
    } else {
        let inst = ir.add_expr(IrExpr::StaticInstance {
            owner: ser_id,
            ty: ser_id,
            field: "INSTANCE",
        });
        // The accessor is declared to return `KSerializer<Foo>` while the singleton's own
        // type is `Foo$serializer`, and kotlinc narrows at the return with a `checkcast` to
        // the interface. The JVM would verify the method without it — which is why this was
        // invisible to every round-trip test — but the bytes differ from kotlinc's.
        let inst = ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast,
            arg: inst,
            type_operand: kserializer_of(serialized_ty),
        });
        let inst_ret = ir.add_expr(IrExpr::Return(Some(inst)));
        ir.add_expr(IrExpr::Block {
            stmts: vec![inst_ret],
            value: None,
        })
    };
    // An object is itself the generated declaration's singleton dispatch receiver. Ordinary
    // classes, including generic ones, complete the exact companion member contributed to
    // frontend resolution.
    let owner = super::serializer_accessor_owner(ir, class_id);
    super::complete_frontend_serializer_accessor(ir, owner, acc_body);

    if plain_data_class(ir, class_id) {
        add_write_self(
            plugin,
            ir,
            class_id,
            &serializer_type_parameters,
            owner_start_line,
        );
    }

    // `<getter>$annotations()` markers for properties kotlinc preserves the serialization
    // annotation on: `@SerialName`, OR a property-level `@Serializable(with = X::class)` custom
    // serializer. The marker's stem is the property's JVM getter name, so a Boolean `isFoo` yields
    // `isFoo$annotations` (JavaBeans `is`-prefix rule).
    let class_identity = ir.classes[class_id as usize].fq_name_id();
    for (prop, _) in &foo_fields {
        if serial_name_of(ctx, ir, class_id, prop).is_none()
            && field_serializer_of(ctx, ir, class_id, prop).is_none()
        {
            continue;
        }
        let source_order = ir.classes[class_id as usize]
            .properties
            .iter()
            .find(|property| property.name == *prop)
            .expect("a serialized field retains its property declaration")
            .source_order;
        let (params, identities, _) =
            ir.property_accessor_parameters(class_id as usize, source_order);
        let ann_ret = ir.add_expr(IrExpr::Return(None));
        let ann_body = ir.add_expr(IrExpr::Block {
            stmts: vec![ann_ret],
            value: None,
        });
        let marker = ir.add_fun(IrFunction {
            name: format!("{}$annotations", property_getter_name(prop)),
            params,
            ret: unit(),
            body: Some(ann_body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.open_methods.insert(marker);
        ir.synthetic_methods.insert(marker);
        ir.deprecated_methods.insert(marker);
        ir.fn_params
            .insert(marker, crate::ir::FnParamInfo::identities(identities));
        ir.classes[class_id as usize].methods.push(marker);
        ir.property_annotation_markers
            .insert((class_identity, source_order), marker);
    }

    let plain_data_class = plain_data_class(ir, class_id);
    if plain_data_class {
        let cached_descriptor = if is_generic {
            let descriptor_elements = element_fields
                .iter()
                .enumerate()
                .map(|(index, (name, _))| {
                    (
                        serial_name_of(ctx, ir, class_id, name)
                            .unwrap_or_else(|| KtString::from(name.clone())),
                        foo_optional[index],
                    )
                })
                .collect::<Vec<_>>();
            let descriptor_name = annotations::class_serial_name(ir, class_id);
            let descriptor_owner = ir.classes[class_id as usize].fq_name_id();
            let cached = add_cached_descriptor(
                ir,
                descriptor_owner,
                descriptor_name,
                &descriptor_elements,
                companion_lines(ir, class_id),
            );
            ir.classes[class_id as usize]
                .field_annotations
                .push(crate::ir::FieldAnnotations {
                    field: "$cachedDescriptor".to_string(),
                    annotations: annotations::jvm_field_annotations(),
                });
            Some(cached)
        } else {
            None
        };
        complete_deserialization_constructor(
            ir,
            class_id,
            ser_id,
            &foo_fields,
            &elements,
            cached_descriptor,
        );
    }

    // The `$childSerializers` cache is built in a SECOND pass — see
    // `add_child_serializer_cache`, which explains why it cannot be built here. A value class
    // caches its underlying element too; only its own `serialize`/`deserialize` build it afresh.
    (!ir.classes[class_id as usize].is_object).then(|| {
        (
            class_id,
            ir.classes[class_id as usize].fq_name_id(),
            element_fields,
        )
    })
}

/// Add `class_id`'s `write$Self`. kotlinc generates it in its IR extension, not its FIR one: a
/// KLIB's metadata has no record of it, while the JVM publishes it as an internal function.
///
/// A generic class's `write$Self` declares its own copy of each class type parameter and takes
/// one serializer per parameter after the descriptor, which `serialize` passes from its
/// `typeSerial<k>` fields.
fn add_write_self(
    plugin: &SerializationPlugin,
    ir: &mut IrFile,
    class_id: ClassId,
    class_type_parameters: &[crate::ir::IrTypeParameter],
    owner_start_line: u32,
) {
    // `write$Self` helper — its NAME is ABI-version-dependent (mangled with the module name
    // on core >= 1.6). Emitted on the serialized class as a static member with a concrete
    // (no-op) body so the class stays concrete (an empty-body method would force it abstract).
    // A value class serializes its sole underlying value inline (no `write$Self`), and an object
    // uses an `ObjectSerializer`; kotlinc emits `write$Self` for a plain data class only.
    let owner = ir.classes[class_id as usize].fq_name_id();
    let any = Ty::nullable(class_ty("kotlin/Any"));
    let type_parameters: Vec<crate::ir::IrTypeParameter> = class_type_parameters
        .iter()
        .enumerate()
        .map(|(ordinal, parameter)| crate::ir::IrTypeParameter {
            name: parameter.name.clone(),
            semantic_name: crate::types::generated_function_type_parameter(
                owner,
                "write$Self",
                ordinal,
                &parameter.name,
            )
            .to_string(),
            // kotlinc's copy is bounded by `Any?` whatever the class parameter's bound, and
            // records that bound explicitly.
            bounds: vec![(any, false)],
            variance: crate::types::TypeVariance::Invariant,
            reified: false,
        })
        .collect();
    let type_arguments: Vec<Ty> = type_parameters
        .iter()
        .map(|parameter| Ty::ty_param(&parameter.semantic_name, any))
        .collect();
    let leading = [
        class_ty("kotlinx/serialization/encoding/CompositeEncoder"),
        class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
    ];
    let physical_self = Ty::obj_name(owner);
    let ws_ret = ir.add_expr(IrExpr::Return(None));
    let ws_body = ir.add_expr(IrExpr::Block {
        stmts: vec![ws_ret],
        value: None,
    });
    let write_self = ir.add_fun(IrFunction {
        name: plugin.write_self_name(),
        params: std::iter::once(physical_self)
            .chain(leading)
            .chain(type_arguments.iter().map(|_| class_ty(KSERIALIZER_FQ)))
            .collect(),
        ret: unit(),
        body: Some(ws_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    if !type_parameters.is_empty() {
        ir.signatures.insert(
            write_self,
            crate::ir::IrGenericSig {
                params: std::iter::once(Ty::obj_args_name(owner, &type_arguments))
                    .chain(leading)
                    .chain(
                        type_arguments
                            .iter()
                            .map(|argument| kserializer_of(*argument)),
                    )
                    .collect(),
                type_params: type_parameters,
                ret: Some(unit()),
                supers: Vec::new(),
            },
        );
    }
    ir.synthetic_methods.insert(write_self);
    ir.classes[class_id as usize].methods.push(write_self);
    publish_write_self(
        ir,
        owner,
        write_self,
        class_type_parameters.len(),
        owner_start_line,
    );
    ir.record_generated_function(
        owner,
        IrGeneratedFunctionRole::SerializationWriteSelf,
        write_self,
    );
}

/// `KSerializer<*>`, the type of a generic serializer's type-parameter serializer storage.
fn star_kserializer() -> Ty {
    kserializer_of(Ty::star_projection(Ty::nullable(class_ty("kotlin/Any"))))
}

/// Declare a generic `$serializer`'s public constructor: it takes one serializer per type
/// parameter, delegates to the private descriptor-building primary constructor and stores them.
fn declare_type_serializer_constructor(
    ir: &mut IrFile,
    serializer: ClassId,
    type_parameter_tys: &[Ty],
    (start_line, end_line): (u32, u32),
) {
    let params: Vec<Ty> = type_parameter_tys
        .iter()
        .map(|parameter| kserializer_of(*parameter))
        .collect();
    let class = &mut ir.classes[serializer as usize];
    assert!(
        class.secondary_ctors.is_empty(),
        "a generated $serializer declares one secondary constructor"
    );
    class.secondary_ctors.push(crate::ir::IrSecondaryCtor {
        // kotlinc declares it in FIR, so it is an ordinary constructor spanning the class
        // declaration: its delegation maps to the declaration's start and its return to its end,
        // and its tables, signature and nullability are a declared constructor's.
        lines: crate::ir::IrSecondaryCtorLines {
            decl_line: start_line,
            delegation_line: start_line,
            decl_end_line: end_line,
            defaults: Vec::new(),
        },
        annotations: crate::ir::DeclarationAnnotations::default(),
        source_order: u32::MAX,
        prefix_params: Vec::new(),
        vararg_index: None,
        named_params: params
            .iter()
            .enumerate()
            .map(|(k, ty)| (format!("typeSerial{k}"), *ty))
            .collect(),
        param_checks: vec![Some(crate::ir::IrParameterCheck::NonNull); params.len()],
        params,
        declared_spellings: Default::default(),
        metadata_visibility: Some(crate::types::Visibility::Public),
        generated_debug: crate::ir::IrGeneratedDeclarationDebug::None,
        defaults: vec![],
        delegate_prelude: Vec::new(),
        delegate_args: vec![],
        default_parameters: Vec::new(),
        body: None,
        delegate: crate::ir::CtorDelegateTarget::This {
            target_params: vec![],
            target: crate::ir::IrConstructorTarget {
                ordinal: 0,
                access: crate::ir::IrConstructorAccess::Private,
            },
            default_masks: vec![],
        },
        synthetic: false,
        vc_params: false,
    });
}

/// The public constructor's body: store each serializer argument (`1..=N`) to its field.
fn complete_type_serializer_constructor(ir: &mut IrFile, serializer: ClassId, n_tp: usize) {
    let stmts = (1..=n_tp as u32)
        .map(|k| {
            let receiver = ir.add_expr(IrExpr::GetValue(0));
            let value = ir.add_expr(IrExpr::GetValue(k));
            ir.add_expr(IrExpr::SetField {
                receiver,
                class: serializer,
                index: k,
                value,
            })
        })
        .collect();
    let body = ir.add_expr(IrExpr::Block { stmts, value: None });
    ir.classes[serializer as usize].secondary_ctors[0].body = Some(body);
}

/// kotlinc's private synthetic `getTypeSerial<k>()` readers. Nothing calls them; its IR plugin adds
/// them after the class's declared members, so they follow the public constructor in the class
/// file, and they are absent from `@Metadata`.
fn add_type_serializer_getters(ir: &mut IrFile, serializer: ClassId, n_tp: usize, line: u32) {
    let owner = ir.classes[serializer as usize].fq_name_id();
    for k in 0..n_tp {
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        let read = ir.add_expr(IrExpr::GetField {
            receiver,
            class: serializer,
            index: k as u32 + 1,
        });
        let returned = ir.add_expr(IrExpr::Return(Some(read)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        });
        let getter = ir.add_fun(IrFunction {
            name: format!("getTypeSerial{k}"),
            params: Vec::new(),
            ret: star_kserializer(),
            body: Some(body),
            is_static: false,
            dispatch_receiver: Some(owner),
            param_checks: Vec::new(),
        });
        ir.synthetic_methods.insert(getter);
        ir.method_visibilities
            .insert(getter, crate::types::Visibility::Private);
        ir.classes[serializer as usize].methods.push(getter);
        ir.record_generated_function(
            owner,
            IrGeneratedFunctionRole::SerializationTypeSerializerGetter(k as u32),
            getter,
        );
        ir.publish_generated_debug_function(
            owner,
            crate::ir::IrGeneratedFunctionPublication {
                function: getter,
                parameter_identities: Vec::new(),
                metadata: None,
                debug: crate::ir::IrGeneratedDeclarationDebug::declaration_line(line),
            },
        );
    }
}

/// The source span of `class_id`'s companion: a declared companion's own lines, else the class's,
/// which a plugin-generated companion takes.
fn companion_lines(ir: &IrFile, class_id: ClassId) -> (u32, u32) {
    let class = &ir.classes[class_id as usize];
    class
        .companion_class
        .and_then(|companion| {
            ir.classes.iter().find(|candidate| {
                candidate.fq_name == companion && !candidate.is_compiler_generated
            })
        })
        .filter(|companion| companion.decl_start_line != 0)
        .map_or((class.decl_start_line, class.decl_end_line), |companion| {
            (companion.decl_start_line, companion.decl_end_line)
        })
}
