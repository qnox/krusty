//! Select and emit one complete serializer for a property element type.

use super::{
    build_contextual_serializer, build_polymorphic_serializer, class_ty,
    collection_serializer_builder, generated_serializer_accessor, is_nullable, kserializer_of,
    serializer_of_name, type_is_contextual, wrap_nullable_serializer,
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
    Contextual(TypeName),
    Polymorphic(TypeName),
    LocalSingleton(ClassId),
    ExternalSingleton(TypeName),
    Builtin(&'static str),
}

/// Builtin serializers that are NOT in every supported kotlinx.serialization artifact. The plugin
/// context probes these against the active classpath; a mapping to one of them is only usable when
/// the probe found it. The long-standing primitive serializers are omitted on purpose — they are in
/// every supported version, so probing them would cost lookups that can only answer yes.
pub(crate) const RUNTIME_DEPENDENT_SERIALIZERS: &[&str] = &[
    "kotlinx/serialization/internal/InstantSerializer",
    "kotlinx/serialization/internal/UuidSerializer",
];

pub(super) fn builtin_element_key(ty: &Ty) -> Option<&'static str> {
    let fq = match ty.non_null() {
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
    Some(if fq.matches("kotlin/Int") {
        "kotlin/Int"
    } else if fq.matches("kotlin/Long") {
        "kotlin/Long"
    } else if fq.matches("kotlin/Boolean") {
        "kotlin/Boolean"
    } else if fq.matches("kotlin/Double") {
        "kotlin/Double"
    } else if fq.matches("kotlin/Float") {
        "kotlin/Float"
    } else if fq.matches("kotlin/Char") {
        "kotlin/Char"
    } else if fq.matches("kotlin/Byte") {
        "kotlin/Byte"
    } else if fq.matches("kotlin/Short") {
        "kotlin/Short"
    } else if fq.matches("kotlin/String") {
        "kotlin/String"
    } else if fq.matches("kotlin/uuid/Uuid") {
        "kotlin/uuid/Uuid"
    } else if fq.matches("kotlin/time/Instant") {
        "kotlin/time/Instant"
    } else {
        return None;
    })
}

pub(super) fn builtin_element_serializer(ty: &Ty) -> Option<&'static str> {
    Some(match builtin_element_key(ty)? {
        "kotlin/String" => "kotlinx/serialization/internal/StringSerializer",
        "kotlin/Int" => "kotlinx/serialization/internal/IntSerializer",
        "kotlin/Long" => "kotlinx/serialization/internal/LongSerializer",
        "kotlin/Boolean" => "kotlinx/serialization/internal/BooleanSerializer",
        "kotlin/Double" => "kotlinx/serialization/internal/DoubleSerializer",
        "kotlin/Float" => "kotlinx/serialization/internal/FloatSerializer",
        "kotlin/Char" => "kotlinx/serialization/internal/CharSerializer",
        "kotlin/Byte" => "kotlinx/serialization/internal/ByteSerializer",
        "kotlin/Short" => "kotlinx/serialization/internal/ShortSerializer",
        "kotlin/uuid/Uuid" => "kotlinx/serialization/internal/UuidSerializer",
        "kotlin/time/Instant" => "kotlinx/serialization/internal/InstantSerializer",
        _ => return None,
    })
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
    if let Some(ser) = builtin_element_serializer(ty) {
        // A builtin mapping is only usable when the ACTIVE runtime carries that class. The
        // long-standing primitive serializers always exist; the newer ones do not ship in every
        // supported kotlinx.serialization artifact, and emitting a reference to a class that is
        // absent would fail at class-load rather than here. Declining leaves the caller to bail
        // with a diagnostic, which is the same answer as any other underivable element.
        let serializer = crate::types::type_name(ser);
        if RUNTIME_DEPENDENT_SERIALIZERS.contains(&ser) && !ctx.runtime_provides(serializer) {
            return None;
        }
        return Some(ElementSerializerPlan::Builtin(ser));
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
        ElementSerializerPlan::Builtin(serializer) => {
            ir.external_static_instance(serializer, serializer, "INSTANCE")
        }
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
