//! Reference native plugin for `kotlinx.serialization`. See `docs/PLUGIN_API.md`.
//!
//! For a `@Serializable class Foo(val a: Int, val b: String)`, kotlinc's serialization plugin
//! synthesizes (across its FIR + IR backend extensions):
//!
//!   - a nested `Foo.$serializer` **object** implementing `kotlinx/serialization/KSerializer<Foo>`
//!     with `getDescriptor`, `serialize`, `deserialize`, `childSerializers`;
//!   - `Foo.serializer()` returning that `KSerializer` (kotlinc puts it on `Foo.Companion`).
//!
//! `generate_declarations` creates the `$serializer` object, its member signatures, and the
//! `serializer()` accessor. `transform_bodies` fills descriptor, serializer-list, serialize, and
//! deserialize bodies.

use crate::ir::{
    Callee, ClassId, ExprId, IrConst, IrCtorArg, IrExpr, IrFile, IrFunction, IrTypeOp,
};
use crate::libraries::InlineKind;
use crate::names::property_getter_name;
use crate::plugins::{synthetic_class, IrPlugin, PluginContext};
use crate::types::{type_name, Ty};

pub const SERIALIZABLE_FQ: &str = "kotlinx/serialization/Serializable";
pub const KSERIALIZER_FQ: &str = "kotlinx/serialization/KSerializer";
const GENERATED_SERIALIZER_FQ: &str = "kotlinx/serialization/internal/GeneratedSerializer";

fn custom_serializer_of(ctx: &PluginContext<'_>, ir: &IrFile, class_id: ClassId) -> Option<String> {
    ctx.class_annotation_class_literal_internal(ir, class_id, "Serializable")
}

fn field_serializer_of(
    ctx: &PluginContext<'_>,
    ir: &IrFile,
    class_id: ClassId,
    property: &str,
) -> Option<String> {
    ctx.property_annotation_class_literal_internal(ir, class_id, property, "Serializable")
}

fn serial_name_of(
    ctx: &PluginContext<'_>,
    ir: &IrFile,
    class_id: ClassId,
    property: &str,
) -> Option<String> {
    ctx.property_annotation_const_string(ir, class_id, property, "SerialName")
}

fn property_is_contextual(
    ctx: &PluginContext<'_>,
    ir: &IrFile,
    class_id: ClassId,
    property: &str,
) -> bool {
    if ctx.property_has_annotation_simple(ir, class_id, property, "Contextual") {
        return true;
    }
    ctx.property_canonical_type_name(ir, class_id, property)
        .is_some_and(|canonical| {
            ctx.file_annotation_mentions_canonical_type("UseContextualSerialization", &canonical)
        })
}

/// `StringFormat.encodeToString(SerializationStrategy, value): String` — the 2-arg member a reified
/// `fmt.encodeToString(x)` lowers to. Owned here (not core) so a new serialization runtime is a
/// plugin concern. Specializes a core `PluginPlaceholder { kind: "encodeToString" }`.
const ENCODE_TO_STRING_DESC: &str =
    "(Lkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)Ljava/lang/String;";
/// `StringFormat.decodeFromString(DeserializationStrategy, String): Any` — the 2-arg member a reified
/// `fmt.decodeFromString<C>(s)` lowers to (its erased `Object` result is `checkcast` to `C`).
const DECODE_FROM_STRING_DESC: &str =
    "(Lkotlinx/serialization/DeserializationStrategy;Ljava/lang/String;)Ljava/lang/Object;";

/// The `kotlinx.serialization` runtime ABI the generated code must match. The synthesized member
/// shape changed across releases, so the plugin emits *per target version* — exactly as krusty
/// itself is pinned to a kotlinc version. A mismatch between generated code and the linked runtime
/// is a runtime linkage error, so this is not cosmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SerializationAbi {
    /// core < 1.6: the per-class write helper is the unmangled `write$Self`.
    V1_0,
    /// core >= 1.6 (Kotlin >= 1.8.20): the helper is module-mangled `write$Self$<module>` to avoid
    /// cross-module name clashes.
    #[default]
    V1_6Plus,
}

impl SerializationAbi {
    /// Pick the ABI from a `kotlinx-serialization-core` version string (`"1.8.1"`, `"1.5.0"`).
    /// krusty derives this from the runtime jar on `-classpath`, so the same inputs kotlinc gets
    /// select the same codegen. `< 1.6` → `V1_0`, else `V1_6Plus`.
    pub fn from_core_version(version: &str) -> SerializationAbi {
        let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        let major = parts.next().unwrap_or(0);
        let minor = parts.next().unwrap_or(0);
        if (major, minor) < (1, 6) {
            SerializationAbi::V1_0
        } else {
            SerializationAbi::V1_6Plus
        }
    }

    /// Detect the ABI from a `-classpath` jar list by finding the `kotlinx-serialization-core[-jvm]`
    /// jar and reading its version. Returns `None` if the runtime isn't on the classpath (then the
    /// `@Serializable` annotation itself wouldn't resolve — a user error kotlinc also reports).
    pub fn from_classpath(cp_jars: &[String]) -> Option<SerializationAbi> {
        cp_jars
            .iter()
            .filter_map(|j| {
                let name = j.rsplit('/').next().unwrap_or(j);
                let stem = name.strip_suffix(".jar")?;
                // kotlinx-serialization-core-jvm-1.8.1  /  kotlinx-serialization-core-1.8.1
                let rest = stem.strip_prefix("kotlinx-serialization-core")?;
                let rest = rest.strip_prefix("-jvm").unwrap_or(rest);
                let ver = rest.strip_prefix('-')?;
                Some(SerializationAbi::from_core_version(ver))
            })
            .next()
    }
}

/// The serialization extension, pinned to a target runtime ABI + the compilation's module name
/// (needed for the >=1.6 `write$Self$<module>` mangling).
pub struct SerializationPlugin {
    pub abi: SerializationAbi,
    pub module: String,
}

impl SerializationPlugin {
    pub fn new(abi: SerializationAbi, module: impl Into<String>) -> Self {
        Self {
            abi,
            module: module.into(),
        }
    }

    /// The per-class write-helper name for the target ABI. On core >= 1.6 the module name is mangled
    /// into the suffix with every non-`[A-Za-z0-9]` character replaced by `_` (kotlinc sanitizes the
    /// module name so it is a legal JVM method-name segment — `com.infragnite_x-httpclient` →
    /// `com_infragnite_x_httpclient`).
    fn write_self_name(&self) -> String {
        match self.abi {
            SerializationAbi::V1_0 => "write$Self".to_string(),
            SerializationAbi::V1_6Plus => {
                let mangled: String = self
                    .module
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                format!("write$Self${mangled}")
            }
        }
    }
}

impl Default for SerializationPlugin {
    fn default() -> Self {
        Self::new(SerializationAbi::default(), "main")
    }
}

/// The FqName of the synthesized serializer object for a `@Serializable` class. The object is literally
/// named `$serializer`, so its binary name is `Foo$$serializer` (the outer class + the nesting `$` +
/// the object name `$serializer`) — matching kotlinc, which krusty previously emitted as `Foo$serializer`.
fn serializer_fq(class_fq: &str) -> String {
    format!("{class_fq}$$serializer")
}

/// The FqName of a `@Serializable` class's `Companion` object (`Foo` → `Foo$Companion`), which holds
/// the `serializer()` accessor (kotlinc puts it there, not as a static on the class itself).
fn companion_fq(class_fq: &str) -> String {
    format!("{class_fq}$Companion")
}

/// Place `serializer()` as an INSTANCE method on `class_fq`'s `Companion` — reusing an existing user
/// companion, or synthesizing a `Foo$Companion` (`is_companion`: the emitter gives it a private ctor +
/// a `(DefaultConstructorMarker)` accessor, and `companion_class` on the outer class emits the
/// `public static final Foo$Companion Companion` field and its `<clinit>` init). Mirrors kotlinc, which
/// resolves `Foo.serializer()` through the `Foo.Companion` static field.
fn place_serializer_accessor(
    ir: &mut IrFile,
    class_id: u32,
    class_fq: &str,
    params: Vec<Ty>,
    ret: Ty,
    body: ExprId,
) {
    let comp_fq = companion_fq(class_fq);
    let accessor = ir.add_fun(IrFunction {
        name: "serializer".to_string(),
        params,
        ret,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(comp_fq.clone()),
        param_checks: Vec::new(),
    });
    if let Some(existing) = ir.classes[class_id as usize].companion_class {
        let cid = ir
            .classes
            .iter()
            .position(|c| existing == c.fq_name)
            .expect("companion class registered");
        ir.classes[cid].methods.push(accessor);
    } else {
        let mut comp = synthetic_class(&comp_fq);
        comp.is_companion = true;
        comp.methods = vec![accessor];
        ir.add_class(comp);
        ir.classes[class_id as usize].companion_class = Some(crate::types::type_name(&comp_fq));
    }
}

/// Emit a call to `class_fq.serializer(args)` routed through its `Companion` — `getstatic
/// Foo.Companion; …args…; invokevirtual Foo$Companion.serializer(params)ret` — matching how kotlinc
/// invokes the relocated accessor.
fn companion_serializer_call(
    ir: &mut IrFile,
    class_fq: &str,
    params: Vec<Ty>,
    ret: Ty,
    args: Vec<ExprId>,
) -> ExprId {
    let comp = companion_fq(class_fq);
    let recv = ir.external_static_instance(class_fq, &comp, "Companion");
    ir.add_expr(IrExpr::Call {
        callee: Callee::CrossFileVirtual {
            owner: comp,
            name: "serializer".to_string(),
            params,
            ret,
            interface: false,
        },
        dispatch_receiver: Some(recv),
        args,
    })
}

/// Emit a call to `class_fq.serializer(args)`, routed through the `Companion` for a plain non-generic
/// `@Serializable` data class (whose accessor was relocated there), or as a direct `invokestatic
/// class_fq.serializer(…)` for an enum / sealed / custom-serializer / generic class (whose accessor
/// stays static for now). The kind check is structural (order-independent, unlike probing
/// `companion_class`, which is only set once the target's own declarations are generated).
fn serializer_of(
    ir: &mut IrFile,
    class_fq: &str,
    params: Vec<Ty>,
    ret: Ty,
    args: Vec<ExprId>,
) -> ExprId {
    let expected_companion = companion_fq(class_fq);
    let companion_routed = ir.classes.iter().any(|c| {
        c.fq_name_matches(class_fq)
            && c.type_params.is_empty()
            && c.companion_class_matches(&expected_companion)
    });
    if companion_routed {
        companion_serializer_call(ir, class_fq, params, ret, args)
    } else {
        ir.add_expr(IrExpr::Call {
            callee: Callee::CrossFile {
                facade: class_fq.to_string(),
                name: "serializer".to_string(),
                params,
                ret,
            },
            dispatch_receiver: None,
            args,
        })
    }
}

fn unit() -> Ty {
    Ty::Unit
}

fn class_ty(fq: &str) -> Ty {
    Ty::obj(fq)
}

/// Rewrite every `PluginPlaceholder { plugin: "serialization" }` core emitted for a reified
/// `StringFormat` round-trip into the concrete 2-arg member call. Core recorded the operands
/// (`exprs = [receiver, serializer, value-or-string]`) and the resolved names (`data = [format
/// internal, @Serializable class internal]`); the kotlinx member descriptors are owned here. Each
/// placeholder's arena slot is overwritten in place (the index-based IR makes this a local edit):
/// encode becomes the `encodeToString` call; decode becomes the `decodeFromString` call wrapped in a
/// `checkcast` to the decoded class (the member returns erased `Object`).
fn specialize_reified_placeholders(ir: &mut IrFile) {
    let markers: Vec<usize> = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            IrExpr::PluginPlaceholder {
                plugin: "serialization",
                ..
            } => Some(i),
            _ => None,
        })
        .collect();
    for mid in markers {
        let IrExpr::PluginPlaceholder {
            kind, exprs, data, ..
        } = &ir.exprs[mid]
        else {
            continue;
        };
        // Operand/name layout is fixed by the core producer; a malformed node is left untouched
        // (`jvm_can_emit` then declines the file rather than miscompiling).
        let (&[recv, ser, arg], [fmt, class_internal]) = (exprs.as_slice(), data.as_slice()) else {
            continue;
        };
        let (kind, fmt, class_internal) = (*kind, fmt.clone(), class_internal.clone());
        let call = |descriptor: &str| IrExpr::Call {
            callee: Callee::Virtual {
                owner: fmt.clone(),
                name: kind.to_string(),
                descriptor: descriptor.to_string(),
                interface: false,
            },
            dispatch_receiver: Some(recv),
            args: vec![ser, arg],
        };
        match kind {
            "encodeToString" => ir.exprs[mid] = call(ENCODE_TO_STRING_DESC),
            "decodeFromString" => {
                let decoded = ir.add_expr(call(DECODE_FROM_STRING_DESC));
                ir.exprs[mid] = IrExpr::TypeOp {
                    op: IrTypeOp::Cast,
                    arg: decoded,
                    type_operand: Ty::obj(&class_internal),
                };
            }
            _ => {}
        }
    }
}

/// The target descriptor for a property getter result.
fn ty_descriptor(ctx: &PluginContext<'_>, ty: &Ty) -> Option<String> {
    ctx.target_type_descriptor(*ty)
}

/// The `CompositeDecoder.decode<T>Element` method + descriptor for a property type, or `None` for a
/// reference/richer type (which needs `decodeSerializableElement`). Covers the full primitive set +
/// String; Long/Double are 2-slot and their field locals are sized via `slot_width`.
fn decode_element_method(ty: &Ty) -> Option<(&'static str, &'static str)> {
    let fq = ty.kotlin_class_internal()?;
    Some(if fq.matches("kotlin/Int") {
        (
            "decodeIntElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)I",
        )
    } else if fq.matches("kotlin/Long") {
        (
            "decodeLongElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)J",
        )
    } else if fq.matches("kotlin/Boolean") {
        (
            "decodeBooleanElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)Z",
        )
    } else if fq.matches("kotlin/Float") {
        (
            "decodeFloatElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)F",
        )
    } else if fq.matches("kotlin/Double") {
        (
            "decodeDoubleElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)D",
        )
    } else if fq.matches("kotlin/String") {
        (
            "decodeStringElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)Ljava/lang/String;",
        )
    } else {
        return None; // reference/richer types: decodeSerializableElement (future work)
    })
}

/// JVM local-slot width of a field type. Long/Double take two slots — but a NULLABLE Long?/Double? is
/// carried boxed (`java.lang.Long`/`Double`), a one-slot reference.
fn slot_width(ty: &Ty) -> u32 {
    match ty {
        // A non-nullable Long/Double occupies two slots, whether carried as a de-erased primitive
        // (`Ty::Long`) or an erased `Obj("kotlin/Long")`. A NULLABLE one is boxed (`java/lang/Long`,
        // `Ty::Nullable`) — a one-slot reference — and falls through to 1.
        Ty::Long | Ty::Double => 2,
        Ty::Obj(fq_name, _) if *fq_name == "kotlin/Long" || *fq_name == "kotlin/Double" => 2,
        _ => 1,
    }
}

/// The default value for a field's local before the decode loop fills it.
fn default_const(ty: &Ty) -> IrConst {
    match ty.kotlin_class_internal() {
        Some(n) if n.matches("kotlin/Int") => IrConst::Int(0),
        Some(n) if n.matches("kotlin/Long") => IrConst::Long(0),
        Some(n) if n.matches("kotlin/Boolean") => IrConst::Boolean(false),
        Some(n) if n.matches("kotlin/Float") => IrConst::Float(0.0),
        Some(n) if n.matches("kotlin/Double") => IrConst::Double(0.0),
        Some(n) if n.matches("kotlin/String") => IrConst::String(String::new()),
        _ => IrConst::Null,
    }
}

/// An `invokeinterface` callee on a runtime interface (`Encoder`/`CompositeEncoder`/…).
fn virtual_iface(owner: &str, name: &str, descriptor: &str) -> Callee {
    Callee::Virtual {
        owner: owner.to_string(),
        name: name.to_string(),
        descriptor: descriptor.to_string(),
        interface: true,
    }
}

/// The `CompositeEncoder.encode<T>Element` method + descriptor for a property type, or `None` if the
/// type isn't a directly-encodable primitive/String (a richer type needs `encodeSerializableElement`).
fn encode_element_method(ty: &Ty) -> Option<(&'static str, &'static str)> {
    let fq = ty.kotlin_class_internal()?;
    let d = "Lkotlinx/serialization/descriptors/SerialDescriptor;";
    Some(if fq.matches("kotlin/Int") {
        (
            "encodeIntElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;II)V",
        )
    } else if fq.matches("kotlin/Long") {
        (
            "encodeLongElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;IJ)V",
        )
    } else if fq.matches("kotlin/Boolean") {
        (
            "encodeBooleanElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;IZ)V",
        )
    } else if fq.matches("kotlin/Double") {
        (
            "encodeDoubleElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ID)V",
        )
    } else if fq.matches("kotlin/Float") {
        (
            "encodeFloatElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;IF)V",
        )
    } else if fq.matches("kotlin/String") {
        (
            "encodeStringElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILjava/lang/String;)V",
        )
    } else {
        let _ = d;
        return None;
    })
}

fn kserializer_of(arg: Ty) -> Ty {
    Ty::obj_args(KSERIALIZER_FQ, &[arg])
}

fn is_nullable(ty: &Ty) -> bool {
    ty.is_nullable()
}

/// An instance of an explicit element serializer `X` (from `@Serializable(with = X::class)` on a
/// property): an `object` serializer is its `INSTANCE`; a class serializer is `new X()` (no-arg ctor,
/// as user-defined property serializers in the corpus have). Mirrors `add_custom_serializer_accessor`
/// but for the no-arg class case.
fn build_field_serializer_instance(ir: &mut IrFile, internal: &str) -> ExprId {
    if let Some(oid) = ir
        .classes
        .iter()
        .position(|c| c.fq_name_matches(internal) && c.is_object)
    {
        ir.add_expr(IrExpr::StaticInstance {
            owner: oid as u32,
            ty: oid as u32,
            field: "INSTANCE",
        })
    } else {
        ir.new_external(internal, "()V", vec![])
    }
}

/// Wrap an element serializer in `.nullable` (`BuiltinSerializersKt.getNullable(s)`), the way kotlinc
/// builds the element serializer for a NULLABLE property carrying an explicit serializer — the nullable
/// descriptor's `serialName` gains the trailing `?`.
fn wrap_nullable_serializer(ir: &mut IrFile, base: ExprId) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: "kotlinx/serialization/builtins/BuiltinSerializersKt".to_string(),
            name: "getNullable".to_string(),
            descriptor: "(Lkotlinx/serialization/KSerializer;)Lkotlinx/serialization/KSerializer;"
                .to_string(),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![base],
    })
}

/// The element-serializer expression for a property of (`@Serializable`/builtin) type `ty`, used by a
/// CONTAINING class's `childSerializers`/`serialize`/`deserialize`:
/// - a GENERIC `@Serializable` class `Foo<A…>` → `Foo.serializer(<serializer for A>…)` (invokestatic the
///   generated generic accessor, recursively building each type-argument's serializer);
/// - a non-generic nested `@Serializable` class → its `Foo$serializer.INSTANCE` singleton;
/// - a directly-supported builtin (`String`/primitive/…) → its `…Serializer.INSTANCE`.
///
/// Returns `None` for a type with no derivable serializer (the caller bails to a clean no-op).
/// The `BuiltinSerializersKt` factory name + type-argument count for a standard collection type, or `None`.
/// `List`/`Set`/`Collection`/`Iterable` → 1-arg `ListSerializer`/`SetSerializer`; `Map` → 2-arg `MapSerializer`.
fn collection_serializer_builder(fq_name: &str) -> Option<(&'static str, usize)> {
    match fq_name {
        "kotlin/collections/List"
        | "kotlin/collections/MutableList"
        | "kotlin/collections/Collection"
        | "kotlin/collections/MutableCollection"
        | "kotlin/collections/Iterable"
        | "kotlin/collections/MutableIterable" => Some(("ListSerializer", 1)),
        "kotlin/collections/Set" | "kotlin/collections/MutableSet" => Some(("SetSerializer", 1)),
        "kotlin/collections/Map" | "kotlin/collections/MutableMap" => Some(("MapSerializer", 2)),
        _ => None,
    }
}

fn element_serializer_expr(ir: &mut IrFile, ty: &Ty) -> Option<ExprId> {
    let nn = ty.non_null();
    // Include de-erased primitives/`String` (`List<Int>` element `Ty::Int`): they own no `Obj` internal
    // name but DO have a builtin element serializer (resolved at the tail via `builtin_element_serializer`).
    // The old `obj_internal()` guard returned `None` for them, leaving a null child serializer → runtime NPE.
    let fq_name = ty.kotlin_class_internal()?.render();
    let type_args = nn.type_args();
    // A sealed `@Serializable` class has NO `$serializer` (its `serializer()` returns a runtime
    // `SealedClassSerializer`); a field of that type uses `Class.serializer()` directly. Requires the
    // generated `serializer()` accessor (i.e. the class IS `@Serializable`) — else a plain sealed type
    // would call a non-existent method.
    if ir.classes.iter().any(|c| {
        c.fq_name_matches(&fq_name)
            && c.is_sealed
            && c.methods
                .iter()
                .any(|&m| ir.functions[m as usize].name == "serializer")
    }) {
        return Some(serializer_of(
            ir,
            &fq_name,
            vec![],
            kserializer_of(class_ty(&fq_name)),
            vec![],
        ));
    }
    // A standard COLLECTION field (`List<T>`/`Set<T>`/`Map<K,V>`, read-only or mutable) serializes through
    // the kotlinx builtin collection serializer over its element serializer(s):
    // `ListSerializer(<T>)` / `SetSerializer(<T>)` / `MapSerializer(<K>, <V>)` (top-level functions in
    // `BuiltinSerializersKt`). The element serializers are derived recursively; if any can't be, the whole
    // collection can't (a clean `null`/bail, as before).
    if let Some((builder, n)) = collection_serializer_builder(&fq_name) {
        if type_args.len() >= n {
            let mut arg_sers = Vec::with_capacity(n);
            for a in type_args.iter().take(n) {
                // A NULLABLE element (`List<String?>`) needs a `.nullable` element serializer — the
                // collection serializer applies it per element (unlike a nullable FIELD, whose nullability
                // is the `encodeNullableSerializableElement` method, not a wrapped serializer).
                let elem = element_serializer_expr(ir, a)?;
                arg_sers.push(if is_nullable(a) {
                    wrap_nullable_serializer(ir, elem)
                } else {
                    elem
                });
            }
            let pdesc = "Lkotlinx/serialization/KSerializer;".repeat(n);
            return Some(ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: "kotlinx/serialization/builtins/BuiltinSerializersKt".to_string(),
                    name: builder.to_string(),
                    descriptor: format!("({pdesc})Lkotlinx/serialization/KSerializer;"),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: arg_sers,
            }));
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
        c.fq_name_matches(&fq_name)
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
        return Some(build_polymorphic_serializer(ir, &fq_name));
    }
    let ser_fq = serializer_fq(&fq_name);
    if let Some(sid) = ir.classes.iter().position(|c| c.fq_name_matches(&ser_fq)) {
        // The declared type-parameter count comes from the BASE class (the `$serializer` is erased).
        let n_tp = ir
            .classes
            .iter()
            .find(|c| c.fq_name_matches(&fq_name))
            .map(|c| c.type_params.len())
            .unwrap_or(0);
        if n_tp == 0 {
            return Some(ir.add_expr(IrExpr::StaticInstance {
                owner: sid as u32,
                ty: sid as u32,
                field: "INSTANCE",
            }));
        }
        // Per-type-parameter upper bound (internal name) of the BASE class — used to build a
        // `PolymorphicSerializer` for a star-projection / erased `Any` argument (`Box<*>` with
        // `Box<T : E>` → `PolymorphicSerializer(E::class)`). `kotlin/Any` for an unbounded parameter.
        let bounds: Vec<String> = {
            let base = ir.classes.iter().find(|c| c.fq_name_matches(&fq_name));
            (0..n_tp)
                .map(|i| {
                    base.and_then(|c| {
                        let name = c.type_params.get(i)?;
                        c.type_param_bounds
                            .iter()
                            .find(|(n, _)| n == name)
                            .and_then(|(_, bt)| bt.non_null().obj_internal().map(|s| s.to_string()))
                    })
                    .unwrap_or_else(|| "kotlin/Any".to_string())
                })
                .collect()
        };
        // Generic: `Foo.serializer(<arg serializer>…)`, each type argument's serializer derived
        // recursively; a star-projection / erased `Any` argument becomes a `PolymorphicSerializer` over
        // the parameter's bound; if any other argument can't be derived, the whole element can't be.
        let mut arg_sers = Vec::with_capacity(n_tp);
        for (i, a) in type_args.iter().take(n_tp).enumerate() {
            if let Some(e) = element_serializer_expr(ir, a) {
                arg_sers.push(e);
            } else if a
                .non_null()
                .obj_internal()
                .is_some_and(|n| n.matches("kotlin/Any"))
                && bounds[i] != "kotlin/Any"
            {
                // A star projection on a BOUNDED type parameter (`Box<*>` with `Box<T : E>`, the arg
                // erased to `Any`) → `PolymorphicSerializer(E::class)`. Gated on a non-`Any` bound so an
                // explicit unbounded `Box<Any>` (indistinguishable from `*` after erasure) isn't captured.
                arg_sers.push(build_polymorphic_serializer(ir, &bounds[i]));
            } else {
                return None;
            }
        }
        if arg_sers.len() != n_tp {
            return None;
        }
        let kser = kserializer_of(class_ty("kotlin/Any"));
        return Some(serializer_of(
            ir,
            &fq_name,
            vec![kser; n_tp],
            kserializer_of(class_ty(&fq_name)),
            arg_sers,
        ));
    }
    if let Some(ser) = builtin_element_serializer(ty) {
        return Some(ir.external_static_instance(ser, ser, "INSTANCE"));
    }
    None
}

/// The element serializer for property `name` of type `ty` IF it is contextual (`name` ∈ `contextual`):
/// `ContextualSerializer(<ty>::class)`, wrapped `.nullable` for a nullable property. `None` when the
/// property isn't contextual or its type has no class internal (a primitive can't be contextual).
fn contextual_serializer_for(ir: &mut IrFile, is_contextual: bool, ty: &Ty) -> Option<ExprId> {
    if !is_contextual {
        return None;
    }
    let internal = ty.non_null().obj_internal()?.render();
    let base = build_contextual_serializer(ir, &internal);
    Some(if is_nullable(ty) {
        wrap_nullable_serializer(ir, base)
    } else {
        base
    })
}

/// `new ContextualSerializer(<type>::class)` — the element serializer for a `@Contextual` property (or one
/// whose type is named in `@file:UseContextualSerialization`). Its descriptor `kind` is CONTEXTUAL; the
/// actual serializer is resolved from the `SerializersModule` at run time.
fn build_contextual_serializer(ir: &mut IrFile, type_internal: &str) -> ExprId {
    let classlit = ir.class_const(Some(type_internal));
    let kclass = ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: "kotlin/jvm/internal/Reflection".to_string(),
            name: "getOrCreateKotlinClass".to_string(),
            descriptor: "(Ljava/lang/Class;)Lkotlin/reflect/KClass;".to_string(),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![classlit],
    });
    ir.new_external(
        "kotlinx/serialization/ContextualSerializer",
        "(Lkotlin/reflect/KClass;)V",
        vec![kclass],
    )
}

/// `new PolymorphicSerializer(<base>::class)` — the element serializer for a star-projection / erased
/// `Any` type argument (`Box<*>` with `Box<T : E>` → `PolymorphicSerializer(E::class)`); its descriptor
/// `serialName` is `kotlinx.serialization.Polymorphic<E>`.
fn build_polymorphic_serializer(ir: &mut IrFile, base_internal: &str) -> ExprId {
    let classlit = ir.class_const(Some(base_internal));
    let kclass = ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: "kotlin/jvm/internal/Reflection".to_string(),
            name: "getOrCreateKotlinClass".to_string(),
            descriptor: "(Ljava/lang/Class;)Lkotlin/reflect/KClass;".to_string(),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![classlit],
    });
    ir.new_external(
        "kotlinx/serialization/PolymorphicSerializer",
        "(Lkotlin/reflect/KClass;)V",
        vec![kclass],
    )
}

/// Non-mutating mirror of [`element_serializer_expr`]: whether a property of type `ty` HAS a derivable
/// element serializer (nested @Serializable generic/non-generic, or a builtin). `deserialize` gates on
/// this so it stubs cleanly instead of emitting a `null` element serializer for an un-derivable type.
fn can_derive_element_serializer(ir: &IrFile, ty: &Ty) -> bool {
    let nn = ty.non_null();
    // Include de-erased primitives/`String` (mirrors `element_serializer_expr`): they own no `Obj`
    // internal name but ARE derivable via the `builtin_element_serializer` tail. The old `obj_internal()`
    // guard rejected them, so a `List<Int>` field looked un-derivable and `deserialize` stubbed out.
    let Some(fq_name) = ty.kotlin_class_internal().map(|n| n.render()) else {
        return false;
    };
    let type_args = nn.type_args();
    // A sealed `@Serializable` class uses `Class.serializer()` (a runtime SealedClassSerializer) — only
    // when the generated `serializer()` accessor exists (the class IS `@Serializable`).
    if ir.classes.iter().any(|c| {
        c.fq_name_matches(&fq_name)
            && c.is_sealed
            && c.methods
                .iter()
                .any(|&m| ir.functions[m as usize].name == "serializer")
    }) {
        return true;
    }
    // A standard COLLECTION field (mirrors `element_serializer_expr`): derivable iff every element type is.
    if let Some((_, n)) = collection_serializer_builder(&fq_name) {
        return type_args.len() >= n
            && type_args
                .iter()
                .take(n)
                .all(|a| can_derive_element_serializer(ir, a));
    }
    // An interface (any) / abstract-@Serializable class field → `PolymorphicSerializer` (mirrors
    // `element_serializer_expr`; a `@Serializable` sealed interface was claimed by the sealed branch above).
    if ir.classes.iter().any(|c| {
        c.fq_name_matches(&fq_name)
            && (c.is_interface
                || (c.is_abstract
                    && !c.is_sealed
                    && (c.companion_class.is_some()
                        || c.methods
                            .iter()
                            .any(|&m| ir.functions[m as usize].name == "serializer"))))
    }) {
        return true;
    }
    if ir
        .classes
        .iter()
        .any(|c| c.fq_name_matches(&serializer_fq(&fq_name)))
    {
        let n_tp = ir
            .classes
            .iter()
            .find(|c| c.fq_name_matches(&fq_name))
            .map(|c| c.type_params.len())
            .unwrap_or(0);
        if n_tp == 0 {
            return true;
        }
        let base = ir.classes.iter().find(|c| c.fq_name_matches(&fq_name));
        return type_args.len() >= n_tp
            && type_args.iter().take(n_tp).enumerate().all(|(i, a)| {
                if can_derive_element_serializer(ir, a) {
                    return true;
                }
                // An erased `Any` argument (star projection) on a BOUNDED type parameter becomes a
                // `PolymorphicSerializer` (mirrors `element_serializer_expr`).
                a.non_null()
                    .obj_internal()
                    .is_some_and(|n| n.matches("kotlin/Any"))
                    && base.is_some_and(|c| {
                        c.type_params.get(i).is_some_and(|name| {
                            c.type_param_bounds.iter().any(|(n, bt)| {
                                n == name
                                    && !bt
                                        .non_null()
                                        .obj_internal()
                                        .is_some_and(|n| n.matches("kotlin/Any"))
                            })
                        })
                    })
            });
    }
    builtin_element_serializer(ty).is_some()
}

fn builtin_element_key(ty: &Ty) -> Option<&'static str> {
    let fq = ty.kotlin_class_internal()?;
    Some(if fq.matches("java/lang/Integer") {
        "kotlin/Int"
    } else if fq.matches("java/lang/Long") {
        "kotlin/Long"
    } else if fq.matches("java/lang/Boolean") {
        "kotlin/Boolean"
    } else if fq.matches("java/lang/Double") {
        "kotlin/Double"
    } else if fq.matches("java/lang/Float") {
        "kotlin/Float"
    } else if fq.matches("java/lang/Character") {
        "kotlin/Char"
    } else if fq.matches("java/lang/Byte") {
        "kotlin/Byte"
    } else if fq.matches("java/lang/Short") {
        "kotlin/Short"
    } else if fq.matches("java/lang/String") {
        "kotlin/String"
    } else {
        return None;
    })
}

fn builtin_element_serializer(ty: &Ty) -> Option<&'static str> {
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
        _ => return None,
    })
}

/// If `ty` names a `@JvmInline value class` defined in this IR, its TERMINAL underlying type — how
/// krusty represents a value-class-typed field/value. Recurses through a value-class chain
/// (`A(val b: B)`, `B(val i: Int)` → `Int`), depth-bounded against a malformed cycle. `None` for any
/// type that isn't (transitively) a value class.
fn value_class_underlying(ir: &IrFile, ty: &Ty) -> Option<Ty> {
    fn rec(ir: &IrFile, ty: &Ty, depth: u32) -> Option<Ty> {
        if depth > 32 {
            return None;
        }
        let fq = ty.kotlin_class_internal()?;
        let rendered = fq.render();
        // A same-file value class: recurse through its declared single field.
        if let Some(c) = ir.classes.iter().find(|c| c.fq_name_matches(&rendered)) {
            if !c.is_value {
                return None;
            }
            let u = c.fields.first()?.ty;
            return Some(rec(ir, &u, depth + 1).unwrap_or(u));
        }
        // A CROSS-FILE `@JvmInline value class` (its `@Metadata`-decoded underlying, carrying nullability)
        // — so a serialized data class with an imported value-class field still recognizes it.
        if let Some(u) = ir.external_value_class_name(fq) {
            return Some(rec(ir, u, depth + 1).unwrap_or(*u));
        }
        None
    }
    rec(ir, ty, 0)
}

/// Plain `Encoder.encode*` / `Decoder.decode*` for a value class's underlying type (`encodeInt` /
/// `decodeInt`), as `(enc_name, enc_desc, dec_name, dec_desc)`. `None` for an unsupported underlying.
fn inline_prim_methods(
    ty: &Ty,
) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    Some(match builtin_element_key(ty)? {
        "kotlin/Int" => ("encodeInt", "(I)V", "decodeInt", "()I"),
        "kotlin/Long" => ("encodeLong", "(J)V", "decodeLong", "()J"),
        "kotlin/Boolean" => ("encodeBoolean", "(Z)V", "decodeBoolean", "()Z"),
        "kotlin/Double" => ("encodeDouble", "(D)V", "decodeDouble", "()D"),
        "kotlin/Float" => ("encodeFloat", "(F)V", "decodeFloat", "()F"),
        "kotlin/Char" => ("encodeChar", "(C)V", "decodeChar", "()C"),
        "kotlin/Byte" => ("encodeByte", "(B)V", "decodeByte", "()B"),
        "kotlin/Short" => ("encodeShort", "(S)V", "decodeShort", "()S"),
        "kotlin/String" => (
            "encodeString",
            "(Ljava/lang/String;)V",
            "decodeString",
            "()Ljava/lang/String;",
        ),
        _ => return None,
    })
}

impl SerializationPlugin {
    /// Add `static serializer(): KSerializer<C>` returning the explicit serializer `X` from
    /// `@Serializable(with = X::class)`. An `object` serializer (`object Other : KSerializer<…>`) is its
    /// `INSTANCE`; a class serializer (`ContextualSerializer`/`PolymorphicSerializer`, single `KClass`
    /// ctor) is `new X(Reflection.getOrCreateKotlinClass(C.class))`.
    fn add_custom_serializer_accessor(
        ir: &mut IrFile,
        class_id: u32,
        class_fq: &str,
        custom: &str,
    ) {
        // An OBJECT serializer has no public constructor — return its singleton `INSTANCE`.
        let inst = if let Some(oid) = ir
            .classes
            .iter()
            .position(|c| c.fq_name_matches(custom) && c.is_object)
        {
            ir.add_expr(IrExpr::StaticInstance {
                owner: oid as u32,
                ty: oid as u32,
                field: "INSTANCE",
            })
        } else {
            let classlit = ir.class_const(Some(class_fq));
            let kclass = ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: "kotlin/jvm/internal/Reflection".to_string(),
                    name: "getOrCreateKotlinClass".to_string(),
                    descriptor: "(Ljava/lang/Class;)Lkotlin/reflect/KClass;".to_string(),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: vec![classlit],
            });
            ir.new_external(custom, "(Lkotlin/reflect/KClass;)V", vec![kclass])
        };
        let ret = ir.add_expr(IrExpr::Return(Some(inst)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        // A custom-serializer / enum / sealed class keeps `serializer()` as a STATIC on the class for
        // now (the Companion relocation is done for plain data classes, whose emitter path honors
        // `companion_class`; the enum/interface emitters do not yet).
        let accessor = ir.add_fun(IrFunction {
            name: "serializer".to_string(),
            params: vec![],
            ret: kserializer_of(class_ty(class_fq)),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.classes[class_id as usize].methods.push(accessor);
    }

    /// Modern (`kotlinx.serialization` ≥ 1.5) `@Serializable enum` codegen: `serializer()` lives on a
    /// nested `Companion` and returns a runtime `EnumSerializer(name, E.values())` cached in a
    /// `private static final Lazy $cachedSerializer$delegate` on the enum, read through a
    /// `public static final Lazy access$get$cachedSerializer$delegate$cp()` accessor. Matches kotlinc's
    /// member set (Companion class + `Companion` field + the accessor; no static `serializer()`).
    fn add_enum_serializer_companion(ir: &mut IrFile, class_id: u32, class_fq: &str) {
        let lazy_ty = class_ty("kotlin/Lazy");
        // `$cachedSerializer$delegate = LazyKt.lazyOf(new EnumSerializer(<name>, E.values()))`.
        let name = ir.add_expr(IrExpr::Const(IrConst::String(class_fq.replace('/', "."))));
        let values = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: class_fq.to_string(),
                name: "values".to_string(),
                descriptor: format!("()[L{class_fq};"),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![],
        });
        let enum_ser = ir.new_external(
            "kotlinx/serialization/internal/EnumSerializer",
            "(Ljava/lang/String;[Ljava/lang/Enum;)V",
            vec![name, values],
        );
        let lazy = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: "kotlin/LazyKt".to_string(),
                name: "lazyOf".to_string(),
                descriptor: "(Ljava/lang/Object;)Lkotlin/Lazy;".to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![enum_ser],
        });
        ir.statics.push(crate::ir::IrStatic {
            name: "$cachedSerializer$delegate".to_string(),
            ty: lazy_ty,
            init: lazy,
            is_var: false,
            is_const: false,
            owner: Some(type_name(class_fq)),
            visibility: crate::types::Visibility::Private,
            custom_accessor: true,
        });
        // `public static final Lazy access$get$cachedSerializer$delegate$cp()` — reads the private field.
        let read =
            ir.external_static_field(class_fq, "$cachedSerializer$delegate", "Lkotlin/Lazy;");
        let acc_ret = ir.add_expr(IrExpr::Return(Some(read)));
        let acc_body = ir.add_expr(IrExpr::Block {
            stmts: vec![acc_ret],
            value: None,
        });
        let acc = ir.add_fun(IrFunction {
            name: "access$get$cachedSerializer$delegate$cp".to_string(),
            params: vec![],
            ret: lazy_ty,
            body: Some(acc_body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.synthetic_methods.insert(acc);
        ir.classes[class_id as usize].methods.push(acc);
        // `Companion.serializer()` returns `(KSerializer) access$…$cp().getValue()` (the cached instance).
        let acc_call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: class_fq.to_string(),
                name: "access$get$cachedSerializer$delegate$cp".to_string(),
                descriptor: "()Lkotlin/Lazy;".to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![],
        });
        let get_value = ir.add_expr(IrExpr::Call {
            callee: Callee::CrossFileVirtual {
                owner: "kotlin/Lazy".to_string(),
                name: "getValue".to_string(),
                params: vec![],
                ret: class_ty("kotlin/Any"),
                interface: true,
            },
            dispatch_receiver: Some(acc_call),
            args: vec![],
        });
        let cast = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: get_value,
            type_operand: Ty::obj("kotlinx/serialization/KSerializer"),
        });
        let sret = ir.add_expr(IrExpr::Return(Some(cast)));
        let sbody = ir.add_expr(IrExpr::Block {
            stmts: vec![sret],
            value: None,
        });
        place_serializer_accessor(
            ir,
            class_id,
            class_fq,
            vec![],
            kserializer_of(class_ty(class_fq)),
            sbody,
        );
    }

    /// `getOrCreateKotlinClass(<internal>.class)` — a `KClass` literal for an internal class name.
    fn kclass_literal(ir: &mut IrFile, internal: &str) -> ExprId {
        let classlit = ir.class_const(Some(internal));
        ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: "kotlin/jvm/internal/Reflection".to_string(),
                name: "getOrCreateKotlinClass".to_string(),
                descriptor: "(Ljava/lang/Class;)Lkotlin/reflect/KClass;".to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![classlit],
        })
    }

    /// Add `static serializer(): KSerializer<C>` for a `@Serializable sealed class`/`sealed interface`,
    /// returning a runtime `SealedClassSerializer(serialName, C::class, [Sub::class…], [Sub.serializer()…])`
    /// over its `@Serializable` direct subclasses — the way kotlinc compiles a sealed polymorphic base.
    fn add_sealed_serializer_accessor(
        ir: &mut IrFile,
        ctx: &PluginContext<'_>,
        class_id: u32,
        class_fq: &str,
    ) {
        // Direct `@Serializable` subclasses: a `class … : C(…)` (superclass == C) or `… : C` (C in its
        // interface list), in declaration order — the order kotlinc registers them.
        let subs: Vec<u32> = ctx
            .classes_with_simple("Serializable")
            .into_iter()
            .filter(|&cid| cid != class_id)
            .filter(|&cid| {
                let c = &ir.classes[cid as usize];
                c.superclass_matches(class_fq) || c.interfaces.iter().any(|i| i.matches(class_fq))
            })
            .collect();

        let serial_name = ir.add_expr(IrExpr::Const(IrConst::String(class_fq.replace('/', "."))));
        let base_kclass = Self::kclass_literal(ir, class_fq);
        let sub_kclasses: Vec<ExprId> = subs
            .iter()
            .map(|&cid| Self::kclass_literal(ir, &ir.classes[cid as usize].fq_name()))
            .collect();
        let sub_serializers: Vec<ExprId> = subs
            .iter()
            .map(|&cid| {
                let s = ir.classes[cid as usize].fq_name();
                let companion_routed = {
                    let c = &ir.classes[cid as usize];
                    c.type_params.is_empty()
                        && c.enum_entries.is_empty()
                        && !c.is_sealed
                        && !c.is_object
                        && custom_serializer_of(ctx, ir, cid).is_none()
                };
                if companion_routed {
                    companion_serializer_call(ir, &s, vec![], kserializer_of(class_ty(&s)), vec![])
                } else {
                    serializer_of(ir, &s, vec![], kserializer_of(class_ty(&s)), vec![])
                }
            })
            .collect();
        let kclass_arr = ir.add_expr(IrExpr::Vararg {
            array_type: Ty::obj_args("kotlin/Array", &[class_ty("kotlin/reflect/KClass")]),
            elements: sub_kclasses,
        });
        let ser_arr = ir.add_expr(IrExpr::Vararg {
            array_type: Ty::obj_args("kotlin/Array", &[kserializer_of(class_ty("kotlin/Any"))]),
            elements: sub_serializers,
        });
        let inst = ir.new_external(
            "kotlinx/serialization/SealedClassSerializer",
            "(Ljava/lang/String;Lkotlin/reflect/KClass;[Lkotlin/reflect/KClass;[Lkotlinx/serialization/KSerializer;)V",
            vec![serial_name, base_kclass, kclass_arr, ser_arr],
        );
        let ret = ir.add_expr(IrExpr::Return(Some(inst)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        // Sealed keeps `serializer()` STATIC on the class (Companion relocation is data-class-only for now).
        let accessor = ir.add_fun(IrFunction {
            name: "serializer".to_string(),
            params: vec![],
            ret: kserializer_of(class_ty(class_fq)),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.classes[class_id as usize].methods.push(accessor);
    }

    /// Add a method to `ir` and return its `FunId`.
    fn add_method(
        ir: &mut IrFile,
        owner_fq: &str,
        name: &str,
        params: Vec<Ty>,
        ret: Ty,
        body: Option<ExprId>,
    ) -> u32 {
        ir.add_fun(IrFunction {
            name: name.to_string(),
            params,
            ret,
            body,
            is_static: false,
            dispatch_receiver: Some(owner_fq.to_string()),
            param_checks: Vec::new(),
        })
    }
}

impl IrPlugin for SerializationPlugin {
    fn name(&self) -> &str {
        "kotlinx.serialization"
    }

    /// Generate the `$serializer` object, its members, and the `serializer()` accessor.
    fn generate_declarations(&self, ir: &mut IrFile, ctx: &PluginContext<'_>) {
        for class_id in ctx.classes_with_simple("Serializable") {
            let class_fq = ir.classes[class_id as usize].fq_name();
            // `@Serializable(with = X::class)`: no generated `$serializer` — `serializer()` returns an
            // instance of the explicit serializer `X` (`new X(C::class)`), the way kotlinc compiles it.
            if let Some(custom) = custom_serializer_of(ctx, ir, class_id) {
                Self::add_custom_serializer_accessor(ir, class_id, &class_fq, &custom);
                continue;
            }
            // A `@Serializable enum`: no generated `$serializer` — `serializer()` lives on a nested
            // `Companion` and returns a runtime `EnumSerializer(name, E.values())` cached in a `Lazy`.
            if !ir.classes[class_id as usize].enum_entries.is_empty() {
                Self::add_enum_serializer_companion(ir, class_id, &class_fq);
                continue;
            }
            // A `@Serializable sealed class`/`sealed interface`: no generated `$serializer` —
            // `serializer()` returns a runtime `SealedClassSerializer` over its `@Serializable` subclasses
            // (polymorphic, `"type"` discriminator), the way kotlinc compiles a sealed base.
            if ir.classes[class_id as usize].is_sealed {
                Self::add_sealed_serializer_accessor(ir, ctx, class_id, &class_fq);
                continue;
            }
            let ser_fq = serializer_fq(&class_fq);

            // Member signatures; bodies are filled in `transform_bodies`.
            let descriptor = Self::add_method(
                ir,
                &ser_fq,
                "getDescriptor",
                vec![],
                class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
                None,
            );
            let serialize = Self::add_method(
                ir,
                &ser_fq,
                "serialize",
                vec![
                    class_ty("kotlinx/serialization/encoding/Encoder"),
                    class_ty(&class_fq),
                ],
                unit(),
                None,
            );
            let deserialize = Self::add_method(
                ir,
                &ser_fq,
                "deserialize",
                vec![class_ty("kotlinx/serialization/encoding/Decoder")],
                class_ty(&class_fq),
                None,
            );
            let child = Self::add_method(
                ir,
                &ser_fq,
                "childSerializers",
                vec![],
                Ty::obj_args("kotlin/Array", &[kserializer_of(class_ty("kotlin/Any"))]),
                None,
            );
            // `typeParametersSerializers(): KSerializer<?>[]` is filled in `transform_bodies`.
            let tps_arr = ir.add_expr(IrExpr::Vararg {
                array_type: Ty::obj_args("kotlin/Array", &[kserializer_of(class_ty("kotlin/Any"))]),
                elements: vec![],
            });
            let tps_ret = ir.add_expr(IrExpr::Return(Some(tps_arr)));
            let tps_body = ir.add_expr(IrExpr::Block {
                stmts: vec![tps_ret],
                value: None,
            });
            let type_params_ser = Self::add_method(
                ir,
                &ser_fq,
                "typeParametersSerializers",
                vec![],
                Ty::obj_args("kotlin/Array", &[kserializer_of(class_ty("kotlin/Any"))]),
                Some(tps_body),
            );
            // kotlinc emits it non-final (a plain `GeneratedSerializer` override).
            ir.open_methods.insert(type_params_ser);
            ir.bridge_methods.insert(type_params_ser);

            let foo_fields: Vec<(String, Ty)> = ir.classes[class_id as usize]
                .fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.clone()))
                .collect();
            // Per-property constant default (`Some` ⇒ the element is `isOptional` — omitted on encode when
            // it still equals the default).
            let foo_defaults: Vec<Option<IrConst>> = ir.classes[class_id as usize]
                .fields
                .iter()
                .map(|f| f.default.clone())
                .collect();
            // Generic `@Serializable class C<T…>`: the `$serializer` is a CLASS (not a singleton object)
            // with one `KSerializer` constructor parameter per type parameter (`typeSerialK`, stored at
            // fields `1..=N`, after the `descriptor` field 0), used as the element serializer for any
            // type-parameter-typed property. A non-generic class keeps the singleton-object form.
            let type_params: Vec<String> = ir.classes[class_id as usize].type_params.clone();
            let n_tp = type_params.len();
            let is_generic = n_tp > 0;
            let mut ser = synthetic_class(&ser_fq);
            ser.is_object = !is_generic; // non-generic `$serializer` is a singleton object (INSTANCE)
                                         // Implement `GeneratedSerializer` (extends `KSerializer`) — it declares `childSerializers()`
                                         // (we generate it) and a DEFAULT `typeParametersSerializers()`, and it lets the descriptor
                                         // (built with `this` below) derive element descriptors for `getElementDescriptor`/introspection.
            ser.interfaces = vec![crate::types::type_name(GENERATED_SERIALIZER_FQ)].into();
            ser.supertypes = vec![kserializer_of(class_ty(&class_fq))];
            // Field 0 is the `descriptor` (a `PluginGeneratedSerialDescriptor`), built in <init>. A generic
            // serializer adds one `KSerializer` field per type parameter (`typeSerial0..N` at fields 1..=N),
            // set from the constructor parameters.
            // Field 0 `descriptor` + each `typeSerial{k}` are `final` private fields.
            ser.fields = vec![crate::ir::IrField {
                is_final: true,
                ..crate::ir::IrField::new(
                    "descriptor".to_string(),
                    class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
                )
            }];
            for k in 0..n_tp {
                ser.fields.push(crate::ir::IrField {
                    is_final: true,
                    ..crate::ir::IrField::new(
                        format!("typeSerial{k}"),
                        kserializer_of(class_ty("kotlin/Any")),
                    )
                });
            }
            ser.ctor_param_count = n_tp as u32;
            // The N constructor params are type-param serializers (`is_field=false`): stored manually in
            // <init> to fields `1..=N`; field 0 is the descriptor, built in <init>.
            ser.ctor_args = vec![
                IrCtorArg {
                    ty: kserializer_of(class_ty("kotlin/Any")),
                    is_field: false,
                    check: None,
                };
                n_tp
            ];
            ser.methods = vec![descriptor, serialize, deserialize, child, type_params_ser];
            // Erased generic bridges the `KSerializer<Foo>` interface requires: the JVM sees
            // `serialize(Encoder, Object)` / `deserialize(Decoder): Object`; each adapts args/return
            // and delegates to the concrete `Foo`-typed override.
            ser.bridges = vec![
                crate::ir::Bridge {
                    name: "serialize".to_string(),
                    erased_params: vec![
                        class_ty("kotlinx/serialization/encoding/Encoder"),
                        class_ty("kotlin/Any"),
                    ],
                    erased_ret: unit(),
                    concrete_params: vec![
                        class_ty("kotlinx/serialization/encoding/Encoder"),
                        class_ty(&class_fq),
                    ],
                    concrete_ret: unit(),
                    target_name: None,
                    box_ret: None,
                    unbox_params: Vec::new(),
                },
                crate::ir::Bridge {
                    name: "deserialize".to_string(),
                    erased_params: vec![class_ty("kotlinx/serialization/encoding/Decoder")],
                    erased_ret: class_ty("kotlin/Any"),
                    concrete_params: vec![class_ty("kotlinx/serialization/encoding/Decoder")],
                    concrete_ret: class_ty(&class_fq),
                    target_name: None,
                    box_ret: None,
                    unbox_params: Vec::new(),
                },
            ];
            ir.mark_synthetic_class(&ser_fq);
            ir.mark_deprecated_class(&ser_fq);
            let ser_id = ir.add_class(ser);

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
                && foo_fields
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
                let name = ir.add_expr(IrExpr::Const(IrConst::String(class_fq.replace('/', "."))));
                let under_ser = foo_fields
                    .first()
                    .and_then(|(_, t)| builtin_element_serializer(t));
                let ser_inst = match under_ser {
                    Some(s) => ir.external_static_instance(s, s, "INSTANCE"),
                    // Unsupported underlying (e.g. a nested @Serializable) — leave the default below.
                    None => ir.add_expr(IrExpr::Const(IrConst::Null)),
                };
                let d = ir.add_expr(IrExpr::Call {
                    callee: Callee::Static {
                        owner: "kotlinx/serialization/internal/InlineClassDescriptorKt".to_string(),
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
                let pgsd_name =
                    ir.add_expr(IrExpr::Const(IrConst::String(class_fq.replace('/', "."))));
                // Pass `this` (the `$serializer`, a `GeneratedSerializer`) so the descriptor can derive
                // element descriptors from `childSerializers()` (`getElementDescriptor`/introspection).
                let pgsd_self = ir.add_expr(IrExpr::GetValue(0));
                let pgsd_n = ir.add_expr(IrExpr::Const(IrConst::Int(foo_fields.len() as i32)));
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
                for (i, (pname, _)) in foo_fields.iter().enumerate() {
                    let d = ir.add_expr(IrExpr::GetValue(desc_local));
                    let element_name =
                        serial_name_of(ctx, ir, class_id, pname).unwrap_or_else(|| pname.clone());
                    let nm = ir.add_expr(IrExpr::Const(IrConst::String(element_name)));
                    // A property with a constant default is an OPTIONAL element (`addElement(name, true)`).
                    let is_optional = foo_defaults.get(i).is_some_and(|d| d.is_some());
                    let opt = ir.add_expr(IrExpr::Const(IrConst::Boolean(is_optional)));
                    init_stmts.push(ir.add_expr(IrExpr::Call {
                        callee: Callee::Virtual {
                            owner: pgsd_internal.to_string(),
                            name: "addElement".to_string(),
                            descriptor: "(Ljava/lang/String;Z)V".to_string(),
                            interface: false,
                        },
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
            let (acc_params, acc_body) = if is_generic {
                let args: Vec<ExprId> = (0..n_tp)
                    .map(|k| ir.add_expr(IrExpr::GetValue(k as u32)))
                    .collect();
                let new_ser = ir.add_expr(IrExpr::New {
                    class: ser_id,
                    args,
                    ctor_params: Some(vec![kserializer_of(class_ty("kotlin/Any")); n_tp]),
                });
                let ret = ir.add_expr(IrExpr::Return(Some(new_ser)));
                (
                    vec![kserializer_of(class_ty("kotlin/Any")); n_tp],
                    ir.add_expr(IrExpr::Block {
                        stmts: vec![ret],
                        value: None,
                    }),
                )
            } else {
                let inst = ir.add_expr(IrExpr::StaticInstance {
                    owner: ser_id,
                    ty: ser_id,
                    field: "INSTANCE",
                });
                let inst_ret = ir.add_expr(IrExpr::Return(Some(inst)));
                (
                    vec![],
                    ir.add_expr(IrExpr::Block {
                        stmts: vec![inst_ret],
                        value: None,
                    }),
                )
            };
            // kotlinc puts `serializer()` on the class's `Companion` object (not a static on the class).
            // Scoped to a PLAIN non-generic NON-object data class for now — a generic class's
            // `serializer(KSerializer…)` on the Companion still has a dispatch gap, and an `object` is its
            // own singleton (a synthesized Companion would duplicate its `<clinit>`), so both keep the
            // static form (see `serializer_of`).
            if is_generic || ir.classes[class_id as usize].is_object {
                let accessor = ir.add_fun(IrFunction {
                    name: "serializer".to_string(),
                    params: acc_params,
                    ret: kserializer_of(class_ty(&class_fq)),
                    body: Some(acc_body),
                    is_static: true,
                    dispatch_receiver: None,
                    param_checks: Vec::new(),
                });
                ir.classes[class_id as usize].methods.push(accessor);
            } else {
                place_serializer_accessor(
                    ir,
                    class_id,
                    &class_fq,
                    acc_params,
                    kserializer_of(class_ty(&class_fq)),
                    acc_body,
                );
            }

            // A `@Serializable` class's shape drives which synthetic members kotlinc emits: a plain data
            // class gets `write$Self` + the deserialization ctor, whereas a value class (inline serializer,
            // `constructor-impl` only) and an object are compiled differently and get neither.
            let plain_data_class =
                !ir.classes[class_id as usize].is_value && !ir.classes[class_id as usize].is_object;

            // `write$Self` helper — its NAME is ABI-version-dependent (mangled with the module name
            // on core >= 1.6). Emitted on the serialized class as a static member with a concrete
            // (no-op) body so the class stays concrete (an empty-body method would force it abstract).
            // A value class serializes its sole underlying value inline (no `write$Self`), and an object
            // uses an `ObjectSerializer`; kotlinc emits `write$Self` for a plain data class only.
            if plain_data_class {
                let ws_ret = ir.add_expr(IrExpr::Return(None));
                let ws_body = ir.add_expr(IrExpr::Block {
                    stmts: vec![ws_ret],
                    value: None,
                });
                let write_self = ir.add_fun(IrFunction {
                    name: self.write_self_name(),
                    params: vec![
                        class_ty(&class_fq),
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

            // ABI-only deserialization ctor `G(int seqMask, <fields…>, SerializationConstructorMarker)`
            // (krusty's `deserialize()` uses the primary ctor): a Super-delegate secondary ctor whose body
            // sets each field from its arg (fields start at param 2, after the seq-mask). kotlinc marks it
            // `ACC_SYNTHETIC`. A value-class field is stored UNBOXED, so its param + `putfield` use the
            // underlying type.
            if plain_data_class {
                // A value-class field unboxes to its underlying in the deser ctor ONLY when that underlying
                // is NON-nullable; a nullable-underlying value class (`VC(val s: String?)`) stays BOXED
                // (its marker-style synthetic ctor can't unbox — kotlinc keeps the `VC` type).
                let deser_field_tys: Vec<Ty> = foo_fields
                    .iter()
                    .map(|(_, ty)| match value_class_underlying(ir, ty) {
                        Some(u) if !u.is_nullable() => u,
                        _ => *ty,
                    })
                    .collect();
                // kotlinc always emits one seen-mask slot past the last full 32-field group.
                let n_masks = foo_fields.len() / 32 + 1;
                let mut deser_params = vec![Ty::Int; n_masks];
                deser_params.extend(deser_field_tys.iter().cloned());
                deser_params.push(class_ty(
                    "kotlinx/serialization/internal/SerializationConstructorMarker",
                ));
                // A value-class field is stored unboxed, so kotlinc treats the deser ctor as a
                // value-class-param ctor and appends a trailing `DefaultConstructorMarker` (its ABI
                // disambiguator — mirrors the primary ctor's `value_param_ctors` marker accessor).
                let has_value_field = foo_fields
                    .iter()
                    .any(|(_, ty)| value_class_underlying(ir, ty).is_some());
                if has_value_field {
                    deser_params.push(class_ty("kotlin/jvm/internal/DefaultConstructorMarker"));
                }
                let this = ir.add_expr(IrExpr::GetValue(0));
                let mut deser_stmts = Vec::with_capacity(foo_fields.len());
                for i in 0..foo_fields.len() {
                    // `this` at 0, the `n_masks` seq-mask ints at 1..=n_masks, then the field params.
                    let arg = ir.add_expr(IrExpr::GetValue(i as u32 + 1 + n_masks as u32));
                    deser_stmts.push(ir.add_expr(IrExpr::SetField {
                        receiver: this,
                        class: class_id,
                        index: i as u32,
                        value: arg,
                    }));
                }
                let deser_body = ir.add_expr(IrExpr::Block {
                    stmts: deser_stmts,
                    value: None,
                });
                ir.classes[class_id as usize]
                    .secondary_ctors
                    .push(crate::ir::IrSecondaryCtor {
                        params: deser_params,
                        delegate_args: vec![],
                        body: Some(deser_body),
                        delegate: crate::ir::CtorDelegateTarget::Super,
                        synthetic: true,
                    });
            }

            // `$childSerializers` cache — a `private static final Lazy[]` + the public synthetic
            // `access$get$childSerializers$cp()` accessor kotlinc emits when a prop's serializer is
            // ALLOCATED rather than a singleton: a collection (`ArrayListSerializer(…)`) or an ENUM
            // (`EnumSerializer(…)`). A primitive/`String` (singleton `INSTANCE`) or a nested `@Serializable`
            // CLASS (singleton `$$serializer.INSTANCE`) is NOT cached. Each slot is `LazyKt.lazyOf(…)`, else null.
            let enum_internals: std::collections::HashSet<String> = ir
                .classes
                .iter()
                .filter(|c| !c.enum_entries.is_empty())
                .map(|c| c.fq_name())
                .collect();
            let needs_cache = |ty: &Ty| -> bool {
                let internal = ty.kotlin_class_internal();
                internal.map(|i| i.render()).is_some_and(|i| {
                    collection_serializer_builder(&i).is_some() || enum_internals.contains(&i)
                })
            };
            if plain_data_class && foo_fields.iter().any(|(_, ty)| needs_cache(ty)) {
                let lazy_arr_ty = Ty::obj_args("kotlin/Array", &[class_ty("kotlin/Lazy")]);
                let elems: Vec<ExprId> = foo_fields
                    .iter()
                    .map(|(_, ty)| {
                        if needs_cache(ty) {
                            if let Some(es) = element_serializer_expr(ir, ty) {
                                return ir.add_expr(IrExpr::Call {
                                    callee: Callee::Static {
                                        owner: "kotlin/LazyKt".to_string(),
                                        name: "lazyOf".to_string(),
                                        descriptor: "(Ljava/lang/Object;)Lkotlin/Lazy;".to_string(),
                                        inline: InlineKind::None,
                                    },
                                    dispatch_receiver: None,
                                    args: vec![es],
                                });
                            }
                        }
                        ir.add_expr(IrExpr::Const(IrConst::Null))
                    })
                    .collect();
                let arr = ir.add_expr(IrExpr::Vararg {
                    array_type: lazy_arr_ty,
                    elements: elems,
                });
                ir.statics.push(crate::ir::IrStatic {
                    name: "$childSerializers".to_string(),
                    ty: lazy_arr_ty,
                    init: arr,
                    is_var: false,
                    is_const: false,
                    owner: Some(type_name(&class_fq)),
                    visibility: crate::types::Visibility::Private,
                    custom_accessor: true,
                });
                let read =
                    ir.external_static_field(&class_fq, "$childSerializers", "[Lkotlin/Lazy;");
                let ret = ir.add_expr(IrExpr::Return(Some(read)));
                let body = ir.add_expr(IrExpr::Block {
                    stmts: vec![ret],
                    value: None,
                });
                let acc = ir.add_fun(IrFunction {
                    name: "access$get$childSerializers$cp".to_string(),
                    params: vec![],
                    ret: lazy_arr_ty,
                    body: Some(body),
                    is_static: true,
                    dispatch_receiver: None,
                    param_checks: Vec::new(),
                });
                ir.synthetic_methods.insert(acc);
                ir.classes[class_id as usize].methods.push(acc);
            }
        }
    }

    /// IR backend generation: fill `childSerializers` with a real per-field element-serializer array
    /// (arity == field count), and `serialize`/`deserialize` with placeholder `return` bodies.
    fn transform_bodies(&self, ir: &mut IrFile, ctx: &PluginContext<'_>) {
        // Specialize the generic reified-serialization placeholders core emitted in user bodies
        // (`fmt.encodeToString(x)` / `fmt.decodeFromString<C>(s)`) into the concrete `StringFormat`
        // member calls — the kotlinx descriptors live here, not in core lowering.
        specialize_reified_placeholders(ir);
        for class_id in ctx.classes_with_simple("Serializable") {
            // `@Serializable(with=X)` classes have no generated `$serializer` to fill (handled wholly in
            // `generate_declarations`).
            if custom_serializer_of(ctx, ir, class_id).is_some()
                || ir.classes[class_id as usize].is_sealed
                || !ir.classes[class_id as usize].enum_entries.is_empty()
            {
                continue;
            }
            // krusty UNBOXES a `@JvmInline value class`-typed field to its underlying (`Holder.f: Foo`
            // is emitted as `int`, `getF()I`, `new Holder(int)`). So serialize/deserialize must treat
            // such a field AS its underlying type — encode/decode the primitive directly (same JSON as
            // kotlinc's inline serializer), not via the (boxed) `<Foo>$serializer` which would store a
            // `Foo` reference into the unboxed slot (VerifyError).
            let fields: Vec<(String, Ty)> = ir.classes[class_id as usize]
                .fields
                .iter()
                .map(|f| {
                    (
                        f.name.clone(),
                        value_class_underlying(ir, &f.ty).unwrap_or(f.ty.clone()),
                    )
                })
                .collect();
            let field_types: Vec<Ty> = fields.iter().map(|(_, ty)| ty.clone()).collect();
            // Per-property constant default (`Some` ⇒ OPTIONAL — serialize omits it when it still equals
            // the default, via `shouldEncodeElementDefault(desc,i) || value.x != default`).
            let field_defaults: Vec<Option<IrConst>> = ir.classes[class_id as usize]
                .fields
                .iter()
                .map(|f| f.default.clone())
                .collect();
            let class_internal = ir.classes[class_id as usize].fq_name();
            let ser_fq = serializer_fq(&class_internal);
            let Some(ser_idx) = ir.classes.iter().position(|c| c.fq_name_matches(&ser_fq)) else {
                continue;
            };
            // The serialized class's ClassId (for constructing it in `deserialize`).
            let foo_id = class_id;
            // For each field: the ClassId of its element `$serializer` if the field's type is itself a
            // `@Serializable` class krusty generated a serializer for (nested/composite), else None.
            let nested: Vec<Option<u32>> = fields
                .iter()
                .map(|(_, ty)| match ty.non_null().obj_internal() {
                    Some(fq_name) => {
                        let s = serializer_fq(&fq_name.render());
                        ir.classes
                            .iter()
                            .position(|c| c.fq_name_matches(&s))
                            .map(|i| i as u32)
                    }
                    None => None,
                })
                .collect();
            // For each field: the `$serializer` field index (`1..=N`) holding its element serializer when
            // the property's declared type IS a class type parameter (`val boxed: T` on a generic class).
            // `None` for a concrete-typed field. Lets serialize/deserialize/childSerializers route a
            // type-param element through the ctor-supplied `this.typeSerialK` instead of a fixed serializer.
            let class_type_params: Vec<String> = ir.classes[class_id as usize].type_params.clone();
            let field_tps: Vec<Option<String>> = ir.classes[class_id as usize]
                .fields
                .iter()
                .map(|f| f.type_param.clone())
                .collect();
            let tp_field: Vec<Option<u32>> = (0..fields.len())
                .map(|i| {
                    field_tps.get(i).and_then(|o| o.as_ref()).and_then(|tp| {
                        class_type_params
                            .iter()
                            .position(|t| t == tp)
                            .map(|k| 1 + k as u32)
                    })
                })
                .collect();
            for fid in ir.classes[ser_idx].methods.clone() {
                match ir.functions[fid as usize].name.as_str() {
                    "getDescriptor" => {
                        // return this.descriptor
                        let recv = ir.add_expr(IrExpr::GetValue(0));
                        let d = ir.add_expr(IrExpr::GetField {
                            receiver: recv,
                            class: ser_idx as u32,
                            index: 0,
                        });
                        let ret = ir.add_expr(IrExpr::Return(Some(d)));
                        let body = ir.add_expr(IrExpr::Block {
                            stmts: vec![ret],
                            value: None,
                        });
                        ir.functions[fid as usize].body = Some(body);
                    }
                    "serialize"
                        if ir.classes[foo_id as usize].is_value
                            && fields
                                .first()
                                .and_then(|(_, t)| inline_prim_methods(t))
                                .is_some() =>
                    {
                        // A `@JvmInline value class`: serialize inline, no CompositeEncoder —
                        //   encoder.encodeInline(this.descriptor).encode<U>(value.get<prop>())
                        let (pname, uty) = fields[0].clone();
                        let (enc_name, enc_desc, _, _) = inline_prim_methods(&uty).unwrap();
                        let this = ir.add_expr(IrExpr::GetValue(0));
                        let desc = ir.add_expr(IrExpr::GetField {
                            receiver: this,
                            class: ser_idx as u32,
                            index: 0,
                        });
                        let enc = ir.add_expr(IrExpr::GetValue(1));
                        let inline_enc = ir.add_expr(IrExpr::Call {
                            callee: virtual_iface(
                                "kotlinx/serialization/encoding/Encoder",
                                "encodeInline",
                                "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/Encoder;",
                            ),
                            dispatch_receiver: Some(enc),
                            args: vec![desc],
                        });
                        let vrecv = ir.add_expr(IrExpr::GetValue(2));
                        let Some(getter_desc) = ty_descriptor(ctx, &uty) else {
                            continue;
                        };
                        let v = ir.add_expr(IrExpr::Call {
                            callee: Callee::Virtual {
                                owner: class_internal.clone(),
                                name: property_getter_name(&pname),
                                descriptor: format!("(){getter_desc}"),
                                interface: false,
                            },
                            dispatch_receiver: Some(vrecv),
                            args: vec![],
                        });
                        let call = ir.add_expr(IrExpr::Call {
                            callee: virtual_iface(
                                "kotlinx/serialization/encoding/Encoder",
                                enc_name,
                                enc_desc,
                            ),
                            dispatch_receiver: Some(inline_enc),
                            args: vec![v],
                        });
                        let ret = ir.add_expr(IrExpr::Return(None));
                        let body = ir.add_expr(IrExpr::Block {
                            stmts: vec![call, ret],
                            value: None,
                        });
                        ir.functions[fid as usize].body = Some(body);
                    }
                    "serialize" => {
                        // serialize(encoder=1, value=2): drive the CompositeEncoder per property.
                        //   val c = encoder.beginStructure(descriptor)        [local 3]
                        //   c.encode<T>Element(descriptor, i, value.<prop_i>)
                        //   c.endStructure(descriptor)
                        let ser_cid = ser_idx as u32;
                        let this_desc = |ir: &mut IrFile| -> ExprId {
                            let r = ir.add_expr(IrExpr::GetValue(0));
                            ir.add_expr(IrExpr::GetField {
                                receiver: r,
                                class: ser_cid,
                                index: 0,
                            })
                        };
                        let enc = ir.add_expr(IrExpr::GetValue(1));
                        let d0 = this_desc(ir);
                        let begin = ir.add_expr(IrExpr::Call {
                            callee: virtual_iface(
                                "kotlinx/serialization/encoding/Encoder",
                                "beginStructure",
                                "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/CompositeEncoder;",
                            ),
                            dispatch_receiver: Some(enc),
                            args: vec![d0],
                        });
                        let cvar = ir.add_expr(IrExpr::Variable {
                            index: 3,
                            ty: class_ty("kotlinx/serialization/encoding/CompositeEncoder"),
                            init: Some(begin),
                            named: false,
                        });
                        let mut stmts = vec![cvar];
                        let mut bail = false;
                        for (i, (pname, ty)) in fields.iter().enumerate() {
                            let n_before = stmts.len();
                            let d = this_desc(ir);
                            let idx = ir.add_expr(IrExpr::Const(IrConst::Int(i as i32)));
                            // Read the property via its PUBLIC getter (`value.getX()`) — the backing
                            // field is private, so a separate `$serializer` class can't read it directly.
                            let vrecv = ir.add_expr(IrExpr::GetValue(2));
                            let Some(getter_desc) = ty_descriptor(ctx, ty) else {
                                bail = true;
                                break;
                            };
                            let v = ir.add_expr(IrExpr::Call {
                                callee: Callee::Virtual {
                                    owner: class_internal.clone(),
                                    name: property_getter_name(pname),
                                    descriptor: format!("(){getter_desc}"),
                                    interface: false,
                                },
                                dispatch_receiver: Some(vrecv),
                                args: vec![],
                            });
                            let c = ir.add_expr(IrExpr::GetValue(3));
                            if let Some(inst) = contextual_serializer_for(
                                ir,
                                property_is_contextual(ctx, ir, class_id, pname),
                                ty,
                            ) {
                                // Contextual element: encode[Nullable]SerializableElement(desc, i,
                                // ContextualSerializer(<type>::class), value.getX()).
                                let method = if is_nullable(ty) {
                                    "encodeNullableSerializableElement"
                                } else {
                                    "encodeSerializableElement"
                                };
                                stmts.push(ir.add_expr(IrExpr::Call {
                                    callee: virtual_iface(
                                        "kotlinx/serialization/encoding/CompositeEncoder",
                                        method,
                                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                                    ),
                                    dispatch_receiver: Some(c),
                                    args: vec![d, idx, inst, v],
                                }));
                            } else if let Some(fidx) = tp_field[i] {
                                // Type-parameter element: encode[Nullable]SerializableElement(desc, i,
                                // this.typeSerialK, value.getX()) — the serializer is the ctor-supplied one.
                                let this_s = ir.add_expr(IrExpr::GetValue(0));
                                let inst = ir.add_expr(IrExpr::GetField {
                                    receiver: this_s,
                                    class: ser_idx as u32,
                                    index: fidx,
                                });
                                let method = if is_nullable(ty) {
                                    "encodeNullableSerializableElement"
                                } else {
                                    "encodeSerializableElement"
                                };
                                stmts.push(ir.add_expr(IrExpr::Call {
                                    callee: virtual_iface(
                                        "kotlinx/serialization/encoding/CompositeEncoder",
                                        method,
                                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                                    ),
                                    dispatch_receiver: Some(c),
                                    args: vec![d, idx, inst, v],
                                }));
                            } else if nested[i].is_some()
                                || ty
                                    .non_null()
                                    .obj_internal()
                                    .map(|n| n.render())
                                    .and_then(|n| collection_serializer_builder(&n))
                                    .is_some()
                            {
                                // Nested @Serializable OR a standard collection: encode[Nullable]Serializable
                                // Element(desc, i, <element serializer>, value.getX()) — `$serializer.INSTANCE`
                                // / `Foo.serializer(A_ser)` / `ListSerializer(…)`. The nullable variant shares
                                // the SAME descriptor (writes JSON null) — a method-name swap.
                                let Some(inst) = element_serializer_expr(ir, ty) else {
                                    bail = true;
                                    break;
                                };
                                let method = if is_nullable(ty) {
                                    "encodeNullableSerializableElement"
                                } else {
                                    "encodeSerializableElement"
                                };
                                stmts.push(ir.add_expr(IrExpr::Call {
                                    callee: virtual_iface(
                                        "kotlinx/serialization/encoding/CompositeEncoder",
                                        method,
                                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                                    ),
                                    dispatch_receiver: Some(c),
                                    args: vec![d, idx, inst, v],
                                }));
                            } else if is_nullable(ty) {
                                // Nullable element: encodeNullableSerializableElement(desc, i,
                                // <Elem>Serializer.INSTANCE, value.getX()) — the encoder writes JSON
                                // null when the value is null. Only reference elements (no boxing) for
                                // now; nullable primitives bail to a clean no-op.
                                if let Some(ser) = builtin_element_serializer(ty) {
                                    let inst = ir.external_static_instance(ser, ser, "INSTANCE");
                                    stmts.push(ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeEncoder",
                                            "encodeNullableSerializableElement",
                                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                                        ),
                                        dispatch_receiver: Some(c),
                                        args: vec![d, idx, inst, v],
                                    }));
                                } else {
                                    bail = true;
                                    break;
                                }
                            } else if let Some((mname, mdesc)) = encode_element_method(ty) {
                                stmts.push(ir.add_expr(IrExpr::Call {
                                    callee: virtual_iface(
                                        "kotlinx/serialization/encoding/CompositeEncoder",
                                        mname,
                                        mdesc,
                                    ),
                                    dispatch_receiver: Some(c),
                                    args: vec![d, idx, v],
                                }));
                            } else if let Some(inst) = element_serializer_expr(ir, ty) {
                                // A non-null reference element with a builtin/derivable serializer (e.g.
                                // `Uuid`) — encodeSerializableElement(desc, i, <Elem>Serializer, value.getX()).
                                stmts.push(ir.add_expr(IrExpr::Call {
                                    callee: virtual_iface(
                                        "kotlinx/serialization/encoding/CompositeEncoder",
                                        "encodeSerializableElement",
                                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                                    ),
                                    dispatch_receiver: Some(c),
                                    args: vec![d, idx, inst, v],
                                }));
                            } else {
                                bail = true;
                                break;
                            }
                            // OPTIONAL element (a constant default): omit it on encode when it still
                            // equals the default — wrap the just-pushed encode call in
                            //   if (c.shouldEncodeElementDefault(desc, i) || value.getX() != default) { … }
                            if let Some(Some(dc)) = field_defaults.get(i) {
                                if stmts.len() == n_before + 1 {
                                    let enc_stmt = stmts.pop().unwrap();
                                    let cd = this_desc(ir);
                                    let ci = ir.add_expr(IrExpr::Const(IrConst::Int(i as i32)));
                                    let cc = ir.add_expr(IrExpr::GetValue(3));
                                    let should = ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeEncoder",
                                            "shouldEncodeElementDefault",
                                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)Z",
                                        ),
                                        dispatch_receiver: Some(cc),
                                        args: vec![cd, ci],
                                    });
                                    // Re-read `value.getX()` for the comparison (a second, independent
                                    // node — an IR expr can't be shared across two tree positions). A
                                    // `@Serializable` property always has an auto-generated side-effect-free
                                    // accessor, and kotlinc's own `write$Self` likewise reads the field twice
                                    // (`self.x != default` then `encode…(self.x)`), so this is equivalent.
                                    let vr = ir.add_expr(IrExpr::GetValue(2));
                                    let Some(getter_desc) = ty_descriptor(ctx, ty) else {
                                        bail = true;
                                        break;
                                    };
                                    let cur = ir.add_expr(IrExpr::Call {
                                        callee: Callee::Virtual {
                                            owner: class_internal.clone(),
                                            name: property_getter_name(pname),
                                            descriptor: format!("(){getter_desc}"),
                                            interface: false,
                                        },
                                        dispatch_receiver: Some(vr),
                                        args: vec![],
                                    });
                                    let def = ir.add_expr(IrExpr::Const(dc.clone()));
                                    let neq = ir.add_expr(IrExpr::PrimitiveBinOp {
                                        op: crate::ir::IrBinOp::Ne,
                                        lhs: cur,
                                        rhs: def,
                                    });
                                    let cond = ir.add_expr(IrExpr::PrimitiveBinOp {
                                        op: crate::ir::IrBinOp::Or,
                                        lhs: should,
                                        rhs: neq,
                                    });
                                    stmts.push(ir.add_expr(IrExpr::When {
                                        branches: vec![(Some(cond), enc_stmt)],
                                    }));
                                }
                            }
                        }
                        if bail {
                            // a field type we can't encode yet — emit a clean no-op return (no
                            // beginStructure/endStructure), not a wrong call.
                            let ret = ir.add_expr(IrExpr::Return(None));
                            let body = ir.add_expr(IrExpr::Block {
                                stmts: vec![ret],
                                value: None,
                            });
                            ir.functions[fid as usize].body = Some(body);
                        } else {
                            let dend = this_desc(ir);
                            let cend = ir.add_expr(IrExpr::GetValue(3));
                            stmts.push(ir.add_expr(IrExpr::Call {
                                callee: virtual_iface(
                                    "kotlinx/serialization/encoding/CompositeEncoder",
                                    "endStructure",
                                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)V",
                                ),
                                dispatch_receiver: Some(cend),
                                args: vec![dend],
                            }));
                            let body = ir.add_expr(IrExpr::Block { stmts, value: None });
                            ir.functions[fid as usize].body = Some(body);
                        }
                    }
                    "deserialize"
                        if ir.classes[foo_id as usize].is_value
                            && fields
                                .first()
                                .and_then(|(_, t)| inline_prim_methods(t))
                                .is_some() =>
                    {
                        // A `@JvmInline value class`: deserialize inline —
                        //   return new Foo(decoder.decodeInline(this.descriptor).decode<U>())
                        let (_, uty) = fields[0].clone();
                        let (_, _, dec_name, dec_desc) = inline_prim_methods(&uty).unwrap();
                        let this = ir.add_expr(IrExpr::GetValue(0));
                        let desc = ir.add_expr(IrExpr::GetField {
                            receiver: this,
                            class: ser_idx as u32,
                            index: 0,
                        });
                        let dec = ir.add_expr(IrExpr::GetValue(1));
                        let inline_dec = ir.add_expr(IrExpr::Call {
                            callee: virtual_iface(
                                "kotlinx/serialization/encoding/Decoder",
                                "decodeInline",
                                "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/Decoder;",
                            ),
                            dispatch_receiver: Some(dec),
                            args: vec![desc],
                        });
                        let u = ir.add_expr(IrExpr::Call {
                            callee: virtual_iface(
                                "kotlinx/serialization/encoding/Decoder",
                                dec_name,
                                dec_desc,
                            ),
                            dispatch_receiver: Some(inline_dec),
                            args: vec![],
                        });
                        let new = ir.add_expr(IrExpr::New {
                            class: foo_id,
                            args: vec![u],
                            ctor_params: None,
                        });
                        let ret = ir.add_expr(IrExpr::Return(Some(new)));
                        let body = ir.add_expr(IrExpr::Block {
                            stmts: vec![ret],
                            value: None,
                        });
                        ir.functions[fid as usize].body = Some(body);
                    }
                    "deserialize" => {
                        // deserialize(decoder=1):
                        //   val c = decoder.beginStructure(descriptor)            [local 2]
                        //   var f0 = <default>; var f1 = <default>                 [locals 4, 5, ...]
                        //   loop@ while (true) {
                        //       val i = c.decodeElementIndex(descriptor)           [local 3]
                        //       when (i) { -1 -> break@loop; 0 -> f0 = c.decode<T>Element(d,0); ... }
                        //   }
                        //   c.endStructure(descriptor); return Foo(f0, f1)
                        // Supported only when every field is a decodable type; otherwise fall back to
                        // a default-construct stub (no wrong decode emitted).
                        let ser_cid = ser_idx as u32;
                        let decodable = fields.iter().enumerate().all(|(i, (pname, t))| {
                            if tp_field[i].is_some()
                                || property_is_contextual(ctx, ir, class_id, pname)
                            {
                                return true;
                            }
                            // A nested @Serializable element is decodable only if its serializer is
                            // actually derivable (a generic field with an un-derivable type arg is not) —
                            // else deserialize stubs cleanly rather than emit a `null` element serializer.
                            if nested[i].is_some() {
                                return can_derive_element_serializer(ir, t);
                            }
                            // A standard collection field decodes through its builtin collection serializer
                            // (via the `element_serializer_expr` fallback below) when its elements derive.
                            if t.non_null()
                                .obj_internal()
                                .map(|n| n.render())
                                .and_then(|n| collection_serializer_builder(&n))
                                .is_some()
                            {
                                return can_derive_element_serializer(ir, t);
                            }
                            if is_nullable(t) {
                                return builtin_element_serializer(t).is_some();
                            }
                            decode_element_method(t).is_some()
                        });
                        // Field-local slot for each property — `this`=0, decoder=1, c=2, i=3, then the
                        // field locals from slot 4, advancing by each type's JVM width (Long/Double=2).
                        let mut slots: Vec<u32> = Vec::with_capacity(fields.len());
                        let mut next = 4u32;
                        for (_, ty) in &fields {
                            slots.push(next);
                            next += slot_width(ty);
                        }
                        let this_desc = |ir: &mut IrFile| -> ExprId {
                            let r = ir.add_expr(IrExpr::GetValue(0));
                            ir.add_expr(IrExpr::GetField {
                                receiver: r,
                                class: ser_cid,
                                index: 0,
                            })
                        };
                        let body = if decodable {
                            let d0 = this_desc(ir);
                            let dec = ir.add_expr(IrExpr::GetValue(1));
                            let begin = ir.add_expr(IrExpr::Call {
                                callee: virtual_iface(
                                    "kotlinx/serialization/encoding/Decoder",
                                    "beginStructure",
                                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/CompositeDecoder;",
                                ),
                                dispatch_receiver: Some(dec),
                                args: vec![d0],
                            });
                            let cvar = ir.add_expr(IrExpr::Variable {
                                index: 2,
                                ty: class_ty("kotlinx/serialization/encoding/CompositeDecoder"),
                                init: Some(begin),
                                named: false,
                            });
                            let mut stmts = vec![cvar];
                            // index local (3) — declared before the field locals so emit's slot order
                            // (by declaration) matches the explicit indices (this=0, decoder=1, c=2,
                            // i=3, f0=4, f1=5, …).
                            let izero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                            stmts.push(ir.add_expr(IrExpr::Variable {
                                index: 3,
                                ty: class_ty("kotlin/Int"),
                                init: Some(izero),
                                named: false,
                            }));
                            // field locals (4 + k), defaulted. An OPTIONAL element (a constant default)
                            // starts at its DEFAULT value, so an element omitted from the input (never
                            // returned by `decodeElementIndex`) keeps the default and the normal
                            // constructor reproduces it — the const-default equivalent of kotlinc's
                            // `seen`-mask + `SerializationConstructorMarker` fill (no synthetic ctor needed).
                            // A non-optional element starts at the type's zero/null (overwritten on decode).
                            for (k, (_, ty)) in fields.iter().enumerate() {
                                let dc = if let Some(Some(d)) = field_defaults.get(k) {
                                    d.clone()
                                } else if is_nullable(ty) {
                                    IrConst::Null
                                } else {
                                    default_const(ty)
                                };
                                let init = ir.add_expr(IrExpr::Const(dc));
                                stmts.push(ir.add_expr(IrExpr::Variable {
                                    index: slots[k],
                                    ty: ty.clone(),
                                    init: Some(init),
                                    named: false,
                                }));
                            }
                            // loop body: i = c.decodeElementIndex(desc); when(i){…}
                            let didx = this_desc(ir);
                            let cdi = ir.add_expr(IrExpr::GetValue(2));
                            let dei = ir.add_expr(IrExpr::Call {
                                callee: virtual_iface(
                                    "kotlinx/serialization/encoding/CompositeDecoder",
                                    "decodeElementIndex",
                                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)I",
                                ),
                                dispatch_receiver: Some(cdi),
                                args: vec![didx],
                            });
                            let set_i = ir.add_expr(IrExpr::SetValue { var: 3, value: dei });
                            // A sequence of single-branch `if` statements (each a Unit statement — no
                            // value mixing): `if (i == -1) break`, then `if (i == k) f_k = decode…`.
                            let mut loop_stmts = vec![set_i];
                            let iref = ir.add_expr(IrExpr::GetValue(3));
                            let neg1 = ir.add_expr(IrExpr::Const(IrConst::Int(-1)));
                            let is_done = ir.add_expr(IrExpr::PrimitiveBinOp {
                                op: crate::ir::IrBinOp::Eq,
                                lhs: iref,
                                rhs: neg1,
                            });
                            let brk = ir.add_expr(IrExpr::Break {
                                label: Some("deser".to_string()),
                            });
                            let brk_blk = ir.add_expr(IrExpr::Block {
                                stmts: vec![brk],
                                value: None,
                            });
                            loop_stmts.push(ir.add_expr(IrExpr::When {
                                branches: vec![(Some(is_done), brk_blk)],
                            }));
                            for (k, (_, ty)) in fields.iter().enumerate() {
                                let iref = ir.add_expr(IrExpr::GetValue(3));
                                let kc = ir.add_expr(IrExpr::Const(IrConst::Int(k as i32)));
                                let is_k = ir.add_expr(IrExpr::PrimitiveBinOp {
                                    op: crate::ir::IrBinOp::Eq,
                                    lhs: iref,
                                    rhs: kc,
                                });
                                let dk = this_desc(ir);
                                let idxc = ir.add_expr(IrExpr::Const(IrConst::Int(k as i32)));
                                let cdk = ir.add_expr(IrExpr::GetValue(2));
                                let decoded = if let Some(inst) = contextual_serializer_for(
                                    ir,
                                    property_is_contextual(ctx, ir, class_id, &fields[k].0),
                                    ty,
                                ) {
                                    // Contextual element: f_k = (T) c.decode[Nullable]SerializableElement(
                                    // desc, k, ContextualSerializer(<type>::class), null).
                                    let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
                                    let method = if is_nullable(ty) {
                                        "decodeNullableSerializableElement"
                                    } else {
                                        "decodeSerializableElement"
                                    };
                                    let raw = ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeDecoder",
                                            method,
                                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/DeserializationStrategy;Ljava/lang/Object;)Ljava/lang/Object;",
                                        ),
                                        dispatch_receiver: Some(cdk),
                                        args: vec![dk, idxc, inst, prev],
                                    });
                                    ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::Cast,
                                        arg: raw,
                                        type_operand: *ty,
                                    })
                                } else if let Some(fidx) = tp_field[k] {
                                    // f_k = (T) c.decode[Nullable]SerializableElement(desc, k,
                                    // this.typeSerialK, null) — the ctor-supplied type-param serializer.
                                    let this_s = ir.add_expr(IrExpr::GetValue(0));
                                    let inst = ir.add_expr(IrExpr::GetField {
                                        receiver: this_s,
                                        class: ser_cid,
                                        index: fidx,
                                    });
                                    let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
                                    let method = if is_nullable(ty) {
                                        "decodeNullableSerializableElement"
                                    } else {
                                        "decodeSerializableElement"
                                    };
                                    let raw = ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeDecoder",
                                            method,
                                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/DeserializationStrategy;Ljava/lang/Object;)Ljava/lang/Object;",
                                        ),
                                        dispatch_receiver: Some(cdk),
                                        args: vec![dk, idxc, inst, prev],
                                    });
                                    ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::Cast,
                                        arg: raw,
                                        type_operand: ty.clone(),
                                    })
                                } else if nested[k].is_some()
                                    || ty
                                        .non_null()
                                        .obj_internal()
                                        .map(|n| n.render())
                                        .and_then(|n| collection_serializer_builder(&n))
                                        .is_some()
                                {
                                    // f_k = (T) c.decode[Nullable]SerializableElement(desc, k,
                                    // <element serializer>, null) — the nested `$serializer.INSTANCE`
                                    // (non-generic) / `Foo.serializer(A_ser)` (generic) / `ListSerializer(…)`
                                    // (collection). Same descriptor; the nullable variant yields null for a
                                    // JSON-null element.
                                    let inst =
                                        element_serializer_expr(ir, ty).unwrap_or_else(|| {
                                            ir.add_expr(IrExpr::Const(IrConst::Null))
                                        });
                                    let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
                                    let method = if is_nullable(ty) {
                                        "decodeNullableSerializableElement"
                                    } else {
                                        "decodeSerializableElement"
                                    };
                                    let raw = ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeDecoder",
                                            method,
                                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/DeserializationStrategy;Ljava/lang/Object;)Ljava/lang/Object;",
                                        ),
                                        dispatch_receiver: Some(cdk),
                                        args: vec![dk, idxc, inst, prev],
                                    });
                                    ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::Cast,
                                        arg: raw,
                                        type_operand: ty.clone(),
                                    })
                                } else if is_nullable(ty) {
                                    // f_k = (T) c.decodeNullableSerializableElement(desc, k,
                                    // <Elem>Serializer.INSTANCE, null) — yields the element or null.
                                    let ser = builtin_element_serializer(ty).unwrap();
                                    let inst = ir.external_static_instance(ser, ser, "INSTANCE");
                                    let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
                                    let raw = ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeDecoder",
                                            "decodeNullableSerializableElement",
                                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/DeserializationStrategy;Ljava/lang/Object;)Ljava/lang/Object;",
                                        ),
                                        dispatch_receiver: Some(cdk),
                                        args: vec![dk, idxc, inst, prev],
                                    });
                                    ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::Cast,
                                        arg: raw,
                                        type_operand: ty.clone(),
                                    })
                                } else {
                                    let (mname, mdesc) = decode_element_method(ty).unwrap();
                                    ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeDecoder",
                                            mname,
                                            mdesc,
                                        ),
                                        dispatch_receiver: Some(cdk),
                                        args: vec![dk, idxc],
                                    })
                                };
                                let setk = ir.add_expr(IrExpr::SetValue {
                                    var: slots[k],
                                    value: decoded,
                                });
                                let setk_blk = ir.add_expr(IrExpr::Block {
                                    stmts: vec![setk],
                                    value: None,
                                });
                                loop_stmts.push(ir.add_expr(IrExpr::When {
                                    branches: vec![(Some(is_k), setk_blk)],
                                }));
                            }
                            let loop_body = ir.add_expr(IrExpr::Block {
                                stmts: loop_stmts,
                                value: None,
                            });
                            let cond = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
                            let whilexpr = ir.add_expr(IrExpr::While {
                                cond,
                                body: loop_body,
                                update: None,
                                post_test: false,
                                label: Some("deser".to_string()),
                            });
                            stmts.push(whilexpr);
                            // endStructure
                            let dend = this_desc(ir);
                            let cend = ir.add_expr(IrExpr::GetValue(2));
                            stmts.push(ir.add_expr(IrExpr::Call {
                                callee: virtual_iface(
                                    "kotlinx/serialization/encoding/CompositeDecoder",
                                    "endStructure",
                                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)V",
                                ),
                                dispatch_receiver: Some(cend),
                                args: vec![dend],
                            }));
                            // return Foo(f0, f1, ...)
                            let args: Vec<ExprId> = (0..fields.len())
                                .map(|k| ir.add_expr(IrExpr::GetValue(slots[k])))
                                .collect();
                            let new = ir.add_expr(IrExpr::New {
                                class: foo_id,
                                args,
                                ctor_params: None,
                            });
                            stmts.push(ir.add_expr(IrExpr::Return(Some(new))));
                            ir.add_expr(IrExpr::Block { stmts, value: None })
                        } else {
                            // fallback: default-construct stub (no decode emitted for unsupported shapes).
                            // Nullable fields default to null, matching the real decode path (and so a
                            // future non-null default in `default_const` can't leak into a nullable slot).
                            let args: Vec<ExprId> = field_types
                                .iter()
                                .map(|ty| {
                                    let dc = if is_nullable(ty) {
                                        IrConst::Null
                                    } else {
                                        default_const(ty)
                                    };
                                    ir.add_expr(IrExpr::Const(dc))
                                })
                                .collect();
                            let new = ir.add_expr(IrExpr::New {
                                class: foo_id,
                                args,
                                ctor_params: None,
                            });
                            let ret = ir.add_expr(IrExpr::Return(Some(new)));
                            ir.add_expr(IrExpr::Block {
                                stmts: vec![ret],
                                value: None,
                            })
                        };
                        ir.functions[fid as usize].body = Some(body);
                    }
                    "childSerializers" => {
                        // Return the per-field element-serializer array (arity == field count): one
                        // `KSerializer` singleton per property. A nested `@Serializable` field uses the
                        // krusty-generated `<T>$serializer.INSTANCE`; a directly-supported field uses the
                        // builtin `…Serializer.INSTANCE`. An unsupported field type contributes `null`
                        // (placeholder) so the array arity still matches the descriptor's element count.
                        let elements: Vec<ExprId> = field_types
                            .iter()
                            .enumerate()
                            .map(|(i, _ty)| {
                                if let Some(inst) = contextual_serializer_for(
                                    ir,
                                    property_is_contextual(ctx, ir, class_id, &fields[i].0),
                                    &field_types[i],
                                ) {
                                    // `@Contextual` / file-level `@UseContextualSerialization` property.
                                    inst
                                } else if let Some(fidx) = tp_field[i] {
                                    // Type-parameter element: `this.typeSerialK` (the ctor-supplied serializer).
                                    let this = ir.add_expr(IrExpr::GetValue(0));
                                    ir.add_expr(IrExpr::GetField {
                                        receiver: this,
                                        class: ser_idx as u32,
                                        index: fidx,
                                    })
                                } else if let Some(internal) =
                                    field_serializer_of(ctx, ir, class_id, &fields[i].0)
                                {
                                    // Explicit per-property serializer: `new X()` (or `X.INSTANCE`),
                                    // wrapped `.nullable` for a nullable property.
                                    let base = build_field_serializer_instance(ir, &internal);
                                    if is_nullable(&field_types[i]) {
                                        wrap_nullable_serializer(ir, base)
                                    } else {
                                        base
                                    }
                                } else if let Some(e) = element_serializer_expr(ir, &field_types[i])
                                {
                                    // Nested @Serializable (generic `Foo<A>` → `Foo.serializer(A_ser)`,
                                    // or non-generic `Foo$serializer.INSTANCE`) | builtin `…Serializer`.
                                    e
                                } else {
                                    ir.add_expr(IrExpr::Const(IrConst::Null))
                                }
                            })
                            .collect();
                        let arr = ir.add_expr(IrExpr::Vararg {
                            array_type: Ty::obj_args("kotlin/Array", &[class_ty(KSERIALIZER_FQ)]),
                            elements,
                        });
                        let ret = ir.add_expr(IrExpr::Return(Some(arr)));
                        let body = ir.add_expr(IrExpr::Block {
                            stmts: vec![ret],
                            value: None,
                        });
                        ir.functions[fid as usize].body = Some(body);
                    }
                    "typeParametersSerializers" => {
                        let elements: Vec<ExprId> = (1..=class_type_params.len() as u32)
                            .map(|fidx| {
                                let this = ir.add_expr(IrExpr::GetValue(0));
                                ir.add_expr(IrExpr::GetField {
                                    receiver: this,
                                    class: ser_idx as u32,
                                    index: fidx,
                                })
                            })
                            .collect();
                        let arr = ir.add_expr(IrExpr::Vararg {
                            array_type: Ty::obj_args("kotlin/Array", &[class_ty(KSERIALIZER_FQ)]),
                            elements,
                        });
                        let ret = ir.add_expr(IrExpr::Return(Some(arr)));
                        let body = ir.add_expr(IrExpr::Block {
                            stmts: vec![ret],
                            value: None,
                        });
                        ir.functions[fid as usize].body = Some(body);
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{synthetic_class, PluginHost};

    fn jvm_type_descriptor(ty: Ty) -> Option<String> {
        Some(crate::jvm::names::type_descriptor(ty))
    }

    fn plugin_context() -> PluginContext<'static> {
        PluginContext::default().with_target_type_descriptor(jvm_type_descriptor)
    }

    /// Build `@Serializable class <name>(<one val per field type>)` as IR + an annotation table.
    fn serializable_class(
        name: &str,
        field_types: &[&str],
    ) -> (IrFile, PluginContext<'static>, u32) {
        let mut ir = IrFile::default();
        let mut c = synthetic_class(name);
        c.fields = field_types
            .iter()
            .enumerate()
            .map(|(i, ty)| crate::ir::IrField::new(format!("f{i}"), class_ty(ty)))
            .collect();
        c.ctor_param_count = field_types.len() as u32;
        let id = ir.add_class(c);
        let mut ctx = plugin_context();
        ctx.class_annotations
            .insert(id, vec![SERIALIZABLE_FQ.to_string()].into());
        (ir, ctx, id)
    }

    fn run(ir: &mut IrFile, ctx: &PluginContext<'_>) {
        let mut host = PluginHost::new();
        host.register(Box::new(SerializationPlugin::default()));
        host.run(ir, ctx);
    }

    fn find_class<'a>(ir: &'a IrFile, fq: &str) -> &'a crate::ir::IrClass {
        ir.classes
            .iter()
            .find(|c| c.fq_name_matches(fq))
            .unwrap_or_else(|| panic!("class {fq} not found"))
    }

    fn calls_method(ir: &IrFile, name: &str) -> bool {
        ir.exprs.iter().any(|e| {
            matches!(e, IrExpr::Call { callee: Callee::Virtual { name: n, .. }, .. } if n == name)
        })
    }

    fn refs_external_static(ir: &IrFile, owner: &str) -> bool {
        ir.exprs.iter().any(
            |e| matches!(e, IrExpr::ExternalStaticInstance { owner: o, .. } if o.matches(owner)),
        )
    }

    #[test]
    fn nullable_string_field_routes_through_nullable_serializable_calls() {
        // `@Serializable class N(val a: Int, val b: String?)` — the nullable element must go through
        // encode/decodeNullableSerializableElement with the builtin StringSerializer singleton, not
        // the plain encode/decodeStringElement path (which can't represent null).
        let (mut ir, ctx, id) = serializable_class("N", &["kotlin/Int", "kotlin/String"]);
        ir.classes[id as usize].fields[1].ty = Ty::nullable(ir.classes[id as usize].fields[1].ty);
        run(&mut ir, &ctx);
        assert!(
            calls_method(&ir, "encodeNullableSerializableElement"),
            "serialize must use encodeNullableSerializableElement for String?"
        );
        assert!(
            calls_method(&ir, "decodeNullableSerializableElement"),
            "deserialize must use decodeNullableSerializableElement for String?"
        );
        assert!(
            refs_external_static(&ir, "kotlinx/serialization/internal/StringSerializer"),
            "must getstatic StringSerializer.INSTANCE as the element serializer"
        );
        // The non-null Int element still uses the direct primitive path.
        assert!(
            calls_method(&ir, "encodeIntElement") && calls_method(&ir, "decodeIntElement"),
            "non-null Int still uses encode/decodeIntElement"
        );
    }

    #[test]
    fn nullable_primitive_uses_boxed_name_and_builtin_serializer() {
        // krusty lowers a nullable primitive to its BOXED fq name (`Int?` → `java/lang/Integer`,
        // nullable). The plugin must still route it through encodeNullableSerializableElement with the
        // builtin IntSerializer singleton — this guards the boxed-name mapping.
        let (mut ir, ctx, id) = serializable_class("P", &["kotlin/Int"]);
        ir.classes[id as usize].fields[0].ty = Ty::nullable(Ty::obj("java/lang/Integer"));
        run(&mut ir, &ctx);
        assert!(
            calls_method(&ir, "encodeNullableSerializableElement")
                && calls_method(&ir, "decodeNullableSerializableElement"),
            "nullable Int? must use the nullable serializable calls"
        );
        assert!(
            refs_external_static(&ir, "kotlinx/serialization/internal/IntSerializer"),
            "nullable Int? must reference IntSerializer.INSTANCE"
        );
    }

    #[test]
    fn nullable_nested_composite_uses_nullable_serializable_calls() {
        // `@Serializable class Outer(val inner: Inner?, val label: String)` where Inner is itself
        // @Serializable — the nullable nested element must go through encode/decodeNullable-
        // SerializableElement (not the plain serializable element, which can't represent null).
        let mut ir = IrFile::default();
        let mut inner = synthetic_class("Inner");
        inner.fields = vec![crate::ir::IrField::new(
            "v".to_string(),
            class_ty("kotlin/Int"),
        )];
        inner.ctor_param_count = 1;
        let inner_id = ir.add_class(inner);
        let mut outer = synthetic_class("Outer");
        outer.fields = vec![
            crate::ir::IrField::new("inner".to_string(), Ty::nullable(Ty::obj("Inner"))),
            crate::ir::IrField::new("label".to_string(), class_ty("kotlin/String")),
        ];
        outer.ctor_param_count = 2;
        let outer_id = ir.add_class(outer);
        let mut ctx = plugin_context();
        ctx.class_annotations
            .insert(inner_id, vec![SERIALIZABLE_FQ.to_string()].into());
        ctx.class_annotations
            .insert(outer_id, vec![SERIALIZABLE_FQ.to_string()].into());
        run(&mut ir, &ctx);
        assert!(
            calls_method(&ir, "encodeNullableSerializableElement")
                && calls_method(&ir, "decodeNullableSerializableElement"),
            "Inner? must use the nullable serializable element calls"
        );
        // The non-null String field still uses the direct primitive path.
        assert!(
            calls_method(&ir, "encodeStringElement"),
            "non-null String still uses encodeStringElement"
        );
    }

    #[test]
    fn synthesizes_serializer_object_with_members() {
        let (mut ir, ctx, _) = serializable_class("demo/Foo", &["kotlin/Int", "kotlin/String"]);
        run(&mut ir, &ctx);

        let ser = find_class(&ir, "demo/Foo$$serializer");
        assert!(ser.is_object, "$serializer is a singleton object");
        assert_eq!(
            ser.interfaces.to_vec(),
            vec![GENERATED_SERIALIZER_FQ.to_string()]
        );
        // supertype is KSerializer<Foo> (parameterized by the serialized type).
        match ser.supertypes[0].non_null().obj_internal() {
            Some(fq_name) => {
                assert_eq!(fq_name, KSERIALIZER_FQ);
                assert_eq!(ser.supertypes[0].type_args(), &[class_ty("demo/Foo")]);
            }
            None => panic!("expected KSerializer<Foo>, got {:?}", ser.supertypes[0]),
        }
        let names: Vec<&str> = ser
            .methods
            .iter()
            .map(|&fid| ir.functions[fid as usize].name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "getDescriptor",
                "serialize",
                "deserialize",
                "childSerializers",
                "typeParametersSerializers"
            ]
        );
    }

    #[test]
    fn deser_ctor_seq_mask_count_is_floor_div_32_plus_1() {
        for (n, expect) in [(1usize, 1usize), (31, 1), (32, 2), (33, 2), (64, 3)] {
            let tys: Vec<&str> = std::iter::repeat_n("kotlin/String", n).collect();
            let (mut ir, ctx, _) = serializable_class("demo/M", &tys);
            run(&mut ir, &ctx);
            let ctor = find_class(&ir, "demo/M")
                .secondary_ctors
                .iter()
                .find(|c| c.synthetic)
                .expect("synthetic deserialization ctor");
            let masks = ctor.params.iter().take_while(|t| **t == Ty::Int).count();
            assert_eq!(
                masks, expect,
                "n={n} fields: expected {expect} seq-mask ints"
            );
            assert_eq!(ctor.params.len(), expect + n + 1, "n={n} total ctor params");
        }
    }

    #[test]
    fn serializer_accessor_reads_singleton() {
        let (mut ir, ctx, _) = serializable_class("demo/Foo", &["kotlin/Int"]);
        run(&mut ir, &ctx);

        // The Companion gains an instance `serializer()` returning KSerializer<Foo>.
        let acc_fid = *find_class(&ir, "demo/Foo$Companion")
            .methods
            .iter()
            .find(|&&fid| ir.functions[fid as usize].name == "serializer")
            .expect("serializer() accessor synthesized on the Companion");
        let acc = &ir.functions[acc_fid as usize];
        assert!(!acc.is_static);
        match acc.ret.non_null().obj_internal() {
            Some(fq_name) => assert_eq!(fq_name, KSERIALIZER_FQ),
            None => panic!("expected KSerializer, got {:?}", acc.ret),
        }
        // Its body returns the $$serializer singleton (`return Foo$$serializer.INSTANCE`).
        let ser_id = ir
            .classes
            .iter()
            .position(|c| c.fq_name_matches("demo/Foo$$serializer"))
            .unwrap() as u32;
        let Some(IrExpr::Block { stmts, .. }) = acc.body.map(|b| ir.expr(b).clone()) else {
            panic!("accessor body is not a block");
        };
        let Some(IrExpr::Return(Some(v))) = stmts.first().map(|&s| ir.expr(s).clone()) else {
            panic!("accessor body does not return a value");
        };
        match ir.expr(v) {
            IrExpr::StaticInstance { ty, field, .. } => {
                assert_eq!(*ty, ser_id);
                assert_eq!(*field, "INSTANCE");
            }
            other => panic!("expected StaticInstance, got {other:?}"),
        }
    }

    #[test]
    fn generic_serializer_returns_type_parameter_serializers() {
        let mut ir = IrFile::default();
        let mut c = synthetic_class("demo/Box");
        c.type_params = vec!["T".to_string()];
        c.fields = vec![crate::ir::IrField::new(
            "value".to_string(),
            Ty::ty_param("T", class_ty("kotlin/Any")),
        )];
        c.ctor_param_count = 1;
        let id = ir.add_class(c);
        let mut ctx = plugin_context();
        ctx.class_annotations
            .insert(id, vec![SERIALIZABLE_FQ.to_string()].into());
        run(&mut ir, &ctx);

        let ser_id = ir
            .classes
            .iter()
            .position(|c| c.fq_name_matches("demo/Box$$serializer"))
            .expect("generic serializer class") as u32;
        let tps_fid = *ir.classes[ser_id as usize]
            .methods
            .iter()
            .find(|&&fid| ir.functions[fid as usize].name == "typeParametersSerializers")
            .expect("typeParametersSerializers method");
        let Some(IrExpr::Block { stmts, .. }) = ir.functions[tps_fid as usize]
            .body
            .map(|b| ir.expr(b).clone())
        else {
            panic!("typeParametersSerializers body is not a block");
        };
        let Some(IrExpr::Return(Some(arr))) = stmts.first().map(|&s| ir.expr(s).clone()) else {
            panic!("typeParametersSerializers does not return a value");
        };
        let IrExpr::Vararg { elements, .. } = ir.expr(arr) else {
            panic!("typeParametersSerializers return is not a vararg");
        };
        assert_eq!(elements.len(), 1);
        match ir.expr(elements[0]) {
            IrExpr::GetField { class, index, .. } => {
                assert_eq!(*class, ser_id);
                assert_eq!(*index, 1);
            }
            other => panic!("expected typeSerial0 field read, got {other:?}"),
        }
    }

    /// The number of `addElement` calls in the `$serializer`'s `<init>` (its descriptor's element
    /// count) — the real, runtime-meaningful "arity tracks the field list" (verified end-to-end by
    /// the encode round-trip in `serialization_roundtrip_e2e`).
    fn descriptor_add_element_count(ir: &IrFile, ser_fq: &str) -> usize {
        let ser = find_class(ir, ser_fq);
        let Some(init) = ser.init_body else {
            return 0;
        };
        let IrExpr::Block { stmts, .. } = ir.expr(init) else {
            panic!("init_body is not a block");
        };
        stmts
            .iter()
            .filter(|&&s| {
                matches!(ir.expr(s), IrExpr::Call { callee: Callee::Virtual { name, .. }, .. } if name == "addElement")
            })
            .count()
    }

    #[test]
    fn descriptor_element_count_tracks_field_count() {
        for fields in [
            &[][..],
            &["kotlin/Int"][..],
            &["kotlin/Int", "kotlin/String", "kotlin/Boolean"][..],
        ] {
            let (mut ir, ctx, _) = serializable_class("demo/C", fields);
            run(&mut ir, &ctx);
            assert_eq!(
                descriptor_add_element_count(&ir, "demo/C$$serializer"),
                fields.len(),
                "one descriptor element per field"
            );
        }
    }

    #[test]
    fn body_phase_fills_serialize_deserialize() {
        let (mut ir, ctx, _) = serializable_class("demo/Foo", &["kotlin/Int"]);
        SerializationPlugin::default().generate_declarations(&mut ir, &ctx);
        let before = ir
            .functions
            .iter()
            .filter(|f| f.name == "serialize" || f.name == "deserialize")
            .all(|f| f.body.is_none());
        assert!(before, "decl phase leaves serialize/deserialize empty");

        SerializationPlugin::default().transform_bodies(&mut ir, &ctx);
        let after = ir
            .functions
            .iter()
            .filter(|f| f.name == "serialize" || f.name == "deserialize")
            .all(|f| f.body.is_some());
        assert!(after, "body phase fills serialize/deserialize");
    }

    #[test]
    fn multiple_serializable_classes_each_get_a_serializer() {
        let mut ir = IrFile::default();
        let mut ctx = plugin_context();
        for (name, n) in [("demo/A", 1usize), ("demo/B", 2)] {
            let mut c = synthetic_class(name);
            c.fields = (0..n)
                .map(|i| crate::ir::IrField::new(format!("f{i}"), class_ty("kotlin/Int")))
                .collect();
            let id = ir.add_class(c);
            ctx.class_annotations
                .insert(id, vec![SERIALIZABLE_FQ.to_string()].into());
        }
        run(&mut ir, &ctx);
        assert!(ir
            .classes
            .iter()
            .any(|c| c.fq_name_matches("demo/A$$serializer")));
        assert!(ir
            .classes
            .iter()
            .any(|c| c.fq_name_matches("demo/B$$serializer")));
    }

    #[test]
    fn write_self_name_follows_target_abi() {
        // The plugin generates per target runtime version: the write helper is unmangled on <1.6 and
        // module-mangled on >=1.6 — like krusty pinning a kotlinc version, codegen follows the target.
        let names = |abi| {
            let (mut ir, ctx, class_id) = serializable_class("demo/Foo", &["kotlin/Int"]);
            SerializationPlugin::new(abi, "app").generate_declarations(&mut ir, &ctx);
            ir.classes[class_id as usize]
                .methods
                .iter()
                .map(|&f| ir.functions[f as usize].name.clone())
                .collect::<Vec<_>>()
        };
        assert!(names(SerializationAbi::V1_0).contains(&"write$Self".to_string()));
        assert!(names(SerializationAbi::V1_6Plus).contains(&"write$Self$app".to_string()));
        // The two versions produce different helper names.
        assert!(!names(SerializationAbi::V1_0).contains(&"write$Self$app".to_string()));
    }

    #[test]
    fn abi_detected_from_classpath_runtime() {
        // Drop-in: the ABI follows the kotlinx-serialization-core jar on -classpath, not a flag.
        assert_eq!(
            SerializationAbi::from_core_version("1.5.0"),
            SerializationAbi::V1_0
        );
        assert_eq!(
            SerializationAbi::from_core_version("1.6.0"),
            SerializationAbi::V1_6Plus
        );
        assert_eq!(
            SerializationAbi::from_core_version("1.8.1"),
            SerializationAbi::V1_6Plus
        );

        let cp = vec![
            "/x/kotlin-stdlib.jar".to_string(),
            "/x/kotlinx-serialization-core-jvm-1.8.1.jar".to_string(),
        ];
        assert_eq!(
            SerializationAbi::from_classpath(&cp),
            Some(SerializationAbi::V1_6Plus)
        );

        let old = vec!["/x/kotlinx-serialization-core-1.5.0.jar".to_string()];
        assert_eq!(
            SerializationAbi::from_classpath(&old),
            Some(SerializationAbi::V1_0)
        );

        // No serialization runtime on the classpath → no ABI (annotation wouldn't resolve either).
        assert_eq!(
            SerializationAbi::from_classpath(&["/x/kotlin-stdlib.jar".to_string()]),
            None
        );

        // -core, not -json/-protobuf, drives the ABI even when several serialization jars co-exist.
        let many = vec![
            "/x/kotlinx-serialization-json-jvm-1.5.0.jar".to_string(),
            "/x/kotlinx-serialization-protobuf-jvm-1.5.0.jar".to_string(),
            "/x/kotlinx-serialization-core-jvm-1.8.1.jar".to_string(),
        ];
        assert_eq!(
            SerializationAbi::from_classpath(&many),
            Some(SerializationAbi::V1_6Plus)
        );

        // A snapshot version still parses by its leading numeric components.
        let snap = vec!["/x/kotlinx-serialization-core-jvm-1.8.1-SNAPSHOT.jar".to_string()];
        assert_eq!(
            SerializationAbi::from_classpath(&snap),
            Some(SerializationAbi::V1_6Plus)
        );
    }

    #[test]
    fn property_descriptor_uses_shared_jvm_mapping() {
        let ctx = plugin_context();
        assert_eq!(ty_descriptor(&ctx, &Ty::Int).as_deref(), Some("I"));
        assert_eq!(
            ty_descriptor(&ctx, &Ty::nullable(Ty::Int)).as_deref(),
            Some("Ljava/lang/Integer;")
        );
        assert_eq!(
            ty_descriptor(&ctx, &Ty::nullable(Ty::Long)).as_deref(),
            Some("Ljava/lang/Long;")
        );
        assert_eq!(
            ty_descriptor(&ctx, &Ty::nullable(Ty::Char)).as_deref(),
            Some("Ljava/lang/Character;")
        );
        assert_eq!(
            ty_descriptor(&ctx, &Ty::nullable(Ty::Boolean)).as_deref(),
            Some("Ljava/lang/Boolean;")
        );
        assert_eq!(
            ty_descriptor(&ctx, &Ty::String).as_deref(),
            Some("Ljava/lang/String;")
        );
        assert_eq!(
            ty_descriptor(&ctx, &Ty::obj("kotlin/collections/List")).as_deref(),
            Some("Ljava/util/List;")
        );
    }

    #[test]
    fn non_serializable_class_untouched() {
        let mut ir = IrFile::default();
        ir.add_class(synthetic_class("demo/Plain"));
        let before = ir.classes.len();
        run(&mut ir, &PluginContext::default());
        assert_eq!(ir.classes.len(), before, "no @Serializable → no synthesis");
    }

    /// A generic reified-serialization placeholder (what core lowering emits) is specialized by the
    /// plugin into the concrete `StringFormat` member call — the kotlinx descriptors live here.
    fn placeholder(kind: &'static str) -> (IrFile, u32, [u32; 3]) {
        let mut ir = IrFile::default();
        let recv = ir.add_expr(IrExpr::GetValue(0));
        let ser = ir.add_expr(IrExpr::GetValue(1));
        let arg = ir.add_expr(IrExpr::GetValue(2));
        let mid = ir.add_expr(IrExpr::PluginPlaceholder {
            plugin: "serialization",
            kind,
            exprs: vec![recv, ser, arg],
            data: vec![
                "kotlinx/serialization/json/Json".to_string(),
                "demo/Foo".to_string(),
            ],
        });
        (ir, mid, [recv, ser, arg])
    }

    #[test]
    fn reified_encode_placeholder_specialized() {
        let (mut ir, mid, [recv, ser, arg]) = placeholder("encodeToString");
        specialize_reified_placeholders(&mut ir);
        match &ir.exprs[mid as usize] {
            IrExpr::Call {
                callee:
                    Callee::Virtual {
                        owner,
                        name,
                        descriptor,
                        interface,
                    },
                dispatch_receiver,
                args,
            } => {
                assert_eq!(owner, "kotlinx/serialization/json/Json");
                assert_eq!(name, "encodeToString");
                assert_eq!(descriptor, ENCODE_TO_STRING_DESC);
                assert!(!interface);
                assert_eq!(*dispatch_receiver, Some(recv));
                assert_eq!(args, &vec![ser, arg]);
            }
            other => panic!("expected encodeToString call, got {other:?}"),
        }
    }

    #[test]
    fn reified_decode_placeholder_specialized_with_checkcast() {
        let (mut ir, mid, [recv, ser, arg]) = placeholder("decodeFromString");
        specialize_reified_placeholders(&mut ir);
        // The slot becomes a checkcast to the decoded class wrapping the decode member call.
        let inner = match &ir.exprs[mid as usize] {
            IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: inner,
                type_operand,
            } => {
                assert_eq!(*type_operand, Ty::obj("demo/Foo"));
                *inner
            }
            other => panic!("expected checkcast, got {other:?}"),
        };
        match &ir.exprs[inner as usize] {
            IrExpr::Call {
                callee: Callee::Virtual {
                    name, descriptor, ..
                },
                dispatch_receiver,
                args,
            } => {
                assert_eq!(name, "decodeFromString");
                assert_eq!(descriptor, DECODE_FROM_STRING_DESC);
                assert_eq!(*dispatch_receiver, Some(recv));
                assert_eq!(args, &vec![ser, arg]);
            }
            other => panic!("expected decodeFromString call, got {other:?}"),
        }
    }

    #[test]
    fn surviving_placeholder_declines_emit() {
        // A placeholder that reaches the JVM emitter (plugin never ran) must make the file decline,
        // not miscompile. `run` here is a non-`@Serializable` no-op, so the marker survives.
        let (mut ir, _mid, _) = placeholder("encodeToString");
        assert!(
            !crate::jvm::ir_emit::jvm_can_emit(&ir),
            "a surviving PluginPlaceholder must be declined by jvm_can_emit"
        );
        // After specialization the file is emittable again.
        specialize_reified_placeholders(&mut ir);
        assert!(crate::jvm::ir_emit::jvm_can_emit(&ir));
    }
}
