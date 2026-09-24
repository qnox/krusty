//! Small, backend-agnostic JVM naming/descriptor helpers (relocated out of the retired AST emitter).

use crate::types::{InternalName, Ty};

/// Kotlin's JVM runtime provides numbered function interfaces only through `Function22`.
pub(crate) const MAX_NUMBERED_FUNCTION_ARITY: usize = 22;

/// Physical JVM functional-interface names. This table is representation state and must not leak
/// into parser, resolver, checked FIR, or common IR; those phases retain `kotlin/FunctionN` or the
/// structural [`Ty::Fun`] shape.
pub(super) const FUNCTION_N_INTERNAL: [&str; 23] = [
    "kotlin/jvm/functions/Function0",
    "kotlin/jvm/functions/Function1",
    "kotlin/jvm/functions/Function2",
    "kotlin/jvm/functions/Function3",
    "kotlin/jvm/functions/Function4",
    "kotlin/jvm/functions/Function5",
    "kotlin/jvm/functions/Function6",
    "kotlin/jvm/functions/Function7",
    "kotlin/jvm/functions/Function8",
    "kotlin/jvm/functions/Function9",
    "kotlin/jvm/functions/Function10",
    "kotlin/jvm/functions/Function11",
    "kotlin/jvm/functions/Function12",
    "kotlin/jvm/functions/Function13",
    "kotlin/jvm/functions/Function14",
    "kotlin/jvm/functions/Function15",
    "kotlin/jvm/functions/Function16",
    "kotlin/jvm/functions/Function17",
    "kotlin/jvm/functions/Function18",
    "kotlin/jvm/functions/Function19",
    "kotlin/jvm/functions/Function20",
    "kotlin/jvm/functions/Function21",
    "kotlin/jvm/functions/Function22",
];

pub(crate) fn uses_function_n(arity: usize) -> bool {
    arity > MAX_NUMBERED_FUNCTION_ARITY
}

/// Physical JVM carrier for a Kotlin function value of the given runtime arity.
pub(crate) fn function_interface_internal_name(arity: usize) -> String {
    if uses_function_n(arity) {
        "kotlin/jvm/functions/FunctionN".to_string()
    } else {
        FUNCTION_N_INTERNAL[arity].to_string()
    }
}

/// The file-facade class internal name for a source file: `Foo.kt` → `FooKt` (package-qualified).
pub fn file_class_name(file_stem: &str, package: Option<&str>) -> String {
    let sanitized: String = file_stem
        .chars()
        .map(|character| {
            if character.is_alphabetic() || character.is_numeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    let mut base = String::new();
    let mut characters = sanitized.chars();
    match characters.next() {
        Some(first) if first.is_numeric() => {
            base.push('_');
            base.push_str(&sanitized);
        }
        Some(first) => {
            base.extend(first.to_uppercase());
            base.push_str(characters.as_str());
        }
        None => {}
    }
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

pub use crate::names::property_setter_name;

/// Physical JVM name for a mapped Kotlin virtual member.
pub fn mapped_builtin_virtual_name<'a>(owner: &str, name: &'a str, descriptor: &str) -> &'a str {
    if let Some(owner) = crate::types::existing_type_name(owner) {
        if let Some(physical) =
            super::mapped_builtin_declarations::physical_name_for_call(owner, name, descriptor)
        {
            return physical;
        }
    }
    match (owner, name) {
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
        (
            "kotlin/reflect/KCallable"
            | "kotlin/reflect/KProperty"
            | "kotlin/reflect/KProperty0"
            | "kotlin/reflect/KProperty1"
            | "kotlin/reflect/KMutableProperty0"
            | "kotlin/reflect/KMutableProperty1",
            "name",
        ) => "getName",
        ("java/lang/Number", "toByte") => "byteValue",
        ("java/lang/Number", "toShort") => "shortValue",
        ("java/lang/Number", "toInt") => "intValue",
        ("java/lang/Number", "toLong") => "longValue",
        ("java/lang/Number", "toFloat") => "floatValue",
        ("java/lang/Number", "toDouble") => "doubleValue",
        _ => name,
    }
}

/// Whether two semantic member spellings address one mapped JVM method.
pub(super) fn same_mapped_virtual_name(
    owner: &str,
    left: &str,
    right: &str,
    descriptor: &str,
) -> bool {
    mapped_builtin_virtual_name(owner, left, descriptor)
        == mapped_builtin_virtual_name(owner, right, descriptor)
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
/// The JVM array descriptor for a primitive specialized array class name (`kotlin/IntArray` → `[I`).
///
/// Two existing operations composed, and no table of its own: [`crate::types::prim_array_element`]
/// identifies the element — it documents itself as "the single canonical table" that "the backend
/// descriptor logic" routes through — and [`type_descriptor`] already erases an unsigned element to
/// the signed primitive it is an inline class over. The hand-written list this replaced was the copy
/// that made that documentation untrue: it named `UIntArray` and `ULongArray` and not `UByteArray`
/// or `UShortArray`, so those two descriptored as `Lkotlin/UByteArray;` and would not load at all.
fn primitive_array_descriptor(internal: impl InternalName) -> Option<String> {
    let element = crate::types::prim_array_element(internal)?;
    Some(format!("[{}", type_descriptor(element)))
}

/// JVM class-constant spelling for a Kotlin array classifier. Array classes use their descriptor as
/// the `CONSTANT_Class` name (`IntArray::class.java` → `[I`); ordinary classifiers use an internal
/// name instead. `Array` is erased here because a classifier-only owner has no element argument.
pub fn array_class_descriptor(internal: impl InternalName) -> Option<String> {
    if internal.internal_matches("kotlin/Array") {
        Some("[Ljava/lang/Object;".to_string())
    } else {
        primitive_array_descriptor(internal)
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
        Ty::Obj(n, _) if crate::types::prim_array_element(n).is_some() => {
            primitive_array_descriptor(n).expect("checked in the guard")
        }
        Ty::Obj(n, _) => obj_desc(&n.render()),
        // `Nothing` is uninhabited, so no value ever has this descriptor — but it IS written into
        // signatures (`fun boom(): Nothing`, `fun f(n: Nothing)`, a `Nothing` getter), and kotlinc
        // writes `java.lang.Void` there, not `Object`. A caller compiled against kotlinc's ABI links
        // against that descriptor.
        Ty::Nothing => obj_desc("java/lang/Void"),
        Ty::Null | Ty::Error => obj_desc("kotlin/Any"),
        Ty::Fun(s) => format!(
            "L{};",
            function_interface_internal_name(s.params.len() + usize::from(s.suspend))
        ),
        Ty::Nullable(inner) => match *inner {
            Ty::Unit => obj_desc("kotlin/Unit"),
            Ty::UByte => obj_desc("kotlin/UByte"),
            Ty::UShort => obj_desc("kotlin/UShort"),
            Ty::UInt => obj_desc("kotlin/UInt"),
            Ty::ULong => obj_desc("kotlin/ULong"),
            other => type_descriptor(other.boxed_ref().unwrap_or(other)),
        },
        Ty::TyParam(_, bound)
        | Ty::PlatformNullable(bound)
        | Ty::OutProjection(bound)
        | Ty::StarProjection(bound) => type_descriptor(*bound),
        // An `in X` occurrence says a caller may WRITE an `X` there; a value read back through it
        // is only known to be `Any?`, so it erases to `Object` rather than to `X`.
        Ty::InProjection(_) => obj_desc("java/lang/Object"),
    }
}

/// The class an `instanceof`/`checkcast` names for `t`.
///
/// One mapping, because the two instructions must agree: a value that passes the check is exactly a
/// value the cast admits. It is a REPRESENTATION question — which runtime class stands for a Kotlin
/// type — so it belongs with the other naming here rather than inside the emitter.
///
/// The fallback is erasure and not a default: an unbounded type parameter genuinely tests against
/// `Object`, which is what Kotlin's erasure means and what kotlinc emits. A type that merely has no
/// arm therefore erases silently, which is how `Unit` came to answer `true` for every non-null
/// value, so a Kotlin type with a runtime class of its own is named explicitly.
///
/// `kotlin.Nothing` is deliberately absent: it has no runtime class at all (there is no
/// `kotlin/Nothing.class` in kotlin-stdlib), so no name here could be right. Common lowering
/// settles `is Nothing` before a backend sees it.
pub(crate) fn instanceof_internal_name(t: Ty) -> String {
    match t {
        Ty::String => "java/lang/String".to_string(),
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) if inner.is_unsigned() => match *inner {
            Ty::UByte => "kotlin/UByte".to_string(),
            Ty::UShort => "kotlin/UShort".to_string(),
            Ty::UInt => "kotlin/UInt".to_string(),
            Ty::ULong => "kotlin/ULong".to_string(),
            _ => unreachable!("is_unsigned accepts only the four unsigned scalar types"),
        },
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => inner
            .boxed_ref()
            .and_then(Ty::obj_internal)
            .map(|name| crate::jvm::names::classfile_internal_name(&name.render()))
            .unwrap_or_else(|| instanceof_internal_name(*inner)),
        // An array's reference identity is its descriptor (`[I`, `[Ljava/lang/String;`) — checked before
        // the `Obj` arm since arrays are now `Obj("kotlin/Array")`/`Obj("kotlin/IntArray")` too.
        t if t.is_array() => type_descriptor(t),
        // Erase a Kotlin built-in name (`kotlin/collections/MutableList`) to its JVM identity here at the
        // bytecode boundary, so `instanceof`/`checkcast`/method-owner refs never leak a Kotlin-only name.
        Ty::Obj(n, _) => crate::jvm::names::classfile_internal_name(&n.render()),
        // A function type's reference identity is its `kotlin/jvm/functions/FunctionN` interface, so
        // `x is Function1<*, *>` / `x as (A) -> B` test/cast against that class, not `Object`.
        Ty::Fun(signature) => crate::jvm::names::function_interface_internal_name(
            signature.params.len() + usize::from(signature.suspend),
        ),
        // `Unit` is a real class with one instance, so `x is Unit` is a real question about the
        // object. Without this arm it fell to the erasure below and asked `instanceof
        // java/lang/Object`, which every non-null value passes.
        Ty::Unit => "kotlin/Unit".to_string(),
        // Everything left is erased: an unbounded type parameter tests against `Object`, which is
        // what Kotlin's erasure means and what kotlinc emits.
        _ => "java/lang/Object".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_facade_names_follow_kotlinc_package_part_rules() {
        for (stem, facade) in [
            ("box", "BoxKt"),
            ("1", "_1Kt"),
            (
                "32defaultParametersInSuspend",
                "_32defaultParametersInSuspendKt",
            ),
            ("a-b", "A_bKt"),
            ("x.y", "X_yKt"),
            ("q$r", "Q_rKt"),
            ("_u", "_uKt"),
        ] {
            assert_eq!(file_class_name(stem, None), facade, "{stem}");
        }
    }

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
    fn high_arity_function_descriptor_uses_function_n() {
        let function = Ty::fun(vec![Ty::Int; 23], Ty::Int);
        assert_eq!(
            type_descriptor(function),
            "Lkotlin/jvm/functions/FunctionN;"
        );
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
