//! Select and emit one complete serializer for a property element type.

use super::{
    build_contextual_serializer, build_polymorphic_serializer, class_ty,
    collection_serializer_builder, generated_serializer_accessor, is_nullable, kserializer_of,
    serializer_of_name, type_is_contextual, wrap_nullable_serializer, KSERIALIZER_FQ,
};
use crate::ir::{Callee, ClassId, ExprId, IrExpr, IrFile};
use crate::libraries::InlineKind;
use crate::plugins::PluginContext;
use crate::types::{type_name, Ty, TypeName};

#[derive(Clone)]
pub(super) enum ElementSerializerPlan {
    Generated {
        classifier: TypeName,
        arguments: Vec<ElementSerializerPlan>,
    },
    Collection {
        builder: &'static str,
        arguments: Vec<ElementSerializerPlan>,
    },
    Nullable(Box<ElementSerializerPlan>),
    /// A local custom serializer class, constructed with one `KSerializer` per type parameter of
    /// the class it serves: `BoxSerializer(<serializer for the argument>)`.
    LocalConstructed {
        serializer: ClassId,
        arguments: Vec<ElementSerializerPlan>,
    },
    Contextual(TypeName),
    Polymorphic(TypeName),
    LocalSingleton(ClassId),
    ExternalSingleton(TypeName),
    Builtin(TypeName),
}

#[derive(Clone, Copy)]
struct BuiltinSerializer {
    classifier: TypeName,
    serializer: TypeName,
    requires_runtime_declaration: bool,
}

fn builtin_serializers() -> &'static [BuiltinSerializer] {
    static SERIALIZERS: std::sync::OnceLock<Box<[BuiltinSerializer]>> = std::sync::OnceLock::new();
    SERIALIZERS.get_or_init(|| {
        [
            (
                "kotlin/String",
                "kotlinx/serialization/internal/StringSerializer",
                false,
            ),
            (
                "kotlin/Int",
                "kotlinx/serialization/internal/IntSerializer",
                false,
            ),
            (
                "kotlin/Long",
                "kotlinx/serialization/internal/LongSerializer",
                false,
            ),
            (
                "kotlin/Boolean",
                "kotlinx/serialization/internal/BooleanSerializer",
                false,
            ),
            (
                "kotlin/Double",
                "kotlinx/serialization/internal/DoubleSerializer",
                false,
            ),
            (
                "kotlin/Float",
                "kotlinx/serialization/internal/FloatSerializer",
                false,
            ),
            (
                "kotlin/Char",
                "kotlinx/serialization/internal/CharSerializer",
                false,
            ),
            (
                "kotlin/Byte",
                "kotlinx/serialization/internal/ByteSerializer",
                false,
            ),
            (
                "kotlin/Short",
                "kotlinx/serialization/internal/ShortSerializer",
                false,
            ),
            (
                "kotlin/uuid/Uuid",
                "kotlinx/serialization/internal/UuidSerializer",
                true,
            ),
            (
                "kotlin/time/Instant",
                "kotlinx/serialization/internal/InstantSerializer",
                true,
            ),
        ]
        .into_iter()
        .map(
            |(classifier, serializer, requires_runtime_declaration)| BuiltinSerializer {
                classifier: type_name(classifier),
                serializer: type_name(serializer),
                requires_runtime_declaration,
            },
        )
        .collect::<Vec<_>>()
        .into_boxed_slice()
    })
}

/// Builtin serializers that are NOT in every supported kotlinx.serialization artifact. The plugin
/// context probes these stable identities against the active classpath; a mapping to one of them is
/// only usable when the probe found it.
pub(crate) fn runtime_dependent_serializers() -> impl Iterator<Item = TypeName> {
    builtin_serializers()
        .iter()
        .filter(|entry| entry.requires_runtime_declaration)
        .map(|entry| entry.serializer)
}

pub(super) fn builtin_element_key(ty: &Ty) -> Option<TypeName> {
    let classifier = match ty.non_null() {
        Ty::Int => type_name("kotlin/Int"),
        Ty::Long => type_name("kotlin/Long"),
        Ty::Boolean => type_name("kotlin/Boolean"),
        Ty::Double => type_name("kotlin/Double"),
        Ty::Float => type_name("kotlin/Float"),
        Ty::Char => type_name("kotlin/Char"),
        Ty::Byte => type_name("kotlin/Byte"),
        Ty::Short => type_name("kotlin/Short"),
        Ty::String => type_name("kotlin/String"),
        semantic => semantic.kotlin_class_internal()?,
    };
    builtin_serializers()
        .iter()
        .any(|entry| entry.classifier == classifier)
        .then_some(classifier)
}

fn builtin_element_serializer(ty: &Ty) -> Option<BuiltinSerializer> {
    let classifier = builtin_element_key(ty)?;
    builtin_serializers()
        .iter()
        .find(|entry| entry.classifier == classifier)
        .copied()
}

/// Serializer identity for a builtin whose declaration is guaranteed by every supported runtime.
/// Runtime-dependent entries deliberately cannot escape through this helper; they must go through
/// [`element_serializer_plan`], which verifies the active classpath first.
pub(super) fn always_available_builtin_serializer(ty: &Ty) -> Option<TypeName> {
    let builtin = builtin_element_serializer(ty)?;
    (!builtin.requires_runtime_declaration).then_some(builtin.serializer)
}

/// If `ty` names a `@JvmInline value class` defined in this IR, its TERMINAL underlying type — how
/// krusty represents a value-class-typed field/value. Recurses through a value-class chain
/// (`A(val b: B)`, `B(val i: Int)` → `Int`), depth-bounded against a malformed cycle. `None` for any
/// type that isn't (transitively) a value class.
/// Select one complete serializer plan without mutating IR. Applicability checks and expression
/// construction consume this same decision, so they cannot drift into parallel overload systems.
pub(super) fn element_serializer_plan(
    ir: &IrFile,
    ctx: &PluginContext,
    ty: &Ty,
) -> Option<ElementSerializerPlan> {
    let nn = ty.non_null();
    // Include de-erased primitives/`String` (`List<Int>` element `Ty::Int`): they own no `Obj` internal
    // name but DO have a builtin element serializer (resolved at the tail via `builtin_element_serializer`).
    // The old `obj_internal()` guard returned `None` for them, leaving a null child serializer → runtime NPE.
    let fq_name = ty.kotlin_class_internal()?;
    let type_args = nn.type_args();
    // A sealed `@Serializable` class has NO `$serializer` (its `serializer()` returns a runtime
    // `SealedClassSerializer`); a field of that type uses `Class.serializer()` directly. Requires the
    // generated `serializer()` accessor (i.e. the class IS `@Serializable`) — else a plain sealed type
    // would call a non-existent method.
    if ir
        .classes
        .iter()
        .any(|c| c.fq_name_id() == fq_name && c.is_sealed)
        && generated_serializer_accessor(ir, fq_name, 0).is_some()
    {
        return Some(ElementSerializerPlan::Generated {
            classifier: fq_name,
            arguments: Vec::new(),
        });
    }
    // An element whose TYPE the file declares contextual (`@file:UseContextualSerialization`)
    // serializes through a `ContextualSerializer`, exactly as a property of that type would. The
    // property-level rule cannot see this one: the contextual type appears only as a collection's
    // element (`List<FlexibleMap>`).
    if type_is_contextual(ctx, ir, fq_name) {
        return Some(ElementSerializerPlan::Contextual(fq_name));
    }
    // A `@Serializable` ENUM element has no `$serializer` class of its own: kotlinc's accessor builds
    // the serializer at run time (`createSimpleEnumSerializer`/`createAnnotatedEnumSerializer`), so an
    // element reads it through the enum's own `serializer()` — `Level.Companion.serializer()`. The
    // accessor's presence is what proves the enum is `@Serializable`.
    if ir
        .classes
        .iter()
        .any(|c| c.fq_name_id() == fq_name && !c.enum_entries.is_empty())
        && generated_serializer_accessor(ir, fq_name, 0).is_some()
    {
        return Some(ElementSerializerPlan::Generated {
            classifier: fq_name,
            arguments: Vec::new(),
        });
    }
    // A standard COLLECTION field (`List<T>`/`Set<T>`/`Map<K,V>`, read-only or mutable) serializes through
    // the kotlinx builtin collection serializer over its element serializer(s):
    // `ListSerializer(<T>)` / `SetSerializer(<T>)` / `MapSerializer(<K>, <V>)` (top-level functions in
    // `BuiltinSerializersKt`). The element serializers are derived recursively; if any can't be, the whole
    // collection can't (a clean `null`/bail, as before).
    if let Some((builder, n)) = collection_serializer_builder(fq_name) {
        if type_args.len() >= n {
            let mut arguments = Vec::with_capacity(n);
            for a in type_args.iter().take(n) {
                // A NULLABLE element (`List<String?>`) needs a `.nullable` element serializer — the
                // collection serializer applies it per element (unlike a nullable FIELD, whose nullability
                // is the `encodeNullableSerializableElement` method, not a wrapped serializer).
                let element = element_serializer_plan(ir, ctx, a)?;
                arguments.push(if is_nullable(a) {
                    ElementSerializerPlan::Nullable(Box::new(element))
                } else {
                    element
                });
            }
            return Some(ElementSerializerPlan::Collection { builder, arguments });
        }
    }
    // A field with OPEN-polymorphic dispatch serializes via `PolymorphicSerializer(<type>::class)`
    // (descriptor serialName `kotlinx.serialization.Polymorphic<T>`), not a generated `$serializer`. Two
    // cases (both non-sealed; a sealed type took the SealedClassSerializer branch above): (a) an INTERFACE
    // type (`InterfaceMultiple<*,*>`) — kotlinx's default for an interface property, no `@Serializable`
    // needed; (b) an ABSTRACT `@Serializable` class (`Poly`/`Poly<*>`) — gated on the generated
    // `serializer()` accessor so a plain abstract base isn't mis-serialized.
    // Scope: matches only a FILE-DECLARED class in `ir.classes`. A stdlib collection interface
    // (`kotlin/collections/List`, …) is not an `ir.classes` entry, so it never lands here — it keeps its
    // builtin/None handling below (a `List` field has no element serializer yet → a clean `null`).
    // An INTERFACE is open-polymorphic whether or not it is `sealed` here: a `@Serializable` SEALED
    // interface was already claimed by the SealedClassSerializer branch above (which requires the
    // `serializer()` accessor), so only a plain interface or a NON-`@Serializable` sealed interface
    // reaches this — both serialize as `PolymorphicSerializer` (kind OPEN). An abstract CLASS still
    // requires the `serializer()` accessor and must not be sealed (a sealed class took the branch above).
    if ir.classes.iter().any(|c| {
        c.fq_name_id() == fq_name
            && (c.is_interface
                || (c.is_abstract
                    && !c.is_sealed
                    // `@Serializable` ⇒ a `serializer()` accessor exists — on the class itself (sealed/
                    // enum/custom) OR relocated to its `Companion` (a plain data/abstract class).
                    && (c.companion_class.is_some()
                        || c.methods
                            .iter()
                            .any(|&m| ir.functions[m as usize].name == "serializer"))))
    }) {
        return Some(ElementSerializerPlan::Polymorphic(fq_name));
    }
    let serializer_name = fq_name.nested_child("$serializer");
    if let Some(sid) = ir
        .classes
        .iter()
        .position(|c| c.fq_name_id() == serializer_name)
    {
        // The declared type-parameter count comes from the BASE class (the `$serializer` is erased).
        let n_tp = ir
            .classes
            .iter()
            .find(|c| c.fq_name_id() == fq_name)
            .map(|c| c.type_params.len())
            .unwrap_or(0);
        if n_tp == 0 {
            return Some(ElementSerializerPlan::LocalSingleton(sid as ClassId));
        }
        // Generic: `Foo.serializer(<arg serializer>…)`, each type argument's serializer derived
        // recursively. A star projection carries its checked readable upper bound separately from
        // an explicit `out` projection, so consume that semantic read type directly.
        // An in-projection has no readable element type from which a serializer can be derived.
        let mut arguments = Vec::with_capacity(n_tp);
        for argument in type_args.iter().take(n_tp) {
            let readable = match argument {
                Ty::OutProjection(inner) | Ty::StarProjection(inner) => **inner,
                Ty::InProjection(_) => return None,
                _ => *argument,
            };
            arguments.push(element_serializer_plan(ir, ctx, &readable)?);
        }
        if arguments.len() != n_tp {
            return None;
        }
        return Some(ElementSerializerPlan::Generated {
            classifier: fq_name,
            arguments,
        });
    }
    // A class this file DECLARES may name its own serializer with a class-level
    // `@Serializable(with = X::class)`. It then has no generated `$serializer` (the accessor branch
    // in `generate_declarations` handles the class itself), so the `$serializer` lookup above finds
    // nothing, and `external_serializer` deliberately covers only types this compilation does NOT
    // declare. Without this arm a same-file annotated class was underivable as an element while the
    // IDENTICAL class in a sibling file resolved through the external map — the reverse of what the
    // file split would suggest. Read the class's own `with =` here, exactly as its accessor does.
    if let Some(class_id) = ir
        .classes
        .iter()
        .position(|class| class.fq_name_id() == fq_name)
    {
        if let Some(custom) = super::annotations::custom_serializer_of(ctx, ir, class_id as ClassId)
        {
            // Only an `object` serializer is reachable as a singleton. A custom serializer declared
            // as a CLASS takes constructor arguments — `ValueSerializer<T>(dataSerializer)` — so
            // reading an `INSTANCE` field off it would reference a field that does not exist. That
            // shape stays underivable here and the caller bails cleanly, exactly as before; it needs
            // a plan that CONSTRUCTS the serializer from its argument serializers.
            if let Some(serializer_id) = ir
                .classes
                .iter()
                .position(|class| class.fq_name_id() == custom && class.is_object)
            {
                return Some(ElementSerializerPlan::LocalSingleton(
                    serializer_id as ClassId,
                ));
            }
            // A serializer CLASS takes one `KSerializer` per type parameter of the class it serves
            // (`BoxSerializer<T>(itemSerializer)`), so construct it with those argument serializers,
            // derived recursively. Require the declared constructor to match that convention
            // exactly: any other constructor shape stays underivable and the caller bails cleanly
            // rather than emitting a call that does not exist.
            if let Some(serializer_id) = ir
                .classes
                .iter()
                .position(|class| class.fq_name_id() == custom)
            {
                let serializer = &ir.classes[serializer_id];
                let readable_arguments = type_args
                    .iter()
                    .map(|argument| match argument {
                        Ty::OutProjection(inner) | Ty::StarProjection(inner) => Some(**inner),
                        Ty::InProjection(_) => None,
                        _ => Some(*argument),
                    })
                    .collect::<Option<Vec<_>>>()?;
                // Each constructor parameter must be `KSerializer<P>` for a distinct DECLARED type
                // parameter. Inspect the resolved `TyParam` identity carried by the type; the strings
                // in `IrClass::type_params` are source labels and are deliberately used only for the
                // declaration count, never to recover identity from spelling.
                let mut parameter_type_parameters = std::collections::HashSet::new();
                let parameters_match = serializer.constructor_prefix_count == 0
                    && serializer.captured_type_params.is_empty()
                    && !readable_arguments.is_empty()
                    && serializer.type_params.len() == readable_arguments.len()
                    && serializer.ctor_args.len() == readable_arguments.len()
                    && serializer.ctor_args.iter().all(|parameter| {
                        let declared = parameter.declared_ty.unwrap_or(parameter.ty).non_null();
                        let Ty::Obj(classifier, [argument]) = declared else {
                            return false;
                        };
                        classifier == type_name(KSERIALIZER_FQ)
                            && argument
                                .ty_param_name()
                                .is_some_and(|identity| parameter_type_parameters.insert(identity))
                    });
                if parameters_match {
                    let arguments = readable_arguments
                        .iter()
                        .map(|argument| element_serializer_plan(ir, ctx, argument))
                        .collect::<Option<Vec<_>>>()?;
                    return Some(ElementSerializerPlan::LocalConstructed {
                        serializer: serializer_id as ClassId,
                        arguments,
                    });
                }
            }
        }
    }
    // A DEPENDENCY's `@Serializable` class brings its own generated serializer: read that singleton
    // off the classpath, which is exactly what kotlinc emits
    // (`getstatic dep/Inner$$serializer.INSTANCE`). Deriving one here is impossible — the plugin only
    // generates serializers for what this file declares.
    // Scope: the non-generic shape. A generic dependency serializer is built through
    // `Foo.Companion.serializer(<argument serializers>)`, which needs the companion's ABI read back
    // from the classpath; until then such a field stays underivable and the caller bails cleanly.
    if type_args.is_empty() {
        if let Some(serializer) = ctx.external_serializer(fq_name) {
            return Some(ElementSerializerPlan::ExternalSingleton(serializer));
        }
    }
    if let Some(builtin) = builtin_element_serializer(ty) {
        // A builtin mapping is only usable when the ACTIVE runtime carries that class. The
        // long-standing primitive serializers always exist; the newer ones do not ship in every
        // supported kotlinx.serialization artifact, and emitting a reference to a class that is
        // absent would fail at class-load rather than here. Declining leaves the caller to bail
        // with a diagnostic, which is the same answer as any other underivable element.
        if builtin.requires_runtime_declaration && !ctx.runtime_provides(builtin.serializer) {
            return None;
        }
        return Some(ElementSerializerPlan::Builtin(builtin.serializer));
    }
    None
}

fn emit_element_serializer(ir: &mut IrFile, plan: ElementSerializerPlan) -> ExprId {
    match plan {
        ElementSerializerPlan::Generated {
            classifier,
            arguments,
        } => {
            let arity = arguments.len();
            let arguments = arguments
                .into_iter()
                .map(|argument| emit_element_serializer(ir, argument))
                .collect();
            serializer_of_name(
                ir,
                classifier,
                vec![kserializer_of(class_ty("kotlin/Any")); arity],
                kserializer_of(Ty::obj_name(classifier)),
                arguments,
            )
        }
        ElementSerializerPlan::Collection { builder, arguments } => {
            let arity = arguments.len();
            let arguments = arguments
                .into_iter()
                .map(|argument| emit_element_serializer(ir, argument))
                .collect();
            let parameters = "Lkotlinx/serialization/KSerializer;".repeat(arity);
            ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: type_name("kotlinx/serialization/builtins/BuiltinSerializersKt"),
                    name: builder.to_string(),
                    descriptor: format!("({parameters})Lkotlinx/serialization/KSerializer;"),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: arguments,
            })
        }
        ElementSerializerPlan::Nullable(inner) => {
            let inner = emit_element_serializer(ir, *inner);
            wrap_nullable_serializer(ir, inner)
        }
        ElementSerializerPlan::Contextual(classifier) => {
            build_contextual_serializer(ir, classifier)
        }
        ElementSerializerPlan::Polymorphic(classifier) => {
            build_polymorphic_serializer(ir, classifier)
        }
        ElementSerializerPlan::LocalConstructed {
            serializer,
            arguments,
        } => {
            let internal = ir.classes[serializer as usize].fq_name_id();
            let arguments = arguments
                .into_iter()
                .map(|argument| emit_element_serializer(ir, argument))
                .collect::<Vec<_>>();
            ir.add_expr(IrExpr::New {
                internal,
                args: arguments,
                ctor_params: None,
                ctor_desc: None,
                external_target: None,
                defaults: Box::new([]),
                default_prefix_count: 0,
            })
        }
        ElementSerializerPlan::LocalSingleton(class) => ir.add_expr(IrExpr::StaticInstance {
            owner: class,
            ty: class,
            field: "INSTANCE",
        }),
        ElementSerializerPlan::ExternalSingleton(serializer) => {
            ir.add_expr(IrExpr::ExternalStaticInstance {
                owner: serializer,
                ty: serializer,
                field: "INSTANCE".to_string(),
            })
        }
        ElementSerializerPlan::Builtin(serializer) => ir.add_expr(IrExpr::ExternalStaticInstance {
            owner: serializer,
            ty: serializer,
            field: "INSTANCE".to_string(),
        }),
    }
}

pub(super) fn element_serializer_expr(
    ir: &mut IrFile,
    ctx: &PluginContext,
    ty: &Ty,
) -> Option<ExprId> {
    let plan = element_serializer_plan(ir, ctx, ty)?;
    Some(emit_element_serializer(ir, plan))
}
