//! Mapping already-selected Kotlin stdlib declarations onto native runtime functions.
//!
//! The frontend has already picked an overload; nothing here is resolution. A declaration arrives
//! as an owner, a name and its semantic parameter types, and the question is only which C function
//! realizes it.
//!
//! A JVM provider may physically own a top-level declaration in a file facade while a klib
//! provider owns it in the package. The provider's callable kind records that it is top-level;
//! the backend never infers that fact from a rendered owner or a `Kt` suffix.

use crate::types::{Ty, TypeName};

/// Exact selected declaration owner as the native runtime tables see it.
///
/// `physical` remains the provider-interned identity. `top_level` is the provider's callable kind,
/// not a guess from a `*Kt` suffix; it lets a JVM facade and a future klib package share the same
/// package comparison without making an arbitrary class in that package look top-level.
#[derive(Clone, Copy)]
pub(super) struct DeclarationOwner {
    physical: TypeName,
    top_level: bool,
}

impl DeclarationOwner {
    pub(super) fn callable(physical: TypeName, top_level: bool) -> Self {
        Self {
            physical,
            top_level,
        }
    }

    fn classifier(physical: TypeName) -> Self {
        Self {
            physical,
            top_level: false,
        }
    }

    fn package_matches(self, package: &str) -> bool {
        self.top_level && (self.physical.matches(package) || self.physical.package_matches(package))
    }

    fn classifier_matches(self, kotlin: &str) -> bool {
        classifier_matches(self.physical, kotlin)
    }
}

/// Whether a provider-owned classifier identity denotes this Kotlin builtin.
///
/// JVM mapped types are target ABI aliases, so accepting their exact interned identities here is
/// representation normalization, not semantic lookup. No source spelling is rendered or interned.
fn classifier_matches(owner: TypeName, kotlin: &str) -> bool {
    if owner.matches(kotlin) {
        return true;
    }
    let jvm = match kotlin {
        "kotlin/String" => &["java/lang/String"][..],
        "kotlin/Any" => &["java/lang/Object"],
        "kotlin/Comparable" => &["java/lang/Comparable"],
        "kotlin/Number" => &["java/lang/Number"],
        "kotlin/Boolean" => &["java/lang/Boolean"],
        "kotlin/Byte" => &["java/lang/Byte"],
        "kotlin/Short" => &["java/lang/Short"],
        "kotlin/Int" => &["java/lang/Integer"],
        "kotlin/Long" => &["java/lang/Long"],
        "kotlin/Char" => &["java/lang/Character"],
        "kotlin/Float" => &["java/lang/Float"],
        "kotlin/Double" => &["java/lang/Double"],
        "kotlin/Throwable" => &["java/lang/Throwable"],
        "kotlin/Error" => &["java/lang/Error"],
        "kotlin/Exception" => &["java/lang/Exception"],
        "kotlin/RuntimeException" => &["java/lang/RuntimeException"],
        "kotlin/IllegalStateException" => &["java/lang/IllegalStateException"],
        "kotlin/IllegalArgumentException" => &["java/lang/IllegalArgumentException"],
        "kotlin/AssertionError" => &["java/lang/AssertionError"],
        "kotlin/NullPointerException" => &["java/lang/NullPointerException"],
        "kotlin/ClassCastException" => &["java/lang/ClassCastException"],
        "kotlin/IndexOutOfBoundsException" => &["java/lang/IndexOutOfBoundsException"],
        "kotlin/ArithmeticException" => &["java/lang/ArithmeticException"],
        "kotlin/UnsupportedOperationException" => &["java/lang/UnsupportedOperationException"],
        "kotlin/NumberFormatException" => &["java/lang/NumberFormatException"],
        "kotlin/NoSuchElementException" => &["java/util/NoSuchElementException"],
        "kotlin/ConcurrentModificationException" => &["java/util/ConcurrentModificationException"],
        "kotlin/Enum" => &["java/lang/Enum"],
        "kotlin/Comparator" => &["java/util/Comparator"],
        "kotlin/collections/ArrayList" => &["java/util/ArrayList"],
        "kotlin/collections/List" => &["java/util/List"],
        "kotlin/collections/Collection" => &["java/util/Collection"],
        "kotlin/collections/Iterator" => &["java/util/Iterator"],
        "kotlin/collections/Iterable" => &["java/lang/Iterable"],
        "kotlin/collections/Map" => &["java/util/Map"],
        "kotlin/collections/HashMap" => &["java/util/HashMap"],
        "kotlin/collections/LinkedHashMap" => &["java/util/LinkedHashMap"],
        "kotlin/collections/Map$Entry" => &["java/util/Map$Entry"],
        "kotlin/collections/Set" => &["java/util/Set"],
        "kotlin/collections/HashSet" => &["java/util/HashSet"],
        "kotlin/collections/LinkedHashSet" => &["java/util/LinkedHashSet"],
        "kotlin/text/StringBuilder" => {
            &["java/lang/StringBuilder", "java/lang/AbstractStringBuilder"]
        }
        "kotlin/CharSequence" => &["java/lang/CharSequence"],
        _ => &[],
    };
    jvm.iter().any(|candidate| owner.matches(candidate))
}

/// Is this `kotlin.Any` — the root class, under either spelling the provider may hand over?
///
/// The root declares no state and no constructor to run, so a `super()` reaching it is nothing to
/// emit. The mapped JVM identity is accepted because `kotlin.Any` arrives as `java/lang/Object`
/// when the signature came out of a JVM jar.
pub(super) fn is_any(owner: crate::types::TypeName) -> bool {
    classifier_matches(owner, "kotlin/Any")
}

/// The runtime type descriptor for a `Throwable` the runtime provides, if this is one.
///
/// These classes are declared in no file krusty compiles, so there is no layout and no constructor
/// to call — the runtime carries the descriptor, the one reference field and the `toString` Kotlin
/// specifies, and `kt_throwable_new` builds one. The chain is Kotlin's, so a `catch` clause naming
/// any of them is an ordinary `kt_is_instance` against the descriptor named here.
///
/// A class the PROGRAM declares that extends one of these is not this: it has its own layout and
/// its own constructor, and is emitted like any other class.
pub(super) fn throwable_descriptor(owner: crate::types::TypeName) -> Option<&'static str> {
    [
        ("kotlin/Throwable", "kt_type_throwable"),
        ("kotlin/Error", "kt_type_error"),
        ("kotlin/Exception", "kt_type_exception"),
        ("kotlin/RuntimeException", "kt_type_runtime_exception"),
        (
            "kotlin/IllegalStateException",
            "kt_type_illegal_state_exception",
        ),
        (
            "kotlin/IllegalArgumentException",
            "kt_type_illegal_argument_exception",
        ),
        (
            "kotlin/NotImplementedError",
            "kt_type_not_implemented_error",
        ),
        ("kotlin/AssertionError", "kt_type_assertion_error"),
        (
            "kotlin/NullPointerException",
            "kt_type_null_pointer_exception",
        ),
        ("kotlin/ClassCastException", "kt_type_class_cast_exception"),
        (
            "kotlin/IndexOutOfBoundsException",
            "kt_type_index_out_of_bounds_exception",
        ),
        ("kotlin/ArithmeticException", "kt_type_arithmetic_exception"),
        (
            "kotlin/UnsupportedOperationException",
            "kt_type_unsupported_operation_exception",
        ),
        (
            "kotlin/NumberFormatException",
            "kt_type_number_format_exception",
        ),
        (
            "kotlin/NoSuchElementException",
            "kt_type_no_such_element_exception",
        ),
        (
            "kotlin/ConcurrentModificationException",
            "kt_type_concurrent_modification_exception",
        ),
        (
            "kotlin/UninitializedPropertyAccessException",
            "kt_type_uninitialized_property_access_exception",
        ),
    ]
    .into_iter()
    .find_map(|(classifier, descriptor)| {
        classifier_matches(owner, classifier).then_some(descriptor)
    })
}

/// How a `Throwable` constructor's single parameter supplies the message.
pub(super) enum ThrowableMessage {
    /// `message: String?` — the string IS the message, and a `null` stays `null`.
    Verbatim,
    /// `message: Any?` — the message is its `toString`, which for `null` is `"null"`. This is
    /// `AssertionError`'s only one-argument form, and the difference from [`Self::Verbatim`] is
    /// observable exactly there: `AssertionError(null)` reports `null` where `Exception(null)`
    /// reports no message at all.
    Rendered,
}

/// Which message a one-argument `Throwable` constructor takes, or `None` for one that is not a
/// message at all.
///
/// `Throwable`'s constructors are told apart by this one parameter. A `cause: Throwable?` is the
/// other one-argument form and answers `None`, because this `Throwable` has no cause field and
/// accepting it would silently drop what the program passed.
pub(super) fn throwable_message(ty: &Ty) -> Option<ThrowableMessage> {
    match ty {
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => throwable_message(inner),
        Ty::Obj(owner, _) if classifier_matches(*owner, "kotlin/String") => {
            Some(ThrowableMessage::Verbatim)
        }
        // A `cause`, under any name in the hierarchy.
        Ty::Obj(owner, _) if throwable_descriptor(*owner).is_some() => None,
        Ty::Obj(_, _) => Some(ThrowableMessage::Rendered),
        // `AssertionError(42)` and its siblings: Java gives each width its own overload, and every
        // one of them reports the value's text.
        _ if ty.is_jvm_scalar() => Some(ThrowableMessage::Rendered),
        _ => None,
    }
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

/// Runtime entry point for a provider-identified console intrinsic.
///
/// The declaration provider has already distinguished stdlib `print`/`println` from unrelated
/// callables with the same spelling. This function chooses only the native ABI suffix from the
/// checked parameter representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConsoleIntrinsic {
    Print,
    Println,
}

pub(super) fn console_intrinsic(operation: ConsoleIntrinsic, params: &[Ty]) -> Option<String> {
    let name = match operation {
        ConsoleIntrinsic::Print => "print",
        ConsoleIntrinsic::Println => "println",
    };
    match (operation, params) {
        (ConsoleIntrinsic::Println, []) => Some("kt_println_unit".to_string()),
        (_, [argument]) => match console_operand(*argument) {
            ConsoleOperand::Scalar(suffix) => Some(format!("kt_{name}_{suffix}")),
            ConsoleOperand::Reference => Some(format!("kt_{name}_any")),
        },
        _ => None,
    }
}

/// The runtime function realizing a selected dependency callable, or `None` when the native
/// runtime does not implement that declaration yet.
pub(super) fn runtime_function(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
) -> Option<String> {
    if owner.package_matches("kotlin") {
        return match (name, params) {
            // `TODO()`, a throw a program writes on purpose: a `Nothing`, so the caller's own
            // bottom-value contract takes over from here. `require`, `check` and `error` are
            // [`precondition`]'s, which the caller asks first; they are not named twice.
            ("TODO", []) => Some("kt_not_implemented".to_string()),
            ("TODO", [_]) => Some("kt_not_implemented_reason".to_string()),
            _ => None,
        };
    }
    if owner.package_matches("kotlin/math") {
        return match (name, params) {
            // `kotlin.math.abs`, one per width it is declared over. The operand crosses at its OWN
            // width and the answer comes back at it: `abs` of a `Long` is a `Long`, and computing it
            // at any other width would change what the minimum answers.
            ("abs", [Ty::Int]) => Some("kt_abs_int".to_string()),
            ("abs", [Ty::Long]) => Some("kt_abs_long".to_string()),
            ("abs", [Ty::Float]) => Some("kt_abs_float".to_string()),
            ("abs", [Ty::Double]) => Some("kt_abs_double".to_string()),
            _ => None,
        };
    }
    if owner.package_matches("kotlin/collections") {
        // The overflow guard `forEachIndexed` and its relatives carry. A jar provider presents
        // those as INLINE declarations, so their bodies are spliced into the caller and this call
        // comes with them; a klib provider answers the walk itself and never mentions it. Kotlin's
        // own is `throw ArithmeticException("Index overflow has happened.")`.
        return matches!((name, params), ("throwIndexOverflow", []))
            .then(|| "kt_throw_index_overflow".to_string());
    }
    None
}

/// One of `kotlin.test`'s assertions, as (runtime symbol, whether a message argument is present).
///
/// The corpus checks itself with these — they are most of what `// WITH_STDLIB` buys it — so they
/// are worth realizing directly rather than waiting for the whole of `kotlin.test` to be
/// splicable. Each compares and, when the comparison fails, raises the `AssertionError` Kotlin
/// specifies with Kotlin's own wording.
///
/// The operands cross as REFERENCES, which is why this does not go through [`runtime_function`]:
/// `assertEquals` is generic, so a call with `Int` arguments arrives typed `Int`, and the
/// comparison Kotlin makes is `==` — structural, on whatever the values are.
pub(super) fn assertion_call(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
) -> Option<(&'static str, usize)> {
    if !owner.package_matches("kotlin/test") {
        return None;
    }
    let (symbol, compared) = match name {
        "assertEquals" => ("kt_assert_equals", 2),
        // IDENTITY rather than equality, which is the whole of the difference: two strings with
        // the same text are equal and are not the same object.
        "assertSame" => ("kt_assert_same", 2),
        "assertNotSame" => ("kt_assert_not_same", 2),
        "assertTrue" => ("kt_assert_true", 1),
        "assertFalse" => ("kt_assert_false", 1),
        _ => return None,
    };
    // What the DECLARATION takes beyond the compared operands is a `message: String?`, and only
    // that. `assertEquals` also has a form whose third parameter is a floating-point tolerance;
    // answering that one as if the tolerance were a message would silently compare exactly, so it
    // falls through to the decline.
    //
    // The count returned is the COMPARED operands, not the declaration's arity: `message` is
    // defaulted, so a call that leaves it out still names a three-parameter declaration, and
    // reading the arity here would take the second operand for the message.
    match params.len() {
        n if n == compared => {}
        n if n == compared + 1 && is_string_type(&params[compared]) => {}
        _ => return None,
    }
    Some((symbol, compared))
}

/// One of `kotlin`'s preconditions: `require`, `check`, `requireNotNull`, `checkNotNull`, `error`.
///
/// Each raises a named exception with a wording Kotlin fixes, and each has a form taking a
/// `lazyMessage: () -> Any` that is called ONLY when the check fails. Kotlin declares them
/// `inline`, so a provider with the body splices them and nothing arrives here; a klib publishes
/// no body to splice, and the call reaches a backend whole.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Precondition {
    /// The runtime descriptor of the exception raised.
    pub(super) descriptor: &'static str,
    /// The message used when the call writes none — Kotlin's own wording, which programs read.
    pub(super) default_message: &'static str,
    /// What is checked: a `Boolean` that must be true, or a reference that must not be null.
    pub(super) shape: PreconditionShape,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PreconditionShape {
    /// `require(value)` / `check(value)`: a `Boolean` that must hold. Answers `Unit`.
    Holds,
    /// `requireNotNull(value)` / `checkNotNull(value)`: answers the value, now known present.
    Present,
    /// `error(message)`: no check at all, and `Nothing` as the result. The message is written
    /// rather than deferred, so it is an ordinary argument and not a block.
    Always,
}

/// Which precondition a selected top-level declaration is, or `None` for anything else.
///
/// Keyed on the package and the shape as well as the name. The lazy-message parameter is what
/// separates the two forms of each, and an overload with anything else in that position falls
/// through to the ordinary declining path rather than being answered with the wrong message.
pub(super) fn precondition(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
) -> Option<Precondition> {
    if !owner.package_matches("kotlin") {
        return None;
    }
    let (descriptor, default_message, shape) = match name {
        "require" => (
            "kt_type_illegal_argument_exception",
            "Failed requirement.",
            PreconditionShape::Holds,
        ),
        "check" => (
            "kt_type_illegal_state_exception",
            "Check failed.",
            PreconditionShape::Holds,
        ),
        "requireNotNull" => (
            "kt_type_illegal_argument_exception",
            "Required value was null.",
            PreconditionShape::Present,
        ),
        "checkNotNull" => (
            "kt_type_illegal_state_exception",
            "Required value was null.",
            PreconditionShape::Present,
        ),
        "error" => (
            "kt_type_illegal_state_exception",
            // Never reached: `error` takes its message as an ordinary argument.
            "",
            PreconditionShape::Always,
        ),
        _ => return None,
    };
    let admitted = match shape {
        // `require(Boolean)` and `require(Boolean) { … }`. The checked operand is declared
        // `Boolean`, so a call whose first parameter is anything else is another declaration.
        PreconditionShape::Holds => {
            matches!(params, [Ty::Boolean] | [Ty::Boolean, Ty::Fun(_)])
        }
        // `requireNotNull(T?)` — the operand is a type parameter, so nothing about it is worth
        // asserting here beyond its count; what matters is that the second parameter, when there
        // is one, is the block.
        PreconditionShape::Present => matches!(params, [_] | [_, Ty::Fun(_)]),
        // `error(Any)`. Its message is NOT a block, which is what keeps it apart from the others.
        PreconditionShape::Always => matches!(params, [param] if !matches!(param, Ty::Fun(_))),
    };
    admitted.then_some(Precondition {
        descriptor,
        default_message,
        shape,
    })
}

/// Is this `kotlin.String`, at either nullability and under either spelling?
fn is_string_type(ty: &Ty) -> bool {
    match ty {
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => is_string_type(inner),
        Ty::Obj(owner, _) => classifier_matches(*owner, "kotlin/String"),
        _ => false,
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
pub(super) fn float_predicate(owner: DeclarationOwner, name: &str) -> Option<FloatPredicate> {
    // Kotlin declares each of these TWICE: as a member of the primitive (`Double.isNaN()`) and as
    // an extension on it in the numbers facade. Which spelling reaches a backend is the provider's
    // choice, not the program's, so both are read here.
    //
    // Only the facade one was, and the package helper used to answer `None` for an owner not ending
    // in `Kt` — so an owner of `kotlin/Double` fell through and the call declined by name. The
    // member spelling is the one the corpus actually produces.
    let declared_here = owner.classifier_matches("kotlin/Double")
        || owner.classifier_matches("kotlin/Float")
        || owner.package_matches("kotlin");
    if !declared_here {
        return None;
    }
    match name {
        "isNaN" => Some(FloatPredicate::IsNaN),
        "isInfinite" => Some(FloatPredicate::IsInfinite),
        "isFinite" => Some(FloatPredicate::IsFinite),
        _ => None,
    }
}

/// `Float.fromBits(n)` / `Double.fromBits(n)`, as (runtime symbol, operand, answer).
///
/// An EXTENSION of the companion object, declared in `kotlin`, so the receiver is that object and
/// nothing reads it — which is why this is not [`scalar_member`]: that table evaluates a receiver
/// and crosses it as a reference, and there is no object here to make. The operand's width is what
/// says which of the two this is.
pub(super) fn bits_to_float(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
) -> Option<(&'static str, Ty, Ty)> {
    if !owner.package_matches("kotlin") || name != "fromBits" {
        return None;
    }
    match params {
        [Ty::Int] => Some(("kt_float_from_bits", Ty::Int, Ty::Float)),
        [Ty::Long] => Some(("kt_double_from_bits", Ty::Long, Ty::Double)),
        _ => None,
    }
}

/// `x.toBits()` / `x.toRawBits()`, as (runtime symbol, answer), for a receiver of `receiver`.
///
/// Also an extension declared in `kotlin`, and its RECEIVER is a machine value — so, like
/// [`floor_mod`], it takes that receiver at its own width rather than through a box. The receiver's
/// type is what says which width, because the declaration takes no parameter to read it from.
pub(super) fn float_to_bits(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
    receiver: Ty,
) -> Option<(&'static str, Ty)> {
    if !owner.package_matches("kotlin") || !params.is_empty() {
        return None;
    }
    match (name, receiver.non_null()) {
        ("toRawBits", Ty::Float) => Some(("kt_float_to_raw_bits", Ty::Int)),
        ("toRawBits", Ty::Double) => Some(("kt_double_to_raw_bits", Ty::Long)),
        ("toBits", Ty::Float) => Some(("kt_float_to_bits", Ty::Int)),
        ("toBits", Ty::Double) => Some(("kt_double_to_bits", Ty::Long)),
        _ => None,
    }
}

/// Whether this names `kotlin.Comparable.compareTo` — the ONE member of that type, asked of a
/// receiver the static type says nothing more about than `Comparable`.
///
/// The answer is the runtime's, read from the receiver's DESCRIPTOR. Whether the runtime may give
/// it is the CALLER's question, not this one's: an object of the program's could stand behind that
/// type too, and only the file knows whether it declares one.
pub(super) fn is_comparable_compare_to(owner: DeclarationOwner, name: &str, params: &[Ty]) -> bool {
    owner.classifier_matches("kotlin/Comparable") && name == "compareTo" && params.len() == 1
}

/// Whether this names `kotlin.CharSequence`, under either spelling a provider may hand over.
///
/// It wears a runtime descriptor for the reason `Number` and `Comparable` do: no instances of its
/// own, and both the string and the builder point at it, so a cast or an `is` against it has
/// something to compare.
pub(super) fn is_char_sequence(internal: crate::types::TypeName) -> bool {
    classifier_matches(internal, "kotlin/CharSequence")
}

/// The string builder the runtime provides, if this names one.
///
/// Like [`is_array_list`], `kotlin.text.StringBuilder` is declared in no file krusty compiles, so
/// constructing one is the runtime's job rather than the generator's.
pub(super) fn is_string_builder(internal: crate::types::TypeName) -> bool {
    classifier_matches(internal, "kotlin/text/StringBuilder")
}

/// A dependency member the runtime answers with its arguments carried as VALUES, and the signature
/// it is called with: `(receiver and parameters, result)`.
///
/// The ordinary member path crosses everything as a reference, which is right for a member that
/// asks about an object and wrong for one that asks about a NUMBER — `s[i]` would box the index to
/// pass it and the runtime would read the box as the index.
pub(super) fn scalar_member(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    let reference = Ty::nullable(Ty::obj("kotlin/Any"));
    // `kotlin.text`'s top-level extensions on `String`, which reach a backend as members of that
    // package's file facade — the `kotlin/io/ConsoleKt` situation, read through the same helper so
    // a facade kotlinc split in two (`StringsKt__StringsKt`) is the same answer.
    if owner.package_matches("kotlin/text") {
        return match (name, params) {
            ("substring", [Ty::Int, Ty::Int]) => Some((
                "kt_string_substring",
                vec![reference, Ty::Int, Ty::Int],
                Ty::obj("kotlin/String"),
            )),
            ("substring", [Ty::Int]) => Some((
                "kt_string_substring_from",
                vec![reference, Ty::Int],
                Ty::obj("kotlin/String"),
            )),
            // Questions about the text that answer a machine value rather than an object. Each is
            // the runtime's because the text is UTF-8 and the answer is about what Kotlin counts:
            // `isEmpty` is about bytes, `first`/`last` about UTF-16 units, and a program that
            // asked either in Kotlin source would walk the encoding to find out.
            ("isEmpty", []) => Some(("kt_string_is_empty", vec![reference], Ty::Boolean)),
            ("isNotEmpty", []) => Some(("kt_string_is_not_empty", vec![reference], Ty::Boolean)),
            ("isBlank", []) => Some(("kt_string_is_blank", vec![reference], Ty::Boolean)),
            ("isNotBlank", []) => Some(("kt_string_is_not_blank", vec![reference], Ty::Boolean)),
            ("first", []) => Some(("kt_string_first", vec![reference], Ty::Char)),
            ("last", []) => Some(("kt_string_last", vec![reference], Ty::Char)),
            ("repeat", [Ty::Int]) => Some((
                "kt_string_repeat",
                vec![reference, Ty::Int],
                Ty::obj("kotlin/String"),
            )),
            _ => None,
        };
    }
    let text_owner = owner.classifier_matches("kotlin/String")
        || owner.classifier_matches("kotlin/CharSequence")
        || owner.classifier_matches("kotlin/text/StringBuilder");
    if text_owner {
        match (name, params) {
            // A `CharSequence`/builder read. `String.get` does not reach this name table: its exact
            // compiler-intrinsic identity selects the same runtime operation in lowering.
            ("get" | "charAt", [Ty::Int]) if !owner.classifier_matches("kotlin/String") => {
                return Some(("kt_string_get", vec![reference, Ty::Int], Ty::Char));
            }
            // `sb.setLength(n)` counts UTF-16 units, so the operand is an `Int` the generator must not
            // box to hand over. It answers nothing, which is why it is not one of the builder's
            // reference-carried members below.
            ("setLength", [Ty::Int]) if owner.classifier_matches("kotlin/text/StringBuilder") => {
                return Some((
                    "kt_string_builder_set_length",
                    vec![reference, Ty::Int],
                    Ty::Unit,
                ));
            }
            // `s.subSequence(a, b)` is `s.substring(a, b)`; the return type only says less about the
            // result, which the call site already knows.
            ("subSequence", [Ty::Int, Ty::Int])
                if owner.classifier_matches("kotlin/String")
                    || owner.classifier_matches("kotlin/CharSequence") =>
            {
                return Some((
                    "kt_string_substring",
                    vec![reference, Ty::Int, Ty::Int],
                    Ty::obj("kotlin/String"),
                ));
            }
            // `isEmpty` and its three relatives are INLINE extensions in `kotlin.text`, so a jar
            // provider presents them as members of the receiver's own type rather than of the text
            // facade — the same declaration under a second spelling, exactly as `charAt` is `get`.
            ("isEmpty", []) => {
                return Some(("kt_string_is_empty", vec![reference], Ty::Boolean));
            }
            ("isNotEmpty", []) => {
                return Some(("kt_string_is_not_empty", vec![reference], Ty::Boolean));
            }
            ("isBlank", []) => {
                return Some(("kt_string_is_blank", vec![reference], Ty::Boolean));
            }
            ("isNotBlank", []) => {
                return Some(("kt_string_is_not_blank", vec![reference], Ty::Boolean));
            }
            // The ANSWER is an `Int`, so this cannot go through the reference-carried member path
            // below: `a < b` would box the very comparison it is asking about.
            ("compareTo", [_]) if owner.classifier_matches("kotlin/String") => {
                return Some(("kt_string_compare_to", vec![reference, reference], Ty::Int));
            }
            _ => {}
        }
    }
    // `kotlin.Number`'s six conversions. A site that could type its value only as a `Number`
    // hands over a box, and which primitive is inside is the descriptor's answer — so the
    // runtime reads it rather than the generator guessing from the static type.
    //
    // Each arrives under either provider spelling, exactly as
    // `kotlin.CharSequence.get`/`java.lang.CharSequence.charAt` does: a mapped builtin whose
    // realization names a different physical member hands over that physical name. Observed:
    // `toByte`/`toShort` come through as `byteValue`/`shortValue` while `toInt`/`toLong` keep
    // the Kotlin spelling, so neither list is the one to write alone. The two spellings are the
    // SAME member, not one replacing the other.
    if owner.classifier_matches("kotlin/Number") {
        return match (name, params) {
            ("toByte" | "byteValue", []) => Some(("kt_number_to_byte", vec![reference], Ty::Byte)),
            ("toShort" | "shortValue", []) => {
                Some(("kt_number_to_short", vec![reference], Ty::Short))
            }
            ("toInt" | "intValue", []) => Some(("kt_number_to_int", vec![reference], Ty::Int)),
            ("toLong" | "longValue", []) => Some(("kt_number_to_long", vec![reference], Ty::Long)),
            ("toFloat" | "floatValue", []) => {
                Some(("kt_number_to_float", vec![reference], Ty::Float))
            }
            ("toDouble" | "doubleValue", []) => {
                Some(("kt_number_to_double", vec![reference], Ty::Double))
            }
            _ => None,
        };
    }
    None
}

/// A `kotlin.text` member whose LAST parameter is Kotlin's `ignoreCase`, with the runtime entry
/// point that answers the case-SENSITIVE form.
///
/// These cannot go through [`scalar_member`]: that table carries every parameter, and the
/// `ignoreCase` one is not a value the runtime is given — it is a question the runtime does not
/// answer. Only the caller can see whether it was asked, so the caller selects the entry point and
/// drops the argument, and declines when the flag is anything but a literal `false`. Case folding
/// is a question about Unicode rather than about text, and the runtime holds no case table.
///
/// The `Char` overload of `contains` is deliberately absent: its operand is a machine value, not a
/// reference, and these entry points take text on both sides.
pub(super) fn case_sensitive_text_member(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
) -> Option<&'static str> {
    if !owner.package_matches("kotlin/text")
        && !owner.classifier_matches("kotlin/String")
        && !owner.classifier_matches("kotlin/CharSequence")
    {
        return None;
    }
    // Either shape of the declaration: with the `ignoreCase` parameter, as the `kotlin.text`
    // extension declares it, or without, as `java.lang.String`'s own member has it. The CALLER
    // decides whether the flag was asked for, by the arguments it holds.
    let text = match params {
        [text] | [text, Ty::Boolean] => text,
        _ => return None,
    };
    if !matches!(*text, Ty::Obj(named, _) if is_char_sequence(named) || classifier_matches(named, "kotlin/String"))
    {
        return None;
    }
    match name {
        "startsWith" => Some("kt_string_starts_with"),
        "endsWith" => Some("kt_string_ends_with"),
        "contains" => Some("kt_string_contains"),
        _ => None,
    }
}

/// `x++` on a primitive, as the type it steps and the step itself.
///
/// `var i: Int? = 10; i++` selects `Int.inc()`, and what is in the box is the very primitive the
/// member was selected on. So this needs no descriptor read, unlike [`scalar_member`]'s `Number`
/// conversions: the OWNER already says which type steps.
///
/// Two spellings arrive, because two providers name the same declaration differently. A JVM
/// provider presents `Int.inc()` as a member of the box class it is realized on; a klib provider
/// has no box class to name and presents the Kotlin classifier. Neither spelling changes what the
/// operation is, and the lowering takes a receiver that is already a scalar without a round trip
/// through a box — so admitting both is the whole of the difference.
pub(super) fn boxed_step(owner: DeclarationOwner, name: &str, params: &[Ty]) -> Option<(Ty, i64)> {
    if !params.is_empty() {
        return None;
    }
    let step = match name {
        "inc" => 1,
        "dec" => -1,
        _ => return None,
    };
    let ty = [
        ("kotlin/Byte", Ty::Byte),
        ("kotlin/Short", Ty::Short),
        ("kotlin/Int", Ty::Int),
        ("kotlin/Long", Ty::Long),
        ("kotlin/Char", Ty::Char),
        ("kotlin/Float", Ty::Float),
        ("kotlin/Double", Ty::Double),
    ]
    .into_iter()
    .find_map(|(classifier, ty)| owner.classifier_matches(classifier).then_some(ty))?;
    Some((ty, step))
}

/// The unsigned integer a member is declared on, for an owner classifier that is one.
///
/// The classifier's IDENTITY decides, through the same canonical semantic type every backend reads
/// — never the spelling a provider realizes the member under. What the caller gets back is the
/// TYPE, which is what decides how wide the operands are and which questions are asked unsigned.
pub(super) fn unsigned_owner(owner: crate::types::TypeName) -> Option<Ty> {
    Some(Ty::obj_name(owner).canonical_semantic()).filter(|ty| ty.is_unsigned())
}

/// `a.mod(b)` — the remainder carrying the DIVISOR's sign, as (runtime symbol, operand type): both
/// operands are read at the operand type and the answer is that type.
///
/// Kotlin computes every pair at the WIDER of the two widths. When that is also the declared
/// result — `Int.mod(Long)` answers a `Long`, `Float.mod(Double)` a `Double` — the pair is this
/// table's. When the receiver is the wider one it is not: `Long.mod(Int)` is computed at `Long` and
/// narrowed to the `Int` it declares, and `Double.mod(Float)` is computed at `Double`, so reading the
/// receiver at the divisor's width would drop its high bits (`(1L shl 33).mod(3)` is 2, not 0).
/// Those decline rather than answer wrongly. The narrow integers are computed at `Int`, which is
/// exact: the answer's magnitude is below the divisor's, so nothing is lost on the way back down.
///
/// Not [`scalar_member`]: that table hands its receiver over as a reference, and the receiver here
/// is a number.
pub(super) fn floor_mod(
    owner: DeclarationOwner,
    name: &str,
    receiver: Ty,
    params: &[Ty],
) -> Option<(&'static str, Ty)> {
    if !owner.package_matches("kotlin") || name != "mod" {
        return None;
    }
    // Each numeric width, ranked; `Char` has no `mod` and a reference names something else.
    let rank = |ty: Ty| match ty {
        Ty::Byte | Ty::Short | Ty::Int => Some(0),
        Ty::Long => Some(1),
        Ty::Float => Some(2),
        Ty::Double => Some(3),
        _ => None,
    };
    let [divisor] = params else {
        return None;
    };
    let (receiver_rank, divisor_rank) = (rank(receiver)?, rank(*divisor)?);
    let integral = |rank: u8| rank < 2;
    if integral(receiver_rank) != integral(divisor_rank) || receiver_rank > divisor_rank {
        return None;
    }
    match *divisor {
        Ty::Byte | Ty::Short | Ty::Int => Some(("kt_mod_int", Ty::Int)),
        Ty::Long => Some(("kt_mod_long", Ty::Long)),
        Ty::Float => Some(("kt_mod_float", Ty::Float)),
        Ty::Double => Some(("kt_mod_double", Ty::Double)),
        _ => None,
    }
}

/// A bit operation `kotlin.experimental` declares for the NARROW integers.
///
/// Kotlin gives `Int` and `Long` `and`/`or`/`xor`/`inv` as members and gives `Byte` and `Short`
/// the same four as extensions in this package. That is a library arrangement, not a difference in
/// the operation: the answer is the machine's, at the receiver's own width. So these are realized
/// as instructions rather than as a call, which is also why they are not in [`scalar_member`] —
/// that table hands its receiver over as a reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BitwiseOp {
    And,
    Or,
    Xor,
    Inv,
}

/// Which of the four a selected declaration is, or `None` for anything else.
///
/// The package is the gate. `kotlin.experimental` also publishes annotations and the opt-in
/// markers, none of which is a call, and the four names are common enough that keying on the name
/// alone would claim a member of some other type.
pub(super) fn experimental_bitwise(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
) -> Option<BitwiseOp> {
    if !owner.package_matches("kotlin/experimental") {
        return None;
    }
    // The OPERAND is the receiver's own type, and a binary form takes it again: Kotlin declares no
    // mixed-width overload here, so `Byte.and(Short)` does not exist and an argument of another
    // width is a declaration this does not answer.
    match (name, params) {
        ("and", [Ty::Byte | Ty::Short]) => Some(BitwiseOp::And),
        ("or", [Ty::Byte | Ty::Short]) => Some(BitwiseOp::Or),
        ("xor", [Ty::Byte | Ty::Short]) => Some(BitwiseOp::Xor),
        ("inv", []) => Some(BitwiseOp::Inv),
        _ => None,
    }
}

/// The unsigned integer a signed-to-unsigned conversion answers, for the facade extension naming
/// one: `42.toUInt()`, `(-1).toUByte()`.
///
/// These are NOT members of an unsigned type — the receiver is SIGNED, so they live on the facade
/// beside it rather than on the value class, and [`unsigned_owner`] does not see them.
///
/// Kotlin defines each as the ordinary signed conversion to the target's width followed by
/// reinterpreting those bits: `Int.toUByte()` is `UByte(this.toByte())`. So the answer here is only
/// the TARGET type, and the caller converts the receiver to it from the receiver's own type, whose
/// signedness is what decides between extending and truncating. Measured against kotlinc, which is
/// what establishes that the rule holds where it is least obvious: `(200.toByte()).toUInt()` is
/// 4294967240 (the source's sign extends) and `300.toUByte()` is 44 (the target's width truncates).
///
/// A FLOAT source is deliberately absent. `Double.toUInt()` is not the signed conversion
/// reinterpreted — it saturates at zero for a negative, where the signed rule would answer a huge
/// positive — so it belongs to its own change rather than to this rule.
pub(super) fn unsigned_conversion(owner: DeclarationOwner, name: &str) -> Option<Ty> {
    // A top-level extension of `kotlin`, under either provider's spelling: a JVM provider names
    // the file facade kotlinc split them across, a klib names the package. The declarations these
    // four names can denote in `kotlin` are exactly these, so the package is discrimination
    // enough; `UInt.toUInt()` is a MEMBER of its own type and answers `kotlin/UInt`, which is not
    // this package and is handled by `unsigned_owner`.
    if !owner.package_matches("kotlin") {
        return None;
    }
    Some(match name {
        "toUByte" => Ty::UByte,
        "toUShort" => Ty::UShort,
        "toUInt" => Ty::UInt,
        "toULong" => Ty::ULong,
        _ => return None,
    })
}

/// Whether an accessor is a non-`String` TEXT `length` — `CharSequence`'s or a builder's.
///
/// One question about the same thing, and the runtime answers all three from one place (see
/// `kt_text_of`) — including for text the PROGRAM declared, whose own `length` the descriptor
/// records. The selected `String.length` declaration carries `CompilerIntrinsic::StringLength`
/// and deliberately does not enter this spelling-based migration path.
pub(super) fn is_non_string_text_length(owner: crate::types::TypeName, name: &str) -> bool {
    // The provider presents it under the Kotlin name of the property, not the JVM accessor's:
    // `length`, where `kotlin.Enum`'s two arrive as `getName`/`getOrdinal`. Both spellings are
    // taken because which one a provider uses is the provider's business, not this table's.
    name == "length"
        && (classifier_matches(owner, "kotlin/CharSequence")
            || classifier_matches(owner, "kotlin/text/StringBuilder"))
}

/// The runtime reader for one of `Throwable`'s two fields, or `None` for any other accessor.
///
/// Both are read by the runtime rather than by an offset here, because the class is the runtime's
/// and so is its layout. A SUBCLASS of it declared in this file is read the same way: its storage
/// begins with the base's, which is exactly what makes one reader answer for both.
pub(super) fn throwable_field(owner: crate::types::TypeName, name: &str) -> Option<&'static str> {
    if !classifier_matches(owner, "kotlin/Throwable") {
        return None;
    }
    match name {
        "message" => Some("kt_throwable_message"),
        "cause" => Some("kt_throwable_cause"),
        _ => None,
    }
}

/// A member of a COMPANION the runtime realizes, whose receiver carries nothing.
///
/// Such an object has no state and no instance in any file krusty compiles, so there is no
/// receiver to pass and none to evaluate — the call is its arguments alone. That is why these are
/// apart from [`runtime_member`], which leads every call with the receiver.
pub(super) fn runtime_companion_member(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
) -> Option<&'static str> {
    if !owner.classifier_matches("kotlin/properties/Delegates") {
        return None;
    }
    match (name, params) {
        // `Delegates.notNull()`. `Delegates` is an OBJECT of the stdlib, carrying nothing, and the
        // delegate it answers with starts empty — so this is the same shape: no receiver to read
        // and no operand to pass.
        ("notNull", []) => Some("kt_not_null_var"),
        // `observable(initial) { property, old, new -> … }`: the initial value and the callback,
        // both references, and the `KProperty` it later hands that callback is the one the
        // delegation passes to `setValue` — nothing here reads it.
        ("observable", [_, _]) => Some("kt_observable"),
        _ => None,
    }
}

/// The runtime function realizing a selected dependency MEMBER, called with the receiver as its
/// first argument. Receiver and arguments are passed as references, so a scalar receiver boxes —
/// which is what `4.toString()` means anyway.
pub(super) fn runtime_member(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
    selected_role: Option<RuntimeMemberRole>,
) -> Option<&'static str> {
    if let Some(role) = selected_role {
        return match (role, params) {
            (RuntimeMemberRole::ToString, []) => Some("kt_to_string"),
            (RuntimeMemberRole::HashCode, []) => Some("kt_hash_code"),
            _ => None,
        };
    }
    // `removeSuffix` is a top-level extension of `kotlin.text`, so it arrives as a member of that
    // package's file facade; everything it takes and answers is a reference, which is this path.
    if owner.package_matches("kotlin/text") {
        match (name, params) {
            ("removeSuffix", [_]) => return Some("kt_string_remove_suffix"),
            // Text in, text out: every operand and the answer are references, so the ordinary
            // reference path carries all of them.
            ("trim", []) => return Some("kt_string_trim"),
            ("trimStart", []) => return Some("kt_string_trim_start"),
            ("trimEnd", []) => return Some("kt_string_trim_end"),
            ("reversed", []) => return Some("kt_string_reversed"),
            // `appendLine` is Kotlin's own, declared beside the builder rather than on it, so it
            // arrives as a member of the facade with the builder as its receiver.
            ("appendLine", [_]) => return Some("kt_string_builder_append_line"),
            ("appendLine", []) => return Some("kt_string_builder_append_new_line"),
            // Every `append` the facade declares is `vararg`, so its one slot is an ARRAY of
            // operands; the builder's own one-operand `append` is a member of the builder and is
            // answered below. Binding the facade's to the one-operand function would render the
            // array itself.
            _ => {}
        }
    }
    if owner.classifier_matches("kotlin/text/StringBuilder") {
        return match (name, params) {
            // A builder's `append` takes one of a dozen overloads on the JVM and one function here:
            // every operand is rendered through its own `toString`, which is the same answer for all of
            // them, and the reference path has already boxed whichever primitive arrived.
            ("append", [_]) => Some("kt_string_builder_append"),
            ("appendLine", [_]) => Some("kt_string_builder_append_line"),
            ("appendLine", []) => Some("kt_string_builder_append_new_line"),
            _ => None,
        };
    }
    match (name, params) {
        // `equals` does not yet carry a declaration role. Keep this remaining runtime ABI bridge
        // narrow until the provider publishes the same exact identity it already does for
        // `hashCode` and `toString`.
        ("equals", [_]) => Some("kt_equals"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RuntimeMemberRole {
    ToString,
    HashCode,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(path: &str) -> DeclarationOwner {
        DeclarationOwner::classifier(crate::types::type_name(path))
    }

    fn top_level(path: &str) -> DeclarationOwner {
        DeclarationOwner {
            physical: crate::types::type_name(path),
            top_level: true,
        }
    }

    /// Both providers' spellings of the same top-level declaration name the same package.
    #[test]
    fn a_top_level_declaration_names_its_package_under_either_spelling() {
        // The JVM provider's: the file facade a top-level function was compiled into.
        assert!(top_level("kotlin/io/ConsoleKt").package_matches("kotlin/io"));
        assert!(top_level("kotlin/text/StringsKt").package_matches("kotlin/text"));
        // The klib provider's: the package itself, because a klib has no facades.
        assert!(top_level("kotlin/io").package_matches("kotlin/io"));
        assert!(top_level("kotlin/collections").package_matches("kotlin/collections"));
        assert!(top_level("kotlin").package_matches("kotlin"));
    }

    /// `x++` names the same operation whichever provider selected the declaration.
    ///
    /// A JVM provider realizes `Int.inc()` on the box class; a klib provider has no box class and
    /// names the Kotlin classifier. Nine corpus cases reached the generator under the second
    /// spelling and were declined as an unknown member.
    #[test]
    fn a_step_is_the_same_operation_under_either_providers_spelling() {
        for (jvm, kotlin, stepped) in [
            ("java/lang/Byte", "kotlin/Byte", Ty::Byte),
            ("java/lang/Short", "kotlin/Short", Ty::Short),
            ("java/lang/Integer", "kotlin/Int", Ty::Int),
            ("java/lang/Long", "kotlin/Long", Ty::Long),
            ("java/lang/Character", "kotlin/Char", Ty::Char),
            ("java/lang/Float", "kotlin/Float", Ty::Float),
            ("java/lang/Double", "kotlin/Double", Ty::Double),
        ] {
            assert_eq!(boxed_step(member(jvm), "inc", &[]), Some((stepped, 1)));
            assert_eq!(boxed_step(member(kotlin), "inc", &[]), Some((stepped, 1)));
            assert_eq!(boxed_step(member(jvm), "dec", &[]), Some((stepped, -1)));
            assert_eq!(boxed_step(member(kotlin), "dec", &[]), Some((stepped, -1)));
        }
        // What the widened table must NOT admit. `Boolean` has no step at all, an argument means
        // the member is something else entirely, and a classifier that merely lives in `kotlin`
        // is not a primitive.
        assert_eq!(boxed_step(member("kotlin/Boolean"), "inc", &[]), None);
        assert_eq!(boxed_step(member("kotlin/String"), "inc", &[]), None);
        assert_eq!(boxed_step(member("kotlin/Int"), "inc", &[Ty::Int]), None);
        assert_eq!(boxed_step(member("kotlin/Int"), "plus", &[]), None);
    }

    /// A class comes back unchanged, so no comparison against a package can match it. This is what
    /// keeps a member of a real class from being read as a top-level declaration of the package
    /// that class lives in — `kotlin/collections/AbstractMutableList` must not answer
    /// `kotlin/collections` merely because it is declared there.
    #[test]
    fn a_class_is_never_mistaken_for_a_package() {
        assert!(!member("kotlin/text/Regex").package_matches("kotlin/text"));
        assert!(
            !member("kotlin/collections/AbstractMutableList").package_matches("kotlin/collections")
        );
        assert!(!member("Ungrouped").package_matches(""));
    }

    #[test]
    fn console_overloads_select_by_parameter_representation() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        assert_eq!(
            console_intrinsic(ConsoleIntrinsic::Println, &[any]).as_deref(),
            Some("kt_println_any"),
            "a reference argument takes the Any? overload, as it does in Kotlin"
        );
        assert_eq!(
            console_intrinsic(ConsoleIntrinsic::Println, &[Ty::Int]).as_deref(),
            Some("kt_println_int"),
            "a scalar argument must not be boxed to reach the console"
        );
        assert_eq!(
            console_intrinsic(ConsoleIntrinsic::Print, &[Ty::Boolean]).as_deref(),
            Some("kt_print_boolean")
        );
        assert_eq!(
            console_intrinsic(ConsoleIntrinsic::Println, &[]).as_deref(),
            Some("kt_println_unit")
        );
    }

    #[test]
    fn an_unsigned_member_is_recognized_by_its_value_class() {
        let owner = crate::types::type_name;
        assert_eq!(unsigned_owner(owner("kotlin/UInt")), Some(Ty::UInt));
        assert_eq!(unsigned_owner(owner("kotlin/ULong")), Some(Ty::ULong));
        assert_eq!(unsigned_owner(owner("kotlin/Int")), None);
    }

    #[test]
    fn string_plus_has_no_name_based_runtime_fallback() {
        assert_eq!(
            runtime_member(member("java/lang/String"), "plus", &[Ty::String], None),
            None,
            "String.plus is admitted only by its CompilerIntrinsic identity"
        );
    }

    #[test]
    fn string_get_has_no_name_based_runtime_fallback() {
        for owner in ["kotlin/String", "java/lang/String"] {
            assert_eq!(
                scalar_member(member(owner), "get", &[Ty::Int]),
                None,
                "String.get from {owner} is admitted only by its CompilerIntrinsic identity"
            );
        }
        assert_eq!(
            scalar_member(member("kotlin/CharSequence"), "get", &[Ty::Int]),
            Some((
                "kt_string_get",
                vec![Ty::nullable(Ty::obj("kotlin/Any")), Ty::Int],
                Ty::Char,
            )),
            "the distinct CharSequence declaration remains explicit migration debt"
        );
    }

    #[test]
    fn string_length_has_no_name_based_property_fallback() {
        for owner in ["kotlin/String", "java/lang/String"] {
            assert!(
                !is_non_string_text_length(crate::types::type_name(owner), "length"),
                "String.length from {owner} is admitted only by its CompilerIntrinsic identity"
            );
        }
        assert!(is_non_string_text_length(
            crate::types::type_name("kotlin/CharSequence"),
            "length"
        ));
    }

    #[test]
    fn semantic_roles_admit_any_members_without_name_fallbacks() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        assert_eq!(
            runtime_member(member("kotlin/String"), "plus", &[any], None),
            None
        );
        assert_eq!(
            runtime_member(
                member("kotlin/Int"),
                "physicalNameDoesNotMatter",
                &[],
                Some(RuntimeMemberRole::ToString)
            ),
            Some("kt_to_string")
        );
        assert_eq!(
            runtime_member(
                member("kotlin/Int"),
                "physicalNameDoesNotMatter",
                &[],
                Some(RuntimeMemberRole::ToString),
            ),
            Some("kt_to_string")
        );
        assert_eq!(
            runtime_member(
                member("kotlin/Any"),
                "physicalNameDoesNotMatter",
                &[],
                Some(RuntimeMemberRole::HashCode)
            ),
            Some("kt_hash_code")
        );
        assert_eq!(
            runtime_member(member("kotlin/Any"), "hashCode", &[], None),
            None,
            "a coincidental spelling has no semantic role"
        );
        assert_eq!(
            runtime_member(member("kotlin/Any"), "equals", &[any], None),
            Some("kt_equals")
        );
        assert_eq!(
            runtime_member(member("kotlin/String"), "repeat", &[Ty::Int], None),
            None,
            "an unimplemented member must decline"
        );
    }

    #[test]
    fn a_floating_point_value_prints_through_its_own_overload() {
        // Kotlin's overload set has one per primitive, and so does the runtime: the value reaches
        // it unboxed, and `krusty_fp.c` decides what it looks like.
        assert_eq!(
            console_intrinsic(ConsoleIntrinsic::Println, &[Ty::Double]).as_deref(),
            Some("kt_println_double")
        );
        assert_eq!(
            console_intrinsic(ConsoleIntrinsic::Print, &[Ty::Float]).as_deref(),
            Some("kt_print_float")
        );
    }

    #[test]
    fn an_unimplemented_declaration_is_declined_rather_than_guessed() {
        assert_eq!(
            runtime_function(
                top_level("kotlin/text/StringsKt"),
                "repeat",
                &[Ty::String, Ty::Int],
            ),
            None,
            "a missing runtime function must produce a diagnostic, never a call to a symbol that \
             does not exist"
        );
        assert_eq!(
            runtime_function(top_level("kotlin/io/ConsoleKt"), "readLine", &[]),
            None
        );
    }

    #[test]
    fn a_mod_is_answered_only_where_its_operand_width_is_its_result() {
        // Computed at the wider width, which is also the declared result.
        assert_eq!(
            floor_mod(top_level("kotlin/NumbersKt"), "mod", Ty::Int, &[Ty::Long]),
            Some(("kt_mod_long", Ty::Long))
        );
        assert_eq!(
            floor_mod(
                top_level("kotlin/NumbersKt"),
                "mod",
                Ty::Float,
                &[Ty::Double],
            ),
            Some(("kt_mod_double", Ty::Double))
        );
        assert_eq!(
            floor_mod(top_level("kotlin/NumbersKt"), "mod", Ty::Byte, &[Ty::Byte]),
            Some(("kt_mod_int", Ty::Int))
        );
        // Computed at the receiver's wider width and narrowed after: reading the receiver at the
        // divisor's width would drop its high bits, so these decline.
        assert_eq!(
            floor_mod(top_level("kotlin/NumbersKt"), "mod", Ty::Long, &[Ty::Int]),
            None
        );
        assert_eq!(
            floor_mod(
                top_level("kotlin/NumbersKt"),
                "mod",
                Ty::Double,
                &[Ty::Float],
            ),
            None
        );
        // An integer and a floating-point operand meet in neither.
        assert_eq!(
            floor_mod(top_level("kotlin/NumbersKt"), "mod", Ty::Int, &[Ty::Double]),
            None
        );
    }

    #[test]
    fn a_facade_append_is_vararg_and_only_the_builders_own_append_is_answered() {
        assert_eq!(
            runtime_member(
                top_level("kotlin/text/StringsKt"),
                "append",
                &[Ty::array(Ty::String)],
                None,
            ),
            None
        );
        assert_eq!(
            runtime_member(
                member("kotlin/text/StringBuilder"),
                "append",
                &[Ty::String],
                None,
            ),
            Some("kt_string_builder_append")
        );
    }

    #[test]
    fn a_precondition_is_named_by_one_table_only() {
        for name in ["require", "check"] {
            assert_eq!(
                runtime_function(top_level("kotlin/PreconditionsKt"), name, &[Ty::Boolean]),
                None
            );
            assert!(
                precondition(top_level("kotlin/PreconditionsKt"), name, &[Ty::Boolean]).is_some()
            );
        }
    }
}
