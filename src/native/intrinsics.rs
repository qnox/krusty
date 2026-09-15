//! Mapping already-selected Kotlin stdlib declarations onto native runtime functions.
//!
//! The frontend has already picked an overload; nothing here is resolution. A declaration arrives
//! as an owner, a name and its semantic parameter types, and the question is only which C function
//! realizes it.
//!
//! **Why the owner is spelled as a JVM facade.** krusty's only symbol provider today reads the
//! Kotlin/JVM stdlib jar, so `kotlin.io.println` is presented as a member of `kotlin/io/ConsoleKt`.
//! That is a property of where the *signatures* come from, not of what gets emitted: the compiled
//! program contains no JVM and links only against `krusty_rt.c`. Phase 7 of
//! `docs/BUILD_AND_NATIVE_PLAN.md` replaces the provider with klib ingestion, at which point the
//! owner becomes a Kotlin package and [`facade_package`] disappears with it.

use crate::types::Ty;

/// Undo the JVM provider's mapping of Kotlin built-ins onto their Java counterparts.
///
/// Part of the same temporary bridge as [`facade_package`]: `kotlin.String` reaches a backend
/// spelled `java/lang/String` because the signatures were read out of a JVM jar. Normalizing here
/// keeps every table below written in Kotlin names, so nothing has to be rewritten when the
/// provider becomes klib-based.
fn kotlin_owner(owner: &str) -> &str {
    match owner {
        "java/lang/String" => "kotlin/String",
        "java/lang/Object" => "kotlin/Any",
        "java/lang/CharSequence" => "kotlin/CharSequence",
        "java/lang/Comparable" => "kotlin/Comparable",
        "java/lang/Number" => "kotlin/Number",
        "java/lang/Throwable" => "kotlin/Throwable",
        "java/lang/Enum" => "kotlin/Enum",
        other => other,
    }
}

/// Is this `kotlin.Any` — the root class, under either spelling the provider may hand over?
///
/// The root declares no state and no constructor to run, so a `super()` reaching it is nothing to
/// emit. Both spellings are checked here for the reason [`kotlin_owner`] exists: `kotlin.Any`
/// arrives as `java/lang/Object` when the signature came out of a JVM jar.
pub(super) fn is_any(owner: crate::types::TypeName) -> bool {
    matches!(kotlin_owner(&owner.render()), "kotlin/Any")
}

/// The Kotlin package a JVM file facade stands for: `kotlin/io/ConsoleKt` → `kotlin/io`.
///
/// Returns `None` for a name that is not a facade, so a member function of a real class can never
/// be mistaken for a top-level one.
fn facade_package(owner: &str) -> Option<&str> {
    let (package, facade) = owner.rsplit_once('/')?;
    facade.strip_suffix("Kt").map(|_| package)
}

/// How a console overload's argument is carried.
enum ConsoleOperand {
    /// A machine scalar, named by the runtime's suffix for it.
    Scalar(&'static str),
    /// A reference — the `Any?` overload, exactly as Kotlin's own overload set selects it.
    Reference,
}

fn console_operand(ty: Ty) -> ConsoleOperand {
    match ty {
        // Each unsigned type has its own entry point rather than taking the `Any?` overload: the
        // value would have to be boxed to reach that one, and it is the runtime that boxes — with
        // the descriptor that makes it print as the value and not as the signed number sharing its
        // bits.
        Ty::UByte => return ConsoleOperand::Scalar("ubyte"),
        Ty::UShort => return ConsoleOperand::Scalar("ushort"),
        Ty::UInt => return ConsoleOperand::Scalar("uint"),
        Ty::ULong => return ConsoleOperand::Scalar("ulong"),
        _ => {}
    }
    match ty {
        Ty::Byte => ConsoleOperand::Scalar("byte"),
        Ty::Short => ConsoleOperand::Scalar("short"),
        Ty::Int => ConsoleOperand::Scalar("int"),
        Ty::Long => ConsoleOperand::Scalar("long"),
        Ty::Char => ConsoleOperand::Scalar("char"),
        Ty::Boolean => ConsoleOperand::Scalar("boolean"),
        Ty::Float => ConsoleOperand::Scalar("float"),
        Ty::Double => ConsoleOperand::Scalar("double"),
        _ => ConsoleOperand::Reference,
    }
}

/// The runtime function realizing a selected dependency callable, or `None` when the native
/// runtime does not implement that declaration yet.
pub(super) fn runtime_function(owner: &str, name: &str, params: &[Ty]) -> Option<String> {
    match (facade_package(kotlin_owner(owner))?, name, params) {
        ("kotlin/io", "println", []) => Some("kt_println_unit".to_string()),
        ("kotlin/io", name @ ("print" | "println"), [argument]) => {
            match console_operand(*argument) {
                ConsoleOperand::Scalar(suffix) => Some(format!("kt_{name}_{suffix}")),
                ConsoleOperand::Reference => Some(format!("kt_{name}_any")),
            }
        }
        _ => None,
    }
}

/// A question Kotlin lets a program ask of a floating-point value directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FloatPredicate {
    /// `Double.isNaN()` / `Float.isNaN()`.
    IsNaN,
    /// `isInfinite()`, true for either infinity and false for `NaN`.
    IsInfinite,
    /// `isFinite()`, the negation of both of the above.
    IsFinite,
}

/// Which of those a selected dependency member is, or `None` for anything else.
///
/// They are extensions on the primitive, so they reach a backend as members of the file facade the
/// stdlib declares them in — `kotlin/NumbersKt` here, which is the `kotlin/io/ConsoleKt` situation
/// again and is normalized in the same place. Each is one comparison, so naming them lets the
/// generator emit that rather than call into the runtime with a boxed operand.
pub(super) fn float_predicate(owner: &str, name: &str) -> Option<FloatPredicate> {
    if facade_package(kotlin_owner(owner))? != "kotlin" {
        return None;
    }
    match name {
        "isNaN" => Some(FloatPredicate::IsNaN),
        "isInfinite" => Some(FloatPredicate::IsInfinite),
        "isFinite" => Some(FloatPredicate::IsFinite),
        _ => None,
    }
}

/// Which member of `kotlin.Enum` an accessor names, or `None` for anything else.
///
/// Every enum constant answers `name` and `ordinal` from the storage its base contributes, and the
/// accessor reaches a backend spelled as the provider named it — `getName` on `java/lang/Enum`,
/// since the signatures come from a JVM jar. Normalizing that here keeps the one place that knows
/// the spelling the same one that knows every other.
pub(super) fn enum_member(owner: &str, accessor: &str) -> Option<&'static str> {
    if kotlin_owner(owner) != "kotlin/Enum" {
        return None;
    }
    match accessor {
        "getName" | "name" => Some("name"),
        "getOrdinal" | "ordinal" => Some("ordinal"),
        _ => None,
    }
}

/// What a scope function's call yields once its block has run.
///
/// `apply`, `also`, `let` and `run` differ in exactly this and in nothing else: each evaluates the
/// receiver once, hands it to the block, and then yields either the receiver it was called on or
/// whatever the block returned. Kotlin's own signatures say which.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScopeResult {
    /// `T.apply(block: T.() -> Unit): T` and `T.also(block: (T) -> Unit): T`.
    Receiver,
    /// `T.let(block: (T) -> R): R` and `T.run(block: T.() -> R): R`.
    BlockResult,
}

/// Which scope function a selected dependency member is, or `None` for anything else.
///
/// A block written at the call site never arrives here: these are `inline`, and the checked
/// lowering splices such a block into its caller. What arrives is the call whose block is an
/// ordinary function VALUE, which has no body to splice.
pub(super) fn scope_function(owner: &str, name: &str) -> Option<ScopeResult> {
    if facade_package(kotlin_owner(owner))? != "kotlin" {
        return None;
    }
    match name {
        "apply" | "also" => Some(ScopeResult::Receiver),
        "let" | "run" => Some(ScopeResult::BlockResult),
        _ => None,
    }
}

/// The runtime function realizing a selected dependency MEMBER, called with the receiver as its
/// first argument. Receiver and arguments are passed as references, so a scalar receiver boxes —
/// which is what `4.toString()` means anyway.
/// `a until b` — the half-open range builder, which the provider presents as an extension function
/// of the ranges file facade rather than a member of the range it answers.
///
/// Recognized here rather than where ranges are lowered, for the same reason [`facade_package`]
/// lives here: the facade is the JVM provider's spelling, not Kotlin's.
pub(super) fn is_range_until(owner: &str, name: &str, arity: usize) -> bool {
    facade_package(kotlin_owner(owner)) == Some("kotlin/ranges") && name == "until" && arity == 1
}

/// Whether a getter is `KCallable.name` — the one member of the reflection surface whose answer a
/// program can have without any reflection metadata existing, because the declaration it names is
/// written in the same file.
pub(super) fn is_callable_name(owner: crate::types::TypeName, name: &str) -> bool {
    name == "getName"
        && [
            "kotlin/reflect/KCallable",
            "kotlin/reflect/KFunction",
            "kotlin/reflect/KProperty",
        ]
        .iter()
        .any(|candidate| owner.matches(candidate))
}

/// Whether a declaration's owner is the file facade the stdlib's delegate operators on a property
/// reference live in. `kotlin.getValue`/`kotlin.setValue` are top-level extensions of
/// `KProperty0`/`KProperty1`, so they reach a backend as members of
/// `kotlin/PropertyReferenceDelegatesKt`.
pub(super) fn is_property_delegates_facade(owner: &str) -> bool {
    kotlin_owner(owner) == "kotlin/PropertyReferenceDelegatesKt"
}

/// Whether a declaration's owner is the file facade `lazy` and `Lazy.getValue` live in. Both are
/// top-level declarations of `kotlin`, so they reach a backend as members of `kotlin/LazyKt`.
pub(super) fn is_lazy_facade(owner: &str) -> bool {
    kotlin_owner(owner) == "kotlin/LazyKt"
}

/// Whether a declaration's owner is the file facade `to` lives in. `kotlin.to` is a top-level
/// extension, so it reaches a backend as a member of `kotlin/TuplesKt` — the `kotlin/io/ConsoleKt`
/// situation again, normalized in the same place.
pub(super) fn is_tuples_facade(owner: &str) -> bool {
    kotlin_owner(owner) == "kotlin/TuplesKt"
}

/// Whether a declaration's owner is the collections file facade `listOf` and its neighbours live
/// in. They are top-level functions of `kotlin.collections`, so they reach a backend as members of
/// the facade class the stdlib declares them in — the `kotlin/io/ConsoleKt` situation again, and
/// normalized in the same place.
pub(super) fn is_collections_facade(owner: &str) -> bool {
    facade_package(kotlin_owner(owner)) == Some("kotlin/collections")
}

/// How a program iterates a receiver it could only type by an INTERFACE.
///
/// `Iterable` and `Iterator` both have a Kotlin spelling and a `java.util` one — the provider
/// presents a Kotlin collection interface under whichever name the declaration it read carried —
/// and normalizing that here is the point: a backend asks which of the two roles a type plays, not
/// which library spelled it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IterationRole {
    /// Something a `for` loop asks for an iterator.
    Iterable,
    /// The iterator itself.
    Iterator,
}

/// Which role a type name plays, or `None` for anything that plays neither.
pub(super) fn iteration_role(internal: crate::types::TypeName) -> Option<IterationRole> {
    [
        ("kotlin/collections/Iterable", IterationRole::Iterable),
        ("kotlin/collections/Collection", IterationRole::Iterable),
        ("kotlin/collections/List", IterationRole::Iterable),
        ("java/lang/Iterable", IterationRole::Iterable),
        ("java/util/Collection", IterationRole::Iterable),
        ("java/util/List", IterationRole::Iterable),
        ("kotlin/collections/Iterator", IterationRole::Iterator),
        ("java/util/Iterator", IterationRole::Iterator),
    ]
    .into_iter()
    .find_map(|(candidate, role)| internal.matches(candidate).then_some(role))
}

/// Whether a type name is the read-only list the native runtime builds, under either spelling.
pub(super) fn is_list_type(internal: crate::types::TypeName) -> bool {
    [
        "kotlin/collections/List",
        "kotlin/collections/Collection",
        "java/util/List",
        "java/util/Collection",
    ]
    .iter()
    .any(|candidate| internal.matches(candidate))
}

/// `x.indices` — the range of an indexable value's positions, which the provider presents as an
/// extension property of the arrays or text file facade rather than a member.
///
/// Answering it needs the receiver's own `size`, so only the caller can decide whether THIS
/// receiver has one; this says only that the declaration named is that extension property.
pub(super) fn is_indices(owner: &str, name: &str) -> bool {
    name == "getIndices"
        && matches!(
            facade_package(kotlin_owner(owner)),
            Some("kotlin/collections" | "kotlin/text")
        )
}

/// A dependency member the runtime answers with its arguments carried as VALUES, and the signature
/// it is called with: `(receiver and parameters, result)`.
///
/// The ordinary member path crosses everything as a reference, which is right for a member that
/// asks about an object and wrong for one that asks about a NUMBER — `s[i]` would box the index to
/// pass it and the runtime would read the box as the index.
pub(super) fn scalar_member(
    owner: &str,
    name: &str,
    params: &[Ty],
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    let reference = Ty::nullable(Ty::obj("kotlin/Any"));
    match (kotlin_owner(owner), name, params) {
        ("kotlin/String" | "kotlin/CharSequence", "get", [Ty::Int]) => {
            Some(("kt_string_get", vec![reference, Ty::Int], Ty::Char))
        }
        _ => None,
    }
}

/// The unsigned integer a value-class member is declared on, for an owner that names one.
///
/// Kotlin compiles each of these members to a static taking the wrapped value, and the provider
/// presents them under the value class's own name with kotlinc's `-impl` suffix. Both are spellings,
/// which is why they are read here; what the caller gets back is the TYPE, which is what decides
/// how wide the operands are and which questions are asked unsigned.
pub(super) fn unsigned_owner(owner: &str) -> Option<Ty> {
    Some(match kotlin_owner(owner) {
        "kotlin/UByte" => Ty::UByte,
        "kotlin/UShort" => Ty::UShort,
        "kotlin/UInt" => Ty::UInt,
        "kotlin/ULong" => Ty::ULong,
        _ => return None,
    })
}

/// The Kotlin name of a value-class member, with kotlinc's mangling removed.
///
/// A member whose signature mentions a value class is emitted as `name-<suffix>`: `-impl` for the
/// static carrying the wrapped value, and a hash of the signature where an overload would otherwise
/// clash (`compareTo-WZ4Q5Ns`). A Kotlin identifier cannot contain `-`, so everything from the
/// first one is the mangling and the name is what precedes it.
pub(super) fn value_class_member(name: &str) -> &str {
    name.split_once('-').map_or(name, |(kotlin, _)| kotlin)
}

pub(super) fn runtime_member(owner: &str, name: &str, params: &[Ty]) -> Option<&'static str> {
    match (kotlin_owner(owner), name, params) {
        ("kotlin/String", "plus", [_]) => Some("kt_string_plus"),
        (_, "toString", []) => Some("kt_to_string"),
        // `kotlin.Any`'s other two members, dispatched through the receiver's vtable.
        (_, "hashCode", []) => Some("kt_hash_code"),
        (_, "equals", [_]) => Some("kt_equals"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_facade_names_its_package() {
        assert_eq!(facade_package("kotlin/io/ConsoleKt"), Some("kotlin/io"));
        assert_eq!(facade_package("kotlin/text/StringsKt"), Some("kotlin/text"));
    }

    #[test]
    fn a_class_is_not_a_facade() {
        // `kotlin/text/Regex` has no `Kt` suffix, and `kotlin/collections/AbstractMutableList` must
        // not be read as a facade for `kotlin/collections` merely because its name ends in `t`.
        assert_eq!(facade_package("kotlin/text/Regex"), None);
        assert_eq!(
            facade_package("kotlin/collections/AbstractMutableList"),
            None
        );
        assert_eq!(facade_package("Ungrouped"), None);
    }

    #[test]
    fn console_overloads_select_by_parameter_representation() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        assert_eq!(
            runtime_function("kotlin/io/ConsoleKt", "println", &[any]).as_deref(),
            Some("kt_println_any"),
            "a reference argument takes the Any? overload, as it does in Kotlin"
        );
        assert_eq!(
            runtime_function("kotlin/io/ConsoleKt", "println", &[Ty::Int]).as_deref(),
            Some("kt_println_int"),
            "a scalar argument must not be boxed to reach the console"
        );
        assert_eq!(
            runtime_function("kotlin/io/ConsoleKt", "print", &[Ty::Boolean]).as_deref(),
            Some("kt_print_boolean")
        );
        assert_eq!(
            runtime_function("kotlin/io/ConsoleKt", "println", &[]).as_deref(),
            Some("kt_println_unit")
        );
    }

    #[test]
    fn the_half_open_range_builder_is_recognized_on_its_facade() {
        assert!(is_range_until("kotlin/ranges/RangesKt", "until", 1));
        assert!(is_range_until("kotlin/ranges/URangesKt", "until", 1));
        // A member of a real class in the same package is not the facade's extension, and neither
        // is a same-named function of another package.
        assert!(!is_range_until("kotlin/ranges/IntRange", "until", 1));
        assert!(!is_range_until("kotlin/text/StringsKt", "until", 1));
        assert!(!is_range_until("kotlin/ranges/RangesKt", "downTo", 1));
        assert!(!is_range_until("kotlin/ranges/RangesKt", "until", 2));
    }

    #[test]
    fn the_indices_extension_is_recognized_on_either_facade() {
        assert!(is_indices("kotlin/collections/ArraysKt", "getIndices"));
        assert!(is_indices("kotlin/text/StringsKt", "getIndices"));
        assert!(!is_indices("kotlin/collections/ArraysKt", "getSize"));
        assert!(!is_indices("kotlin/collections/AbstractList", "getIndices"));
    }

    #[test]
    fn an_unsigned_member_is_recognized_by_its_value_class() {
        assert_eq!(unsigned_owner("kotlin/UInt"), Some(Ty::UInt));
        assert_eq!(unsigned_owner("kotlin/ULong"), Some(Ty::ULong));
        assert_eq!(unsigned_owner("kotlin/Int"), None);
        // kotlinc mangles a value class's members; the Kotlin name is what a table is written in.
        // Both spellings occur: `-impl` for the static, and a signature hash where an overload
        // would otherwise clash.
        assert_eq!(value_class_member("toString-impl"), "toString");
        assert_eq!(value_class_member("compareTo-WZ4Q5Ns"), "compareTo");
        assert_eq!(value_class_member("plus"), "plus");
    }

    #[test]
    fn a_java_spelled_builtin_is_normalized_to_its_kotlin_name() {
        // The JVM jar presents `kotlin.String` as `java.lang.String`. A table written in Kotlin
        // names must still match, or every Kotlin built-in would silently go unimplemented.
        assert_eq!(
            runtime_member("java/lang/String", "plus", &[Ty::String]),
            Some("kt_string_plus")
        );
    }

    #[test]
    fn string_concatenation_and_rendering_are_runtime_members() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        assert_eq!(
            runtime_member("kotlin/String", "plus", &[any]),
            Some("kt_string_plus")
        );
        assert_eq!(
            runtime_member("kotlin/Int", "toString", &[]),
            Some("kt_to_string")
        );
        assert_eq!(
            runtime_member("kotlin/Any", "hashCode", &[]),
            Some("kt_hash_code")
        );
        assert_eq!(
            runtime_member("kotlin/Any", "equals", &[any]),
            Some("kt_equals")
        );
        assert_eq!(
            runtime_member("kotlin/String", "repeat", &[Ty::Int]),
            None,
            "an unimplemented member must decline"
        );
    }

    #[test]
    fn a_floating_point_value_prints_through_its_own_overload() {
        // Kotlin's overload set has one per primitive, and so does the runtime: the value reaches
        // it unboxed, and `krusty_fp.c` decides what it looks like.
        assert_eq!(
            runtime_function("kotlin/io/ConsoleKt", "println", &[Ty::Double]).as_deref(),
            Some("kt_println_double")
        );
        assert_eq!(
            runtime_function("kotlin/io/ConsoleKt", "print", &[Ty::Float]).as_deref(),
            Some("kt_print_float")
        );
    }

    #[test]
    fn an_unimplemented_declaration_is_declined_rather_than_guessed() {
        assert_eq!(
            runtime_function("kotlin/text/StringsKt", "repeat", &[Ty::String, Ty::Int]),
            None,
            "a missing runtime function must produce a diagnostic, never a call to a symbol that \
             does not exist"
        );
        assert_eq!(
            runtime_function("kotlin/io/ConsoleKt", "readLine", &[]),
            None
        );
    }
}
