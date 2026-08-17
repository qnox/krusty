//! Small, backend-agnostic JVM naming/descriptor helpers (relocated out of the retired AST emitter).

use crate::types::{InternalName, Ty};

/// The file-facade class internal name for a source file: `Foo.kt` → `FooKt` (package-qualified).
pub fn file_class_name(file_stem: &str, package: Option<&str>) -> String {
    // A file-name character illegal in a JVM class name (`.`, `;`, `[`, `/`, `<`, `>`, `:`) becomes
    // `_` — e.g. `foo.1.0.kt` → `Foo_1_0Kt` (a verbatim `.` would emit a `ClassFormatError`).
    let sanitized: String = file_stem
        .chars()
        .map(|c| if ".;[]/<>:".contains(c) { '_' } else { c })
        .collect();
    let mut base = String::new();
    let mut chars = sanitized.chars();
    if let Some(c) = chars.next() {
        base.extend(c.to_uppercase());
    }
    base.push_str(chars.as_str());
    base.push_str("Kt");
    match package {
        Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), base),
        _ => base,
    }
}

pub use crate::names::property_getter_name;

/// Convert a semantic classifier name to its physical JVM classfile name. Kotlin metadata spells
/// nested classifiers with dots in the class tail (`pkg/Outer.Inner`); class constants use `$`.
pub fn classfile_internal_name(internal: &str) -> String {
    if let Some(intrinsic) = crate::jvm::jvm_class_map::intrinsic_companion_to_jvm(internal) {
        return intrinsic;
    }
    let mapped = crate::jvm::jvm_class_map::to_jvm_internal(internal);
    let tail = mapped.rfind('/').map_or(0, |slash| slash + 1);
    if mapped[tail..].contains('.') {
        format!("{}{}", &mapped[..tail], mapped[tail..].replace('.', "$"))
    } else {
        mapped.to_string()
    }
}

/// The `java.util` method name a mapped `kotlin.collections` interface declares for a Kotlin *property*
/// member (`Map.keys` → `keySet()`, `Collection.size` → `size()`), from `JavaToKotlinClassMap`'s
/// SpecialBuiltinMembers. `None` for a property with no special stub (its interface method is the plain
/// `get<Name>` getter). A class implementing such an interface must emit this method as a bridge that
/// forwards to the Kotlin getter, or the `java.util` abstract stays unimplemented. The READ direction of
/// this same mapping lives in `Classpath::member` (the classpath member-name resolution).
pub fn collection_property_stub_name(prop: &str) -> Option<&'static str> {
    collection_property_stub(prop).map(|(name, _)| name)
}

pub fn collection_property_stub(prop: &str) -> Option<(&'static str, crate::types::Ty)> {
    use crate::types::Ty;
    match prop {
        "size" => Some(("size", Ty::Int)),
        "values" => Some(("values", Ty::obj("kotlin/collections/Collection"))),
        "keys" => Some(("keySet", Ty::obj("kotlin/collections/Set"))),
        "entries" => Some(("entrySet", Ty::obj("kotlin/collections/Set"))),
        _ => None,
    }
}

pub use crate::names::property_setter_name;

/// Physical JVM name for a mapped Kotlin virtual member.
pub fn mapped_builtin_virtual_name<'a>(owner: &str, name: &'a str) -> &'a str {
    match (owner, name) {
        ("java/lang/CharSequence", "get") => "charAt",
        ("java/lang/String", "get") | ("kotlin/String", "get") => "charAt",
        ("java/lang/StringBuilder", "get") | ("kotlin/text/StringBuilder", "get") => "charAt",
        (
            "kotlin/ranges/IntRange" | "kotlin/ranges/LongRange" | "kotlin/ranges/CharRange",
            "start",
        ) => "getFirst",
        (
            "kotlin/ranges/IntRange" | "kotlin/ranges/LongRange" | "kotlin/ranges/CharRange",
            "endInclusive",
        ) => "getLast",
        ("java/util/Map" | "kotlin/collections/Map" | "kotlin/collections/MutableMap", "keys") => {
            "keySet"
        }
        (
            "java/util/Map" | "kotlin/collections/Map" | "kotlin/collections/MutableMap",
            "entries",
        ) => "entrySet",
        (
            "kotlin/reflect/KCallable"
            | "kotlin/reflect/KProperty"
            | "kotlin/reflect/KProperty0"
            | "kotlin/reflect/KProperty1"
            | "kotlin/reflect/KMutableProperty0"
            | "kotlin/reflect/KMutableProperty1",
            "name",
        ) => "getName",
        // `MutableList.removeAt(Int)` is `java.util.List.remove(int)` — kotlinc's
        // `BuiltinMethodsWithDifferentJvmName`, the same table as `CharSequence.get`/`Number.toInt`.
        // The read-only `List` has no `removeAt`, so only the mutable Kotlin name and the erased JVM
        // owner a `MutableList` call carries need an entry.
        ("java/util/List" | "kotlin/collections/MutableList", "removeAt") => "remove",
        ("java/lang/Number", "toByte") => "byteValue",
        ("java/lang/Number", "toShort") => "shortValue",
        ("java/lang/Number", "toInt") => "intValue",
        ("java/lang/Number", "toLong") => "longValue",
        ("java/lang/Number", "toFloat") => "floatValue",
        ("java/lang/Number", "toDouble") => "doubleValue",
        _ => name,
    }
}

pub fn mapped_builtin_virtual_source_name<'a>(owner: &str, name: &'a str) -> &'a str {
    match (owner, name) {
        ("java/lang/Number", "byteValue") => "toByte",
        ("java/lang/Number", "shortValue") => "toShort",
        ("java/lang/Number", "intValue") => "toInt",
        ("java/lang/Number", "longValue") => "toLong",
        ("java/lang/Number", "floatValue") => "toFloat",
        ("java/lang/Number", "doubleValue") => "toDouble",
        _ => name,
    }
}

fn split_field_descriptor(desc: &str) -> Option<(&str, &str)> {
    let bytes = desc.as_bytes();
    let mut end = bytes.iter().take_while(|byte| **byte == b'[').count();
    match bytes.get(end)? {
        b'L' => end += desc[end..].find(';')? + 1,
        _ => end += 1,
    }
    Some(desc.split_at(end))
}

fn valid_field_descriptor(desc: &str) -> bool {
    let base = desc.trim_start_matches('[');
    matches!(base, "B" | "C" | "D" | "F" | "I" | "J" | "S" | "Z")
        || (base.starts_with('L')
            && base.ends_with(';')
            && base.len() > 2
            && !base[1..base.len() - 1].contains([';', '[']))
}

pub(crate) fn parse_method_descriptor(desc: &str) -> Option<(Vec<&str>, &str)> {
    let body = desc.strip_prefix('(')?;
    let close = body.find(')')?;
    let mut rest = &body[..close];
    let mut params = Vec::new();
    while !rest.is_empty() {
        let (param, tail) = split_field_descriptor(rest)?;
        if !valid_field_descriptor(param) {
            return None;
        }
        params.push(param);
        rest = tail;
    }
    let ret = &body[close + 1..];
    (ret == "V" || valid_field_descriptor(ret)).then_some((params, ret))
}

pub(crate) fn reference_array_element(ty: Ty) -> Ty {
    match ty {
        Ty::Nullable(inner) => Ty::nullable(reference_array_element(*inner)),
        Ty::Unit => Ty::obj("kotlin/Unit"),
        Ty::Nothing => Ty::obj("kotlin/Nothing"),
        other => other.boxed_ref().unwrap_or(other),
    }
}

/// A JVM method descriptor `(params)ret` from krusty `Ty`s.
pub fn method_descriptor(params: &[Ty], ret: Ty) -> String {
    let mut s = String::from("(");
    s.push_str(&params_descriptor(params));
    s.push(')');
    s.push_str(&type_descriptor(ret));
    s
}

/// The parameter-only JVM descriptor key used where JVM lowering needs an overload identity.
pub fn params_descriptor(params: &[Ty]) -> String {
    params.iter().map(|t| type_descriptor(*t)).collect()
}

/// The JVM array descriptor for a primitive-array class name (`kotlin/IntArray` → `[I`), or `None`.
fn primitive_array_descriptor(internal: impl InternalName) -> Option<&'static str> {
    if internal.internal_matches("kotlin/IntArray") {
        Some("[I")
    } else if internal.internal_matches("kotlin/LongArray")
        || internal.internal_matches("kotlin/ULongArray")
    {
        Some("[J")
    } else if internal.internal_matches("kotlin/ShortArray") {
        Some("[S")
    } else if internal.internal_matches("kotlin/ByteArray") {
        Some("[B")
    } else if internal.internal_matches("kotlin/BooleanArray") {
        Some("[Z")
    } else if internal.internal_matches("kotlin/CharArray") {
        Some("[C")
    } else if internal.internal_matches("kotlin/FloatArray") {
        Some("[F")
    } else if internal.internal_matches("kotlin/DoubleArray") {
        Some("[D")
    } else if internal.internal_matches("kotlin/UIntArray") {
        Some("[I")
    } else {
        None
    }
}

/// JVM class-constant spelling for a Kotlin array classifier. Array classes use their descriptor as
/// the `CONSTANT_Class` name (`IntArray::class.java` → `[I`); ordinary classifiers use an internal
/// name instead. `Array` is erased here because a classifier-only owner has no element argument.
pub fn array_class_descriptor(internal: impl InternalName) -> Option<String> {
    if internal.internal_matches("kotlin/Array") {
        Some("[Ljava/lang/Object;".to_string())
    } else {
        primitive_array_descriptor(internal).map(str::to_string)
    }
}

/// A JVM field/type descriptor from a krusty `Ty`.
pub fn type_descriptor(ty: Ty) -> String {
    // `@Metadata` spells a nested class with a dot (`kotlin/coroutines/CoroutineContext.Key`), and
    // the frontend deliberately KEEPS that spelling for the stdlib-mapped nested collections
    // (`Map.Entry`) so their extensions match. A descriptor is the JVM-emission boundary: dots in
    // the class segment are nested separators and MUST be `$` here — emitted raw, the JVM refuses
    // to load the class (ClassFormatError). Normalizing at this one boundary, rather than at the
    // metadata decode sites, leaves the frontend's spelling equilibrium untouched and covers every
    // `Ty` that reaches bytecode.
    let obj_desc = |internal: &str| format!("L{};", classfile_internal_name(internal));
    match ty {
        // The resolution engine converts an undetermined declaration into a decline before
        // anything is emitted, so reaching emission with one is a broken invariant, not a shape to
        // encode. Silently writing `Object` here is how a wrong descriptor used to ship.
        Ty::Pending => unreachable!("a not-determined type reached {}", "a JVM descriptor"),
        Ty::Int => "I".into(),
        Ty::Byte => "B".into(),
        Ty::Short => "S".into(),
        Ty::Long => "J".into(),
        Ty::Float => "F".into(),
        Ty::Double => "D".into(),
        Ty::Boolean => "Z".into(),
        Ty::Char => "C".into(),
        // An unsigned type erases to the signed primitive it is an inline class over.
        Ty::UByte => "B".into(),
        Ty::UShort => "S".into(),
        Ty::UInt => "I".into(),
        Ty::ULong => "J".into(),
        Ty::String => obj_desc("kotlin/String"),
        Ty::Unit => "V".into(),
        // A boxed `Array<T>` (`Obj("kotlin/Array", [T])`) is `[<boxed T>` (`Array<Int>` = `[Ljava/lang/Integer;`),
        // and a primitive array class name (`kotlin/IntArray`) is its JVM array descriptor (`[I`) — without
        // this they would descriptor to a bogus `Lkotlin/Array;`/`Lkotlin/IntArray;` class.
        Ty::Obj(n, args) if n.matches("kotlin/Array") => {
            let e = args
                .first()
                .copied()
                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
            format!("[{}", type_descriptor(reference_array_element(e)))
        }
        Ty::Obj(n, _) if primitive_array_descriptor(n).is_some() => {
            primitive_array_descriptor(n).unwrap().into()
        }
        Ty::Obj(n, _) => obj_desc(&n.render()),
        // `Nothing` is uninhabited, so no value ever has this descriptor — but it IS written into
        // signatures (`fun boom(): Nothing`, `fun f(n: Nothing)`, a `Nothing` getter), and kotlinc
        // writes `java.lang.Void` there, not `Object`. A caller compiled against kotlinc's ABI links
        // against that descriptor.
        Ty::Nothing => obj_desc("java/lang/Void"),
        Ty::Null | Ty::Error => obj_desc("kotlin/Any"),
        Ty::Fun(s) => format!(
            "Lkotlin/jvm/functions/Function{};",
            s.params.len() + usize::from(s.suspend)
        ),
        Ty::Nullable(inner) => match *inner {
            Ty::Unit => obj_desc("kotlin/Unit"),
            Ty::UByte => obj_desc("kotlin/UByte"),
            Ty::UShort => obj_desc("kotlin/UShort"),
            Ty::UInt => obj_desc("kotlin/UInt"),
            Ty::ULong => obj_desc("kotlin/ULong"),
            other => type_descriptor(other.boxed_ref().unwrap_or(other)),
        },
        Ty::TyParam(_, bound) | Ty::PlatformNullable(bound) | Ty::OutProjection(bound) => {
            type_descriptor(*bound)
        }
        // An `in X` occurrence says a caller may WRITE an `X` there; a value read back through it
        // is only known to be `Any?`, so it erases to `Object` rather than to `X`.
        Ty::InProjection(_) => obj_desc("java/lang/Object"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ty_param_descriptor_erases_to_its_bound() {
        let bounded = Ty::ty_param("T", Ty::obj("kotlin/CharSequence"));
        assert_eq!(
            type_descriptor(bounded),
            type_descriptor(Ty::obj("kotlin/CharSequence"))
        );

        let unbounded = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        assert_eq!(
            type_descriptor(unbounded),
            type_descriptor(Ty::obj("kotlin/Any"))
        );
    }

    #[test]
    fn unit_array_uses_the_unit_reference_descriptor() {
        let array = Ty::obj_args("kotlin/Array", &[Ty::Unit]);
        assert_eq!(type_descriptor(array), "[Lkotlin/Unit;");
    }

    #[test]
    fn nested_object_descriptors_normalize_only_the_class_tail() {
        // A `Ty` uses `/` for its package and may retain metadata's source-facing dots between nested
        // classifiers. The shared descriptor boundary owns the complete conversion: it preserves the
        // package path and turns every class-tail dot into `$`, including more than one nesting level.
        // This direct contract guard keeps classpath matching and bytecode emission from growing local
        // `.replace` repairs for individual providers or call sites.
        assert_eq!(
            type_descriptor(Ty::obj("sample/pkg/Outer.Middle.Inner")),
            "Lsample/pkg/Outer$Middle$Inner;"
        );
        assert_eq!(
            type_descriptor(Ty::obj("kotlin/collections/Map.Entry")),
            "Ljava/util/Map$Entry;"
        );
    }

    #[test]
    fn nullable_signed_primitive_descriptor_boxes_to_jvm_wrapper() {
        assert_eq!(
            type_descriptor(Ty::nullable(Ty::Int)),
            "Ljava/lang/Integer;"
        );
        assert_eq!(
            type_descriptor(Ty::nullable(Ty::Boolean)),
            "Ljava/lang/Boolean;"
        );
    }

    #[test]
    fn nullable_unsigned_primitive_descriptor_boxes_to_inline_class() {
        assert_eq!(type_descriptor(Ty::nullable(Ty::UInt)), "Lkotlin/UInt;");
        assert_eq!(type_descriptor(Ty::nullable(Ty::ULong)), "Lkotlin/ULong;");
    }

    #[test]
    fn nullable_unit_descriptor_is_singleton_reference() {
        assert_eq!(type_descriptor(Ty::nullable(Ty::Unit)), "Lkotlin/Unit;");
        assert_eq!(
            method_descriptor(&[Ty::nullable(Ty::Unit)], Ty::Unit),
            "(Lkotlin/Unit;)V"
        );
    }

    #[test]
    fn nullable_reference_descriptor_matches_non_null() {
        assert_eq!(
            type_descriptor(Ty::nullable(Ty::String)),
            type_descriptor(Ty::String)
        );

        let p = Ty::obj("demo/Point");
        assert_eq!(type_descriptor(Ty::nullable(p)), type_descriptor(p));
    }
}
