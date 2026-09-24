//! Select and emit one complete serializer for a property element type.

use super::constructed_standard_serializers::constructed_standard_serializer;
use super::type_parameter_serializers::TypeParameterSerializers;
use super::{
    build_contextual_serializer, build_polymorphic_serializer, class_ty,
    generated_serializer_accessor, is_nullable, kserializer_of, serializer_of_name,
    type_is_contextual, wrap_nullable_serializer, KSERIALIZER_FQ,
};
use crate::ir::{ClassId, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::plugins::PluginContext;

use super::ExternalSerializer;
use crate::types::{type_name, Ty, TypeName};

#[derive(Clone)]
pub(super) enum ElementSerializerPlan {
    Generated {
        classifier: TypeName,
        arguments: Vec<ElementSerializerPlan>,
    },
    ConstructedStandard {
        serializer: TypeName,
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
    /// A `@Serializable object`: `ObjectSerializer(<serial name>, <object>.INSTANCE, [])`.
    Object {
        object: TypeName,
        serial_name: crate::kt_string::KtString,
        serial_info_unsupported: bool,
    },
    LocalSingleton(ClassId),
    ExternalSingleton(TypeName),
    /// A class type parameter's serializer, which the generic `$serializer` holds in `field`.
    TypeParameter {
        serializer_class: ClassId,
        field: u32,
    },
    /// A classifier declared outside this file whose serializer its companion's generated
    /// `serializer(…)` returns, called with one argument serializer per type parameter.
    ExternalCompanion {
        classifier: TypeName,
        field: Box<str>,
        companion: TypeName,
        arguments: Vec<ElementSerializerPlan>,
    },
    Builtin(TypeName),
}

/// The type requires a child-cache slot, but no complete serializer plan can be selected for it.
/// Keeping this distinct from `Ok(None)` prevents an invalid cached element from being silently
/// reclassified as an uncached one.
pub(super) struct UnderivableChildCacheElement;

/// Select the one serializer plan a child-cache slot will emit.
///
/// `Ok(None)` means the type does not use the cache. `Err` means it does require a cache (a
/// constructed standard type or same-file enum), but its complete serializer is underivable. The
/// cache builder publishes that invalid state as an unsupported residual rather than trying an
/// uncached path.
pub(super) fn child_cache_element_plan(
    ir: &IrFile,
    ctx: &PluginContext,
    ty: &Ty,
) -> Result<Option<ElementSerializerPlan>, UnderivableChildCacheElement> {
    // A static cache cannot hold a serializer built from an instance's type-parameter serializers;
    // kotlinc builds such an element in `childSerializers` itself.
    if ty.mentions_ty_param() {
        return Ok(None);
    }
    let Some(classifier) = ty.kotlin_class_internal() else {
        return Ok(None);
    };
    // kotlinc caches every element whose serializer is a constructed instance rather than a
    // singleton: a standard serializer's, an enum's, an object's `ObjectSerializer`, or an
    // external companion's accessor.
    let cacheable = constructed_standard_serializer(classifier).is_some()
        || ir
            .classes
            .iter()
            .any(|class| class.fq_name_id() == classifier && !class.enum_entries.is_empty())
        || super::cached_serializer::local_serializable_object(ir, ctx, classifier).is_some()
        || matches!(
            ctx.external_serializer(classifier),
            Some(ExternalSerializer::Object { .. } | ExternalSerializer::Companion { .. })
        );
    if !cacheable {
        return Ok(None);
    }
    element_serializer_plan(ir, ctx, ty)
        .map(Some)
        .ok_or(UnderivableChildCacheElement)
}

/// Preserve an element whose required serializer could not be emitted as an explicit plugin-owned
/// residual. A literal `null` would turn the invalid state into bytecode and defer the failure to
/// the serialization runtime; an unhandled placeholder makes every backend decline the file.
pub(super) fn unsupported_element_serializer(ir: &mut IrFile, ty: Ty) -> ExprId {
    ir.add_expr(IrExpr::PluginPlaceholder {
        plugin: "serialization",
        kind: "element-serializer",
        exprs: Vec::new(),
        data: ty.kotlin_class_internal().into_iter().collect(),
        types: vec![ty],
    })
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
    element_serializer_plan_in(ir, ctx, ty, TypeParameterSerializers::NONE)
}

/// [`element_serializer_plan`] where the class type parameters in `scope` have serializers: an
/// element of such a parameter's type reads the one its `$serializer` was constructed with.
pub(super) fn element_serializer_plan_in(
    ir: &IrFile,
    ctx: &PluginContext,
    ty: &Ty,
    scope: TypeParameterSerializers<'_>,
) -> Option<ElementSerializerPlan> {
    let nn = ty.non_null();
    // A type parameter's serializer is the one supplied for it, never its bound's: `T : Base`
    // holds whatever subtype the caller serialized. Outside a scope that supplies one, there is
    // none to use.
    if let Some(identity) = nn.ty_param_name() {
        return scope
            .field(identity)
            .map(|field| ElementSerializerPlan::TypeParameter {
                serializer_class: scope.serializer_class(),
                field,
            });
    }
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
    // A `@Serializable object` has no `$serializer` either: its serializer is an `ObjectSerializer`
    // over its `INSTANCE`, which kotlinc constructs at the use site.
    if let Some(object) = super::cached_serializer::local_serializable_object(ir, ctx, fq_name) {
        return Some(ElementSerializerPlan::Object {
            object: fq_name,
            serial_name: super::annotations::class_serial_name(ir, object),
            serial_info_unsupported: super::annotations::class_has_serial_info(ctx, ir, object),
        });
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
    // A constructed standard type (`List<T>`, `Pair<A, B>`, `Map.Entry<K, V>`, …) serializes
    // through the runtime serializer selected for its classifier. Its operands come from every
    // checked semantic type argument; the classifier-to-serializer ABI table owns no arity fact.
    if let Some(serializer) = constructed_standard_serializer(fq_name) {
        if type_args.is_empty() {
            return None;
        }
        let arguments = type_args
            .iter()
            .map(|argument| type_argument_serializer_plan(ir, ctx, argument, scope))
            .collect::<Option<Vec<_>>>()?;
        return Some(ElementSerializerPlan::ConstructedStandard {
            serializer,
            arguments,
        });
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
            arguments.push(type_argument_serializer_plan(ir, ctx, argument, scope)?);
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
                    .map(readable_type_argument)
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
                        .map(|argument| type_argument_serializer_plan(ir, ctx, argument, scope))
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
    // A provider-confirmed non-generic object is reachable through `INSTANCE`. A generic custom
    // serializer class needs an ordinary checked constructor call; this post-check plugin cannot
    // reconstruct overload selection from classifier arity, so that shape remains underivable.
    match ctx.external_serializer(fq_name) {
        Some(&ExternalSerializer::Singleton(serializer)) if type_args.is_empty() => {
            return Some(ElementSerializerPlan::ExternalSingleton(serializer));
        }
        Some(ExternalSerializer::Object {
            serial_name,
            serial_info_unsupported,
        }) if type_args.is_empty() => {
            return Some(ElementSerializerPlan::Object {
                object: fq_name,
                serial_name: serial_name.clone(),
                serial_info_unsupported: *serial_info_unsupported,
            });
        }
        Some(ExternalSerializer::Companion {
            field,
            companion,
            type_parameters,
        }) => {
            if type_args.len() != *type_parameters {
                return None;
            }
            let arguments = type_args
                .iter()
                .map(|argument| type_argument_serializer_plan(ir, ctx, argument, scope))
                .collect::<Option<Vec<_>>>()?;
            return Some(ElementSerializerPlan::ExternalCompanion {
                classifier: fq_name,
                field: field.clone(),
                companion: *companion,
                arguments,
            });
        }
        _ => {}
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

/// Select a serializer passed as one generic serializer factory's type argument. A nullable
/// argument needs its own `.nullable` wrapper; the nullable-element encode/decode methods apply to
/// the containing property and cannot provide this nested argument's null semantics.
fn type_argument_serializer_plan(
    ir: &IrFile,
    ctx: &PluginContext,
    argument: &Ty,
    scope: TypeParameterSerializers<'_>,
) -> Option<ElementSerializerPlan> {
    let readable = readable_type_argument(argument)?;
    let plan = element_serializer_plan_in(ir, ctx, &readable, scope)?;
    Some(if is_nullable(&readable) {
        ElementSerializerPlan::Nullable(Box::new(plan))
    } else {
        plan
    })
}

/// The checked read type of one projected serializer argument. An `in` projection has no readable
/// value type, so it cannot supply a serializer operand.
fn readable_type_argument(argument: &Ty) -> Option<Ty> {
    match argument {
        Ty::OutProjection(inner) | Ty::StarProjection(inner) => Some(**inner),
        Ty::InProjection(_) => None,
        _ => Some(*argument),
    }
}

fn emit_constructed_standard_serializer(
    ir: &mut IrFile,
    serializer: TypeName,
    arguments: Vec<ElementSerializerPlan>,
) -> ExprId {
    let arity = arguments.len();
    let arguments = arguments
        .into_iter()
        .map(|argument| {
            let argument = emit_element_serializer(ir, argument);
            narrow_to_kserializer(ir, argument)
        })
        .collect();
    // Constructed directly, not obtained from the `BuiltinSerializersKt` factory that returns the
    // same value. kotlinc's plugin emits the runtime serializer construction itself.
    ir.add_expr(IrExpr::New {
        internal: serializer,
        args: arguments,
        ctor_params: Some(vec![class_ty(KSERIALIZER_FQ); arity]),
        ctor_desc: Some(format!(
            "({})V",
            "Lkotlinx/serialization/KSerializer;".repeat(arity)
        )),
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    })
}

/// kotlinc passes every serializer operand of a serializer factory (a constructed standard
/// serializer, the `nullable` extension) as a `KSerializer`: the cast is a `checkcast` wherever the
/// operand's static type is a concrete serializer class (`StringSerializer.INSTANCE`) and vanishes
/// where it already is `KSerializer` (a type parameter's serializer, another factory's result).
fn narrow_to_kserializer(ir: &mut IrFile, serializer: ExprId) -> ExprId {
    ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: serializer,
        type_operand: class_ty(KSERIALIZER_FQ),
    })
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
        ElementSerializerPlan::ConstructedStandard {
            serializer,
            arguments,
        } => emit_constructed_standard_serializer(ir, serializer, arguments),
        ElementSerializerPlan::Nullable(inner) => {
            let inner = emit_element_serializer(ir, *inner);
            let inner = narrow_to_kserializer(ir, inner);
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
        ElementSerializerPlan::Object {
            object,
            serial_name,
            serial_info_unsupported,
        } => {
            let name = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(serial_name)));
            super::cached_serializer::object_serializer(ir, object, name, serial_info_unsupported)
        }
        ElementSerializerPlan::TypeParameter {
            serializer_class,
            field,
        } => {
            let this = ir.add_expr(IrExpr::GetValue(0));
            ir.add_expr(IrExpr::GetField {
                receiver: this,
                class: serializer_class,
                index: field,
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
        ElementSerializerPlan::ExternalCompanion {
            classifier,
            field,
            companion,
            arguments,
        } => {
            let arguments = arguments
                .into_iter()
                .map(|argument| emit_narrowed_argument(ir, argument))
                .collect();
            super::call_external_companion_serializer(ir, classifier, &field, companion, arguments)
                .expect("a companion is a classifier of its own")
        }
        ElementSerializerPlan::Builtin(serializer) => ir.add_expr(IrExpr::ExternalStaticInstance {
            owner: serializer,
            ty: serializer,
            field: "INSTANCE".to_string(),
        }),
    }
}

/// An argument serializer of a companion's `serializer(…)`: kotlinc narrows each operand to the
/// `KSerializer` the callee takes, including the one a `.nullable` wraps and a collection's own.
fn emit_narrowed_argument(ir: &mut IrFile, plan: ElementSerializerPlan) -> ExprId {
    let serializer = match plan {
        ElementSerializerPlan::Nullable(inner) => {
            let inner = emit_narrowed_argument(ir, *inner);
            wrap_nullable_serializer(ir, inner)
        }
        ElementSerializerPlan::Collection { builder, arguments } => {
            emit_collection_serializer(ir, builder, arguments, true)
        }
        plan => emit_element_serializer(ir, plan),
    };
    ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: serializer,
        type_operand: class_ty(KSERIALIZER_FQ),
    })
}

/// Emit the serializer a child-cache factory returns from the already-selected semantic plan.
///
/// kotlinc narrows the resulting serializer to `KSerializer`, as it does each of its operands.
/// Keeping that representation detail here lets the emitter build the intended IR once; callers
/// never reopen a generic `IrExpr::New` and guess what semantic plan produced it.
pub(super) fn emit_cached_element_serializer(
    ir: &mut IrFile,
    plan: ElementSerializerPlan,
) -> ExprId {
    let serializer = emit_element_serializer(ir, plan);
    narrow_to_kserializer(ir, serializer)
}

pub(super) fn element_serializer_expr(
    ir: &mut IrFile,
    ctx: &PluginContext,
    ty: &Ty,
) -> Option<ExprId> {
    element_serializer_expr_in(ir, ctx, ty, TypeParameterSerializers::NONE)
}

/// [`element_serializer_expr`] inside a generic `$serializer`, whose type-parameter serializers
/// `scope` names.
pub(super) fn element_serializer_expr_in(
    ir: &mut IrFile,
    ctx: &PluginContext,
    ty: &Ty,
    scope: TypeParameterSerializers<'_>,
) -> Option<ExprId> {
    let plan = element_serializer_plan_in(ir, ctx, ty, scope)?;
    Some(emit_element_serializer(ir, plan))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_object_serial_info_remains_an_exact_residual() {
        let object = type_name("fixtures/Singleton");
        let mut ir = IrFile::default();
        let expression = emit_element_serializer(
            &mut ir,
            ElementSerializerPlan::Object {
                object,
                serial_name: crate::kt_string::KtString::from("fixtures.Singleton"),
                serial_info_unsupported: true,
            },
        );

        assert_eq!(expression, 3);
        assert_eq!(ir.exprs.len(), 4);
        assert!(matches!(
            ir.expr(2),
            IrExpr::PluginPlaceholder {
                plugin: "serialization",
                kind: "object-serial-info-annotations",
                exprs,
                data,
                types,
            } if exprs.is_empty() && data.as_slice() == [object] && types.is_empty()
        ));
    }
}
