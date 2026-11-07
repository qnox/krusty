//! Small, backend-agnostic JVM naming/descriptor helpers (relocated out of the retired AST emitter).

use crate::types::{InternalName, Ty, TyKind, TypeName};

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
    let base = crate::names::file_part_short_name(file_stem);
    match package {
        Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), base),
        _ => base,
    }
}

pub use crate::names::property_getter_name;

/// A classifier's class id with `$` between nested segments (`app/Outer.Inner` → `app/Outer$Inner`),
/// read from the name tree. Unlike [`classfile_internal_name`] it maps no built-in.
pub(super) fn binary_class_name(classifier: TypeName) -> String {
    classifier.jvm_binary_name()
}

/// Convert a semantic classifier name to its physical JVM classfile name. Kotlin metadata spells
/// nested classifiers with dots in the class tail (`pkg/Outer.Inner`); class constants use `$`.
///
/// The mapping reads only whether the name is interned, and an interned name stays interned, so the
/// physical name of a name interned before it is mapped is remembered per thread.
pub fn classfile_internal_name(internal: &str) -> std::borrow::Cow<'static, str> {
    thread_local! {
        static BY_SPELLING: std::cell::RefCell<crate::fx_hash::HashMap<Box<str>, &'static str>> =
            std::cell::RefCell::default();
    }
    // Class constants map the same spellings over and over, and finding a spelling's identity
    // probes the name tree once per package segment; a single probe here answers a repeat.
    if let Some(physical) = BY_SPELLING.with(|known| known.borrow().get(internal).copied()) {
        return physical.into();
    }
    if let Some(identity) = crate::types::existing_type_name(internal) {
        let physical = classfile_internal_name_of(identity);
        BY_SPELLING.with(|known| known.borrow_mut().insert(internal.into(), physical));
        return physical.into();
    }
    physical_classfile_name(internal).into()
}

/// Physical JVM classfile name of an interned classifier. The spelling is retained once per
/// emitting thread. A repeated lookup returns that same text and does not render or allocate.
/// Callers that already hold a [`TypeName`] use this instead of rendering the name into
/// [`classfile_internal_name`].
pub(super) fn classfile_internal_name_of(internal: TypeName) -> &'static str {
    thread_local! {
        static INTERNED: std::cell::RefCell<crate::fx_hash::HashMap<TypeName, &'static str>> =
            std::cell::RefCell::default();
    }
    if let Some(physical) = INTERNED.with(|known| known.borrow().get(&internal).copied()) {
        return physical;
    }
    let physical = Box::leak(physical_classfile_name_of(internal).into_boxed_str());
    INTERNED.with(|known| known.borrow_mut().insert(internal, physical));
    physical
}

/// Physical JVM classfile spelling of `internal`, owned by the caller. This is for class-writer
/// output whose lifetime is bounded by the emitted class rather than the process name catalog.
pub(super) fn owned_classfile_internal_name(internal: TypeName) -> String {
    physical_classfile_name_of(internal)
}

fn physical_classfile_name_of(internal: TypeName) -> String {
    let mapped = crate::jvm::jvm_class_map::to_jvm_classfile_type_name(internal);
    mapped.jvm_binary_name()
}

/// Whether `descriptor` is the object descriptor of `classifier` (`Lpkg/Foo;`). The comparison uses
/// the classifier's internal spelling, not its mapped classfile name, and a miss does not intern
/// the descriptor.
pub(crate) fn descriptor_is_classifier(descriptor: &str, classifier: TypeName) -> bool {
    let Some(raw) = object_descriptor_internal(descriptor) else {
        return false;
    };
    if let Some(name) = crate::types::existing_type_name(raw) {
        return name == classifier;
    }
    classifier.matches(raw)
}

/// Whether `descriptor` names the nested class `owner$nested` (`Lpkg/Owner$Companion;`). A miss
/// does not intern that nested name.
pub(crate) fn descriptor_is_nested_class(descriptor: &str, owner: TypeName, nested: &str) -> bool {
    let Some(raw) = object_descriptor_internal(descriptor) else {
        return false;
    };
    if let Some(name) = crate::types::existing_type_name(raw) {
        return name.nested_owner() == Some(owner) && name.nested_segment_ref() == nested;
    }
    owner.nested_child_matches_path(nested, raw)
}

fn object_descriptor_internal(descriptor: &str) -> Option<&str> {
    let raw = descriptor
        .strip_prefix('L')
        .and_then(|value| value.strip_suffix(';'))?;
    (!raw.is_empty()).then_some(raw)
}

/// Whether `candidate` is `original` with `owner` inserted as the first parameter
/// (`(I)V` and `pkg/Foo` match `(Lpkg/Foo;I)V`). A miss does not intern the candidate's class.
pub(crate) fn descriptor_prepends_classifier(
    original: &str,
    owner: TypeName,
    candidate: &str,
) -> bool {
    let Some(tail) = original.strip_prefix('(') else {
        return false;
    };
    let Some(after_owner) = candidate.strip_prefix("(L") else {
        return false;
    };
    let Some((class, rest)) = after_owner.split_once(';') else {
        return false;
    };
    if rest != tail {
        return false;
    }
    if let Some(name) = crate::types::existing_type_name(class) {
        return name == owner;
    }
    owner.matches(class)
}

fn physical_classfile_name(internal: &str) -> String {
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

/// Physical JVM name for a mapped Kotlin virtual member. The owner is already resolved and
/// interned, so the realization table never depends on whether some earlier operation happened to
/// intern a classfile spelling.
pub fn mapped_builtin_virtual_name<'a>(
    owner: TypeName,
    name: &'a str,
    descriptor: &str,
) -> &'a str {
    if let Some(physical) =
        super::mapped_builtin_declarations::physical_name_for_call(owner, name, descriptor)
    {
        return physical;
    }
    name
}

/// Whether two semantic member spellings address one mapped JVM method.
pub(super) fn same_mapped_virtual_name_of(
    owner: TypeName,
    left: &str,
    right: &str,
    descriptor: &str,
) -> bool {
    mapped_builtin_virtual_name(owner, left, descriptor)
        == mapped_builtin_virtual_name(owner, right, descriptor)
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

fn method_descriptor_parts(desc: &str) -> Option<(&str, &str)> {
    let body = desc.strip_prefix('(')?;
    let close = body.find(')')?;
    let parameters = &body[..close];
    let ret = &body[close + 1..];
    (ret == "V" || valid_field_descriptor(ret)).then_some((parameters, ret))
}

/// The validated return descriptor of a JVM method descriptor without allocating its parameter
/// list. Return-only emission paths use this instead of silently treating malformed input as void.
pub(super) fn parse_method_return_descriptor(desc: &str) -> Option<&str> {
    let (mut rest, ret) = method_descriptor_parts(desc)?;
    while !rest.is_empty() {
        let (param, tail) = split_field_descriptor(rest)?;
        if !valid_field_descriptor(param) {
            return None;
        }
        rest = tail;
    }
    Some(ret)
}

pub(crate) fn parse_method_descriptor(desc: &str) -> Option<(Vec<&str>, &str)> {
    let (mut rest, ret) = method_descriptor_parts(desc)?;
    let mut params = Vec::new();
    while !rest.is_empty() {
        let (param, tail) = split_field_descriptor(rest)?;
        if !valid_field_descriptor(param) {
            return None;
        }
        params.push(param);
        rest = tail;
    }
    Some((params, ret))
}

pub(crate) fn reference_array_element(ty: Ty) -> Ty {
    match ty.kind() {
        TyKind::Nullable(inner) => Ty::nullable(reference_array_element(inner)),
        TyKind::Unit => Ty::obj("kotlin/Unit"),
        TyKind::Nothing => Ty::obj("kotlin/Nothing"),
        _ => ty.boxed_ref().unwrap_or(ty),
    }
}

/// A JVM method descriptor `(params)ret` from krusty `Ty`s.
///
/// Component descriptors are appended directly to the result. Keeping a separate parameter
/// descriptor would allocate and then copy the whole parameter spelling on every call.
pub fn method_descriptor(params: &[Ty], ret: Ty) -> String {
    let mut descriptor = String::with_capacity(params.len().saturating_add(2));
    descriptor.push('(');
    append_type_descriptors(&mut descriptor, params);
    descriptor.push(')');
    let ret = type_descriptor(ret);
    descriptor.push_str(&ret);
    descriptor
}

fn append_type_descriptors(out: &mut String, types: &[Ty]) {
    for ty in types {
        let descriptor = type_descriptor(*ty);
        out.push_str(&descriptor);
    }
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
    assert_determined_descriptor_type(ty);
    let mut descriptor = String::new();
    push_descriptor_shape(ty, &mut descriptor);
    descriptor
}

/// Whether `left` and `right` emit the same JVM descriptor.
///
/// `type_descriptor` builds that spelling for the class file. A comparison only needs the shape:
/// primitive tags, the interned classfile name, or one array dimension. Equal determined types
/// return before any of that work; an undetermined type still trips the emission invariant.
pub(crate) fn same_type_descriptor(left: Ty, right: Ty) -> bool {
    assert_determined_descriptor_type(left);
    assert_determined_descriptor_type(right);
    left == right || descriptor_shapes_match(left, right)
}

fn assert_determined_descriptor_type(ty: Ty) {
    if ty.mentions_pending() || ty.mentions_error() {
        unreachable!("a not-determined type reached {}", "a JVM descriptor");
    }
}

enum DescriptorShape {
    Primitive(u8),
    Class(&'static str),
    /// `[Lname;` for an array that stores this class rather than the element's own descriptor.
    /// `Array<UIntArray>` stores `kotlin.UIntArray`; describing that element as a type emits the
    /// carrier `[[I`.
    ObjectArray(&'static str),
    Array(Ty),
}

fn push_descriptor_shape(ty: Ty, descriptor: &mut String) {
    match descriptor_shape(ty) {
        DescriptorShape::Primitive(tag) => descriptor.push(char::from(tag)),
        DescriptorShape::Class(internal) => {
            descriptor.push('L');
            descriptor.push_str(internal);
            descriptor.push(';');
        }
        DescriptorShape::ObjectArray(internal) => {
            descriptor.push('[');
            descriptor.push('L');
            descriptor.push_str(internal);
            descriptor.push(';');
        }
        DescriptorShape::Array(element) => {
            descriptor.push('[');
            push_descriptor_shape(element, descriptor);
        }
    }
}

fn descriptor_shapes_match(left: Ty, right: Ty) -> bool {
    match (descriptor_shape(left), descriptor_shape(right)) {
        (DescriptorShape::Primitive(left), DescriptorShape::Primitive(right)) => left == right,
        (DescriptorShape::Class(left), DescriptorShape::Class(right)) => left == right,
        (DescriptorShape::ObjectArray(left), DescriptorShape::ObjectArray(right)) => left == right,
        (DescriptorShape::Array(left), DescriptorShape::Array(right)) => {
            descriptor_shapes_match(left, right)
        }
        _ => false,
    }
}

fn descriptor_shape(ty: Ty) -> DescriptorShape {
    match ty.kind() {
        TyKind::Pending | TyKind::Error | TyKind::Var(..) => {
            unreachable!("a not-determined type reached {}", "a JVM descriptor")
        }
        TyKind::INT | TyKind::UINT => DescriptorShape::Primitive(b'I'),
        TyKind::BYTE | TyKind::UBYTE => DescriptorShape::Primitive(b'B'),
        TyKind::SHORT | TyKind::USHORT => DescriptorShape::Primitive(b'S'),
        TyKind::LONG | TyKind::ULONG => DescriptorShape::Primitive(b'J'),
        TyKind::FLOAT => DescriptorShape::Primitive(b'F'),
        TyKind::DOUBLE => DescriptorShape::Primitive(b'D'),
        TyKind::BOOLEAN => DescriptorShape::Primitive(b'Z'),
        TyKind::CHAR => DescriptorShape::Primitive(b'C'),
        TyKind::Unit => DescriptorShape::Primitive(b'V'),
        TyKind::STRING => {
            DescriptorShape::Class(classfile_internal_name_of(crate::types::wk::string()))
        }
        TyKind::Obj(name, args) => {
            if ty.is_reference_array() {
                let element = args
                    .first()
                    .copied()
                    .unwrap_or_else(|| Ty::obj_name(crate::types::wk::any()));
                let element = reference_array_element(element);
                // `Array<UIntArray>` stores the box. The carrier descriptor `[I` would make the
                // array `int[][]`, and storing `kotlin.UIntArray` then fails.
                if let Some(name) = boxed_primitive_array_element(element) {
                    return DescriptorShape::ObjectArray(classfile_internal_name_of(name));
                }
                return DescriptorShape::Array(element);
            }
            if let Some(element) = ty.array_elem() {
                return DescriptorShape::Array(element);
            }
            DescriptorShape::Class(classfile_internal_name_of(name))
        }
        TyKind::Nothing => {
            DescriptorShape::Class(classfile_internal_name_of(crate::types::wk::java_void()))
        }
        TyKind::Null => DescriptorShape::Class(classfile_internal_name_of(crate::types::wk::any())),
        TyKind::Fun(signature) => DescriptorShape::Class(function_classfile_name(
            signature.params.len() + usize::from(signature.suspend),
        )),
        TyKind::Nullable(inner) => match inner {
            Ty::Unit => {
                DescriptorShape::Class(classfile_internal_name_of(crate::types::wk::unit()))
            }
            Ty::UByte => {
                DescriptorShape::Class(classfile_internal_name_of(crate::types::wk::ubyte()))
            }
            Ty::UShort => {
                DescriptorShape::Class(classfile_internal_name_of(crate::types::wk::ushort()))
            }
            Ty::UInt => {
                DescriptorShape::Class(classfile_internal_name_of(crate::types::wk::uint()))
            }
            Ty::ULong => {
                DescriptorShape::Class(classfile_internal_name_of(crate::types::wk::ulong()))
            }
            other => descriptor_shape(other.boxed_ref().unwrap_or(other)),
        },
        TyKind::DefinitelyNotNull(inner) => descriptor_shape(inner),
        TyKind::Intersection(parts) => descriptor_shape(
            super::physical_type::intersection_jvm_carrier(parts.as_slice()),
        ),
        TyKind::TyParam(_, bound)
        | TyKind::PlatformNullable(bound)
        | TyKind::MutabilityFlexible(bound)
        | TyKind::OutProjection(bound)
        | TyKind::StarProjection(bound)
        | TyKind::RawProjection(bound) => descriptor_shape(bound),
        TyKind::InProjection(_) => {
            DescriptorShape::Class(classfile_internal_name_of(crate::types::wk::java_object()))
        }
    }
}

fn function_classfile_name(arity: usize) -> &'static str {
    if uses_function_n(arity) {
        "kotlin/jvm/functions/FunctionN"
    } else {
        FUNCTION_N_INTERNAL[arity]
    }
}

/// An element of Kotlin `Array<T>` that is itself an unsigned primitive-array value class.
///
/// The array stores the box (`kotlin/UIntArray`). The carrier descriptor is `[I`, so `anewarray`
/// and `Array<UIntArray>` must not use that descriptor. This is the one place that decision is
/// made; callers ask this helper instead of repeating it.
pub(crate) fn boxed_primitive_array_element(element: Ty) -> Option<TypeName> {
    let name = element.non_null().obj_internal()?;
    crate::types::prim_array_element(name)
        .is_some_and(Ty::is_unsigned)
        .then_some(name)
}

/// The class operand of `anewarray` for `element`.
pub(crate) fn anewarray_element_class(element: Ty) -> String {
    boxed_primitive_array_element(element)
        .map(|name| classfile_internal_name_of(name).to_string())
        .unwrap_or_else(|| instanceof_internal_name(element.non_null()))
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
    match t.kind() {
        TyKind::STRING => "java/lang/String".to_string(),
        TyKind::Nullable(inner) | TyKind::PlatformNullable(inner) if inner.is_unsigned() => {
            match inner {
                Ty::UByte => "kotlin/UByte".to_string(),
                Ty::UShort => "kotlin/UShort".to_string(),
                Ty::UInt => "kotlin/UInt".to_string(),
                Ty::ULong => "kotlin/ULong".to_string(),
                _ => unreachable!("is_unsigned accepts only the four unsigned scalar types"),
            }
        }
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::MutabilityFlexible(inner) => inner
            .boxed_ref()
            .and_then(Ty::obj_internal)
            .map(|name| crate::jvm::names::classfile_internal_name_of(name).to_string())
            .unwrap_or_else(|| instanceof_internal_name(inner)),
        // An array's reference identity is its descriptor (`[I`, `[Ljava/lang/String;`) — checked before
        // the `Obj` arm since arrays are now `Obj("kotlin/Array")`/`Obj("kotlin/IntArray")` too.
        _ if t.is_array() => type_descriptor(t),
        // Erase a Kotlin built-in name (`kotlin/collections/MutableList`) to its JVM identity here at the
        // bytecode boundary, so `instanceof`/`checkcast`/method-owner refs never leak a Kotlin-only name.
        TyKind::Obj(n, _) => crate::jvm::names::classfile_internal_name_of(n).to_string(),
        // A function type's reference identity is its `kotlin/jvm/functions/FunctionN` interface, so
        // `x is Function1<*, *>` / `x as (A) -> B` test/cast against that class, not `Object`.
        TyKind::Fun(signature) => crate::jvm::names::function_interface_internal_name(
            signature.params.len() + usize::from(signature.suspend),
        ),
        // `Unit` is a real class with one instance, so `x is Unit` is a real question about the
        // object. Without this arm it fell to the erasure below and asked `instanceof
        // java/lang/Object`, which every non-null value passes.
        TyKind::Unit => "kotlin/Unit".to_string(),
        // Everything left is erased: an unbounded type parameter tests against `Object`, which is
        // what Kotlin's erasure means and what kotlinc emits.
        _ => "java/lang/Object".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped_virtual_renames_require_an_exact_declaration_realization() {
        let cases = [
            ("java/lang/CharSequence", "get", "(I)C", "charAt"),
            (
                "java/util/List",
                "removeAt",
                "(I)Ljava/lang/Object;",
                "remove",
            ),
            ("java/lang/Number", "toDouble", "()D", "doubleValue"),
            ("java/lang/Number", "toDouble", "()I", "toDouble"),
            ("demo/Foo", "get", "()V", "get"),
        ];
        for (owner, name, descriptor, renamed) in cases {
            assert_eq!(
                mapped_builtin_virtual_name(crate::types::type_name(owner), name, descriptor),
                renamed,
                "{owner}.{name}"
            );
        }
    }

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
    fn remembered_physical_names_match_the_mapping_before_and_after_interning() {
        let fresh = "sample/remembered/Outer.Inner";
        assert!(crate::types::existing_type_name(fresh).is_none());
        assert_eq!(
            classfile_internal_name(fresh),
            "sample/remembered/Outer$Inner"
        );
        let identity = crate::types::type_name(fresh);
        assert_eq!(
            crate::types::existing_type_name("sample/remembered/Outer$Inner"),
            Some(identity)
        );
        crate::types::type_name("kotlin/Int.Companion");
        for internal in [
            fresh,
            "sample/remembered/Outer$Inner",
            fresh,
            "kotlin/Int.Companion",
            "kotlin/Int.Companion",
        ] {
            assert_eq!(
                classfile_internal_name(internal),
                physical_classfile_name(internal),
                "{internal}"
            );
        }
        assert_eq!(
            classfile_internal_name("kotlin/Int.Companion"),
            "kotlin/jvm/internal/IntCompanionObject"
        );
    }

    #[test]
    fn prepended_receiver_descriptor_does_not_intern_a_miss() {
        let owner = crate::types::type_name("probe/holder6044/Face");
        let nested = crate::types::type_name("probe/holder6044/Outer$Inner");
        let missing = "probe/holder6044/Missing";
        assert!(crate::types::existing_type_name(missing).is_none());
        assert!(descriptor_prepends_classifier(
            "(I)Ljava/lang/String;",
            owner,
            "(Lprobe/holder6044/Face;I)Ljava/lang/String;"
        ));
        assert!(descriptor_prepends_classifier(
            "()V",
            nested,
            "(Lprobe/holder6044/Outer$Inner;)V"
        ));
        assert!(!descriptor_prepends_classifier(
            "(I)Ljava/lang/String;",
            owner,
            "(Lprobe/holder6044/Missing;I)Ljava/lang/String;"
        ));
        assert!(!descriptor_prepends_classifier(
            "(I)V",
            owner,
            "(Lprobe/holder6044/Face;J)V"
        ));
        assert!(crate::types::existing_type_name(missing).is_none());
    }

    #[test]
    fn descriptor_comparison_does_not_intern_a_miss() {
        let owner = crate::types::type_name("probe/desc6044/Outer$Inner");
        let companion = "probe/desc6044/Outer$Inner$Companion";
        let missing = "probe/desc6044/Missing";
        assert!(crate::types::existing_type_name(companion).is_none());
        assert!(crate::types::existing_type_name(missing).is_none());

        assert!(descriptor_is_nested_class(
            "Lprobe/desc6044/Outer$Inner$Companion;",
            owner,
            "Companion"
        ));
        assert!(!descriptor_is_nested_class(
            "Lprobe/desc6044/Outer$Inner$Other;",
            owner,
            "Companion"
        ));
        assert!(descriptor_is_classifier(
            "Lprobe/desc6044/Outer$Inner;",
            owner
        ));
        assert!(!descriptor_is_classifier("Lprobe/desc6044/Missing;", owner));
        assert!(!descriptor_is_classifier("I", owner));

        let root = crate::types::type_name("Root6044");
        assert!(descriptor_is_nested_class(
            "LRoot6044$Companion;",
            root,
            "Companion"
        ));

        assert!(crate::types::existing_type_name(companion).is_none());
        assert!(crate::types::existing_type_name(missing).is_none());
        assert!(crate::types::existing_type_name("Root6044$Companion").is_none());

        let interned = crate::types::type_name(companion);
        assert!(descriptor_is_nested_class(
            "Lprobe/desc6044/Outer$Inner$Companion;",
            owner,
            "Companion"
        ));
        assert!(descriptor_is_classifier(
            "Lprobe/desc6044/Outer$Inner$Companion;",
            interned
        ));
    }

    #[test]
    fn interned_classfile_name_matches_the_string_mapping() {
        for spelling in [
            "kotlin/String",
            "kotlin/collections/MutableList",
            "kotlin/Function1",
            "kotlin/reflect/KFunction2",
            "sample/identity6044/Outer.Inner",
            "kotlin/Int.Companion",
        ] {
            let identity = crate::types::type_name(spelling);
            let physical = classfile_internal_name_of(identity);
            assert_eq!(
                physical,
                classfile_internal_name(&identity.render()).as_ref(),
                "{spelling}"
            );
            assert!(std::ptr::eq(physical, classfile_internal_name_of(identity)));
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
    fn function_classifier_descriptor_uses_the_jvm_interface() {
        assert_eq!(
            type_descriptor(Ty::obj("kotlin/Function1")),
            "Lkotlin/jvm/functions/Function1;"
        );
        // Exercise the remembered identity path as well as the initial conversion.
        assert_eq!(
            type_descriptor(Ty::obj("kotlin/Function1")),
            "Lkotlin/jvm/functions/Function1;"
        );
        assert_eq!(
            type_descriptor(Ty::obj("kotlin/collections/MutableList")),
            "Ljava/util/List;"
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
    fn an_unsigned_array_carrier_is_not_its_boxed_class() {
        let element = Ty::obj("kotlin/UIntArray");
        assert_eq!(type_descriptor(element), "[I");
        assert_eq!(instanceof_internal_name(element), "[I");
        assert_eq!(
            type_descriptor(Ty::obj_args("kotlin/Array", &[element])),
            "[Lkotlin/UIntArray;"
        );
        assert_eq!(
            type_descriptor(Ty::obj_args("kotlin/Array", &[Ty::obj("kotlin/IntArray")])),
            "[[I"
        );
        assert_eq!(anewarray_element_class(element), "kotlin/UIntArray");
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
    fn method_descriptor_appends_each_component_in_declaration_order() {
        assert_eq!(method_descriptor(&[], Ty::Unit), "()V");
        assert_eq!(
            method_descriptor(&[Ty::Int, Ty::Long], Ty::Boolean),
            "(IJ)Z"
        );
        assert_eq!(
            method_descriptor(
                &[
                    Ty::String,
                    Ty::obj("kotlin/IntArray"),
                    Ty::nullable(Ty::Int),
                ],
                Ty::obj("sample/Result"),
            ),
            "(Ljava/lang/String;[ILjava/lang/Integer;)Lsample/Result;"
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

    #[test]
    fn descriptor_equality_matches_the_emitted_spelling() {
        let types = vec![
            Ty::Int,
            Ty::UInt,
            Ty::Byte,
            Ty::UByte,
            Ty::Long,
            Ty::ULong,
            Ty::nullable(Ty::Int),
            Ty::nullable(Ty::UInt),
            Ty::nullable(Ty::Boolean),
            Ty::String,
            Ty::obj("kotlin/String"),
            Ty::obj("java/lang/String"),
            Ty::obj("kotlin/Any"),
            Ty::obj("java/lang/Object"),
            Ty::Null,
            Ty::Nothing,
            Ty::Unit,
            Ty::nullable(Ty::Unit),
            Ty::array(Ty::Int),
            Ty::obj("kotlin/IntArray"),
            Ty::obj("kotlin/UIntArray"),
            Ty::array(Ty::String),
            Ty::array(Ty::array(Ty::Int)),
            Ty::fun(vec![Ty::Int], Ty::String),
            Ty::fun(vec![Ty::String], Ty::Int),
            Ty::fun(vec![Ty::Int, Ty::Int], Ty::Unit),
            Ty::obj("kotlin/Function1"),
            Ty::obj("kotlin/jvm/functions/Function1"),
            Ty::ty_param("T", Ty::obj("kotlin/CharSequence")),
            Ty::from(TyKind::DefinitelyNotNull(Ty::ty_param(
                "D",
                Ty::nullable(Ty::obj("kotlin/Any")),
            ))),
            Ty::nullable(Ty::obj("demo/Point")),
            Ty::obj("demo/Point"),
            Ty::obj("kotlin/collections/Map.Entry"),
            Ty::obj("sample/pkg/Outer.Middle.Inner"),
        ];
        for &left in &types {
            for &right in &types {
                assert_eq!(
                    same_type_descriptor(left, right),
                    type_descriptor(left) == type_descriptor(right),
                    "{left:?} vs {right:?}"
                );
            }
        }
    }

    fn assert_undetermined_descriptor_rejected(label: &str, action: impl FnOnce()) {
        let panic = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)) {
            Ok(()) => panic!("{label}: descriptor action did not panic"),
            Err(panic) => panic,
        };
        let message = if let Some(message) = panic.downcast_ref::<&str>() {
            *message
        } else if let Some(message) = panic.downcast_ref::<String>() {
            message.as_str()
        } else {
            panic!("{label}: descriptor action produced a non-string panic");
        };
        assert_eq!(
            message,
            "internal error: entered unreachable code: a not-determined type reached a JVM descriptor",
            "{label}"
        );
    }

    #[test]
    fn descriptor_apis_reject_direct_and_nested_undetermined_types() {
        let invalid = [
            ("direct pending", Ty::Pending),
            ("direct error", Ty::Error),
            (
                "object argument pending",
                Ty::obj_args("sample/Box", &[Ty::Pending]),
            ),
            (
                "object argument error",
                Ty::obj_args("sample/Box", &[Ty::Error]),
            ),
            (
                "function parameter pending",
                Ty::fun(vec![Ty::Pending], Ty::Unit),
            ),
            (
                "function parameter error",
                Ty::fun(vec![Ty::Error], Ty::Unit),
            ),
            ("function return pending", Ty::fun(Vec::new(), Ty::Pending)),
            ("function return error", Ty::fun(Vec::new(), Ty::Error)),
            (
                "definitely non-null pending",
                Ty::from(TyKind::DefinitelyNotNull(Ty::Pending)),
            ),
            (
                "definitely non-null error",
                Ty::from(TyKind::DefinitelyNotNull(Ty::Error)),
            ),
        ];

        for (label, ty) in invalid {
            assert_undetermined_descriptor_rejected(label, || drop(type_descriptor(ty)));
            assert_undetermined_descriptor_rejected(label, || {
                let _ = same_type_descriptor(ty, ty);
            });
            assert_undetermined_descriptor_rejected(label, || {
                let _ = same_type_descriptor(ty, Ty::String);
            });
            assert_undetermined_descriptor_rejected(label, || {
                let _ = same_type_descriptor(Ty::String, ty);
            });
        }
    }
}
