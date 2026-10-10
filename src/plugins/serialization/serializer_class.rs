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
    type_param_bounds: Vec<(String, Ty)>,
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
        let serializer_type_parameters = ir
            .class_signature_name(class_name)
            .map(|signature| signature.type_params.clone())
            .unwrap_or_default();
        let type_params = serializer_type_parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let type_param_bounds = serializer_type_parameters
            .iter()
            .flat_map(|parameter| {
                parameter
                    .bounds
                    .iter()
                    .map(|(bound, _)| (parameter.name.clone(), *bound))
            })
            .collect::<Vec<_>>();
        let type_parameter_tys = serializer_type_parameters
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
            type_param_bounds,
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
pub(super) fn declare(ir: &mut IrFile, class_id: ClassId) {
    let SerializedClass {
        class_name,
        serializer_name,
        serializer_type_parameters,
        type_params,
        type_param_bounds,
        type_parameter_tys,
        serialized_ty,
        owner_start_line,
        owner_header_line,
        owner_end_line,
        foo_fields,
        elements,
        ..
    } = SerializedClass::of(ir, class_id);
    let GeneratedSerializerMembers {
        descriptor,
        serialize,
        deserialize,
        child_serializers: child,
        type_parameter_serializers: type_params_ser,
    } = add_serializer_members(
        ir,
        serializer_name,
        serialized_ty,
        owner_start_line,
        owner_end_line,
        !type_params.is_empty(),
    );
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
    ser.type_param_bounds = type_param_bounds;
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
    for (k, parameter) in type_parameter_tys.iter().copied().enumerate() {
        ser.fields.push(
            crate::ir::IrField::new(format!("typeSerial{k}"), kserializer_of(parameter))
                .with_is_final(true),
        );
    }
    ser.ctor_param_count = n_tp as u32;
    // The N constructor params are type-param serializers (`is_field=false`): stored manually in
    // <init> to fields `1..=N`; field 0 is the descriptor, built in <init>.
    ser.ctor_args = type_parameter_tys
        .iter()
        .copied()
        .map(|parameter| IrCtorArg {
            name: None,
            context_kind: crate::types::ContextParameterKind::None,
            ty: kserializer_of(parameter),
            declared_ty: None,
            is_field: false,
            field_index: None,
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: crate::ir::IrCtorParameterProvenance::Value,
            capture_identity: None,
        })
        .collect();
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
        type_params,
        serialized_ty,
        type_serializers,
        owner_start_line,
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
    // "<fqname>", null, <n>)` then `descriptor.addElement("<prop>", false)` per property.
    // Build in a local typed as PluginGeneratedSerialDescriptor (so `addElement` —
    // invokevirtual on PGSD — has a correctly-typed receiver), then store to the field:
    //   val d = PluginGeneratedSerialDescriptor("<fq>", null, n)   [local 1, this=0]
    //   d.addElement("<prop>", false) ...
    //   this.descriptor = d
    // A value class uses the inline descriptor ONLY when its underlying is a directly-supported
    // primitive/String — the same condition the inline serialize/deserialize arms require, so an
    // unsupported underlying falls back consistently to the PGSD path (never a mismatched mix).
    let is_value = ir.classes[class_id as usize].is_value
        && element_fields
            .first()
            .and_then(|(_, t)| inline_prim_methods(t))
            .is_some();
    let pgsd_internal = "kotlinx/serialization/internal/PluginGeneratedSerialDescriptor";
    // The `descriptor` local index: `this` is 0 and the `N` constructor params (type-param
    // serializers) are `1..=N`, so the descriptor temporary lives at `N+1` (just `1` when N==0).
    let desc_local = n_tp as u32 + 1;
    let mut init_stmts;
    if is_value {
        // A `@JvmInline value class`: the descriptor is `InlinePrimitiveDescriptor(name,
        // <Underlying>Serializer.INSTANCE)` — `isInline == true`, one element (the underlying).
        let name = ir.add_expr(IrExpr::Const(IrConst::String(
            annotations::class_serial_name(ir, class_id),
        )));
        let under_ser = element_fields
            .first()
            .and_then(|(_, t)| element_serializer::always_available_builtin_serializer(t));
        let ser_inst = match under_ser {
            Some(serializer) => ir.add_expr(IrExpr::ExternalStaticInstance {
                owner: serializer,
                ty: serializer,
                field: "INSTANCE".to_string(),
            }),
            // Unsupported underlying (e.g. a nested @Serializable) — leave the default below.
            None => ir.add_expr(IrExpr::Const(IrConst::Null)),
        };
        let d = ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: type_name("kotlinx/serialization/internal/InlineClassDescriptorKt"),
                    name: "InlinePrimitiveDescriptor".to_string(),
                    descriptor: "(Ljava/lang/String;Lkotlinx/serialization/KSerializer;)Lkotlinx/serialization/descriptors/SerialDescriptor;".to_string(),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: vec![name, ser_inst],
            });
        init_stmts = vec![ir.add_expr(IrExpr::Variable {
            index: desc_local,
            ty: class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
            init: Some(d),
            named: false,
        })];
    } else {
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
        let pgsd_n = ir.add_expr(IrExpr::Const(IrConst::Int(element_fields.len() as i32)));
        let pgsd = ir.new_external(
            pgsd_internal,
            "(Ljava/lang/String;Lkotlinx/serialization/internal/GeneratedSerializer;I)V",
            vec![pgsd_name, pgsd_self, pgsd_n],
        );
        let dvar = ir.add_expr(IrExpr::Variable {
            index: desc_local,
            ty: class_ty(pgsd_internal),
            init: Some(pgsd),
            named: false,
        });
        init_stmts = vec![dvar];
        for (i, (pname, _)) in element_fields.iter().enumerate() {
            let d = ir.add_expr(IrExpr::GetValue(desc_local));
            let element_name = serial_name_of(ctx, ir, class_id, pname)
                .unwrap_or_else(|| KtString::from(pname.clone()));
            let nm = ir.add_expr(IrExpr::Const(IrConst::String(element_name)));
            let is_optional = foo_optional.get(i).copied().unwrap_or(false);
            let opt = ir.add_expr(IrExpr::Const(IrConst::Boolean(is_optional)));
            init_stmts.push(ir.add_expr(IrExpr::Call {
                callee: Callee::realized_virtual(
                    type_name(pgsd_internal),
                    "addElement".to_string(),
                    "(Ljava/lang/String;Z)V".to_string(),
                    None,
                    false,
                ),
                dispatch_receiver: Some(d),
                args: vec![nm, opt],
            }));
        }
    }
    let this0 = ir.add_expr(IrExpr::GetValue(0));
    let dval = ir.add_expr(IrExpr::GetValue(desc_local));
    init_stmts.push(ir.add_expr(IrExpr::SetField {
        receiver: this0,
        class: ser_id,
        index: 0,
        value: dval,
    }));
    // Store each constructor type-param serializer (`GetValue(1..=N)`) to its field (`1..=N`).
    for k in 0..n_tp {
        let this_k = ir.add_expr(IrExpr::GetValue(0));
        let pv = ir.add_expr(IrExpr::GetValue(k as u32 + 1));
        init_stmts.push(ir.add_expr(IrExpr::SetField {
            receiver: this_k,
            class: ser_id,
            index: k as u32 + 1,
            value: pv,
        }));
    }
    let init = ir.add_expr(IrExpr::Block {
        stmts: init_stmts,
        value: None,
    });
    let ser_idx = ser_id as usize;
    ir.classes[ser_idx].init_body = Some(init);

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
        add_write_self(plugin, ir, class_id, serialized_ty, owner_start_line);
    }

    // `<getter>$annotations()` markers for properties kotlinc preserves the serialization
    // annotation on: `@SerialName`, OR a property-level `@Serializable(with = X::class)` custom
    // serializer. The marker's stem is the property's JVM getter name, so a Boolean `isFoo` yields
    // `isFoo$annotations` (JavaBeans `is`-prefix rule).
    for (prop, _) in &foo_fields {
        if serial_name_of(ctx, ir, class_id, prop).is_none()
            && field_serializer_of(ctx, ir, class_id, prop).is_none()
        {
            continue;
        }
        let ann_ret = ir.add_expr(IrExpr::Return(None));
        let ann_body = ir.add_expr(IrExpr::Block {
            stmts: vec![ann_ret],
            value: None,
        });
        let marker = ir.add_fun(IrFunction {
            name: format!("{}$annotations", property_getter_name(prop)),
            params: vec![],
            ret: unit(),
            body: Some(ann_body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.open_methods.insert(marker);
        ir.synthetic_methods.insert(marker);
        ir.deprecated_methods.insert(marker);
        ir.classes[class_id as usize].methods.push(marker);
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
            Some(add_cached_descriptor(
                ir,
                descriptor_owner,
                descriptor_name,
                &descriptor_elements,
            ))
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
    // `add_child_serializer_cache`, which explains why it cannot be built here.
    plain_data_class.then(|| {
        (
            class_id,
            ir.classes[class_id as usize].fq_name_id(),
            element_fields,
        )
    })
}

/// Add `class_id`'s `write$Self`. kotlinc generates it in its IR extension, not its FIR one: a
/// KLIB's metadata has no record of it, while the JVM publishes it as an internal function.
fn add_write_self(
    plugin: &SerializationPlugin,
    ir: &mut IrFile,
    class_id: ClassId,
    serialized_ty: Ty,
    owner_start_line: u32,
) {
    // `write$Self` helper — its NAME is ABI-version-dependent (mangled with the module name
    // on core >= 1.6). Emitted on the serialized class as a static member with a concrete
    // (no-op) body so the class stays concrete (an empty-body method would force it abstract).
    // A value class serializes its sole underlying value inline (no `write$Self`), and an object
    // uses an `ObjectSerializer`; kotlinc emits `write$Self` for a plain data class only.
    let ws_ret = ir.add_expr(IrExpr::Return(None));
    let ws_body = ir.add_expr(IrExpr::Block {
        stmts: vec![ws_ret],
        value: None,
    });
    let write_self = ir.add_fun(IrFunction {
        name: plugin.write_self_name(),
        params: vec![
            serialized_ty,
            class_ty("kotlinx/serialization/encoding/CompositeEncoder"),
            class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
        ],
        ret: unit(),
        body: Some(ws_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.synthetic_methods.insert(write_self);
    ir.classes[class_id as usize].methods.push(write_self);
    let owner = ir.classes[class_id as usize].fq_name_id();
    publish_write_self(ir, owner, write_self, owner_start_line);
    ir.record_generated_function(
        owner,
        IrGeneratedFunctionRole::SerializationWriteSelf,
        write_self,
    );
}
