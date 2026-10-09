//! Mapping already-selected Kotlin stdlib declarations onto native runtime functions.
//!
//! The frontend has already picked an overload; nothing here is resolution. A declaration arrives
//! as its semantic owner, name, receiver, parameter types, and result, and the question is only
//! which C function realizes that complete declaration signature.
//!
//! A JVM provider may physically own a top-level declaration in a file facade while a klib
//! provider owns it in the package. The provider's callable kind records that it is top-level;
//! the backend never infers that fact from a rendered owner or a `Kt` suffix.

use crate::types::{SemanticCallableOwner, Ty, TypeName};

/// Exact selected declaration owner as the native runtime tables see it.
///
/// `physical` remains the provider-interned emission identity. `semantic` is the exact source
/// namespace frozen from the provider; a JVM facade and a KLIB package can therefore share one
/// comparison, while a mapped JVM class cannot erase the Kotlin classifier that declared a member.
#[derive(Clone, Copy)]
pub(super) struct DeclarationOwner {
    physical: TypeName,
    semantic: Option<SemanticCallableOwner>,
}

impl DeclarationOwner {
    pub(super) fn callable(physical: TypeName, semantic: Option<SemanticCallableOwner>) -> Self {
        Self { physical, semantic }
    }

    pub(super) fn physical(self) -> TypeName {
        self.physical
    }

    /// Render the already-recorded semantic namespace for an unsupported-call diagnostic. This is
    /// a boundary conversion only; dispatch continues to compare interned identities above.
    pub(super) fn diagnostic_name(self) -> String {
        match self.semantic {
            Some(SemanticCallableOwner::Package(owner)) => {
                format!("package {}", owner.render().replace('/', "."))
            }
            Some(SemanticCallableOwner::Classifier(owner)) => {
                format!("classifier {}", owner.render().replace('/', "."))
            }
            None => "unknown semantic owner".to_string(),
        }
    }

    pub(super) fn semantic_classifier(self) -> Option<TypeName> {
        match self.semantic {
            Some(SemanticCallableOwner::Classifier(owner)) => Some(owner),
            Some(SemanticCallableOwner::Package(_)) | None => None,
        }
    }

    #[cfg(test)]
    fn classifier(physical: TypeName) -> Self {
        Self {
            physical,
            semantic: Some(SemanticCallableOwner::Classifier(physical)),
        }
    }

    pub(super) fn package_matches(self, package: &str) -> bool {
        matches!(
            self.semantic,
            Some(SemanticCallableOwner::Package(owner)) if owner.matches(package)
        )
    }

    fn package_is(self, package: TypeName) -> bool {
        self.semantic == Some(SemanticCallableOwner::Package(package))
    }

    fn classifier_matches(self, kotlin: &str) -> bool {
        match self.semantic {
            Some(SemanticCallableOwner::Classifier(owner)) => classifier_matches(owner, kotlin),
            Some(SemanticCallableOwner::Package(_)) => false,
            None => classifier_matches(self.physical, kotlin),
        }
    }
}

/// Complete semantic identity of one selected function declaration.
///
/// Runtime tables match this contract rather than treating owner, spelling, and arity as a
/// declaration identity. Physical descriptors remain a backend representation and are not parsed
/// to recover semantic parameter or result types.
#[derive(Clone, Copy)]
pub(super) struct FunctionSignature<'a> {
    pub(super) owner: DeclarationOwner,
    pub(super) name: &'a str,
    pub(super) receiver: Option<Ty>,
    pub(super) params: &'a [Ty],
    pub(super) ret: Ty,
    /// Exact declarations this one overrides, as the member hierarchy froze them.
    pub(super) overridden: &'a [crate::types::OverriddenDeclaration],
}

impl<'a> FunctionSignature<'a> {
    pub(super) fn new(owner: DeclarationOwner, name: &'a str, params: &'a [Ty], ret: Ty) -> Self {
        Self {
            owner,
            name,
            receiver: None,
            params,
            ret,
            overridden: &[],
        }
    }

    pub(super) fn with_receiver(mut self, receiver: Option<Ty>) -> Self {
        self.receiver = receiver;
        self
    }

    pub(super) fn with_overridden(
        mut self,
        overridden: &'a [crate::types::OverriddenDeclaration],
    ) -> Self {
        self.overridden = overridden;
        self
    }
}

/// Whether a provider-owned classifier identity denotes this Kotlin builtin.
///
/// JVM mapped types are target ABI aliases, so accepting their exact interned identities here is
/// representation normalization, not semantic lookup. No source spelling is rendered or interned.
pub(super) fn classifier_matches(owner: TypeName, kotlin: &str) -> bool {
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

/// Whether Kotlin gives this member a SPECIAL BRIDGE, so that a call through the wide type does
/// not always reach an override at all.
///
/// `Map<Any, Any>.get(key: Any)` is declared with a NON-NULL parameter, and a caller holding the
/// same object as a `Map<Any?, Any?>` may pass `null`. Kotlin does not call the override there: it
/// answers the member's default — `null` for `get` and `remove`, `false` for a `contains`, `-1` for
/// an `indexOf` — because the argument cannot be what the declaration accepts. The corpus asks
/// exactly that (`specialBuiltins/notEmptyMap.kt`, `bridges/special.kt`).
///
/// The receiver dispatch has no bridge to put in front of an implementor's arm, so it declines
/// these rather than calling an override Kotlin would have skipped. Every one of them takes an
/// ARGUMENT, which is why the nullary members are unaffected.
pub(super) fn has_special_bridge(collection: crate::types::MappedCollection, name: &str) -> bool {
    let over_a_map = matches!(
        collection.kind,
        crate::types::CollectionKind::Map | crate::types::CollectionKind::MapEntry
    );
    let over_a_collection = matches!(
        collection.kind,
        crate::types::CollectionKind::Iterable
            | crate::types::CollectionKind::Collection
            | crate::types::CollectionKind::List
            | crate::types::CollectionKind::Set
    );
    match name {
        "get" | "containsKey" | "containsValue" | "getOrDefault" => over_a_map,
        // `MutableCollection.remove(element)` is as special as `Map.remove(key)`.
        "remove" => over_a_map || over_a_collection,
        "contains" | "indexOf" | "lastIndexOf" => over_a_collection,
        _ => false,
    }
}

/// The one member of a functional interface the RUNTIME knows, or `None` for any other.
///
/// A `fun interface` declared in this file becomes an object wearing that interface's table, so a
/// caller reaches its member through a program-wide number. One the runtime knows needs no number
/// at all: nothing but its single member is ever asked of it, and every caller here is the runtime
/// or a call site that can see the type — so the object is the ordinary FUNCTION VALUE the lambda
/// already is, answering through the one invoke slot every function value declares.
///
/// `kotlin.Comparator` is the only one. Its `compare` is what `sortWith` calls, and a program
/// calling it directly is the same invoke.
pub(super) fn runtime_functional_interface(
    classifier: crate::types::TypeName,
) -> Option<&'static str> {
    classifier_matches(classifier, "kotlin/Comparator").then_some("compare")
}

/// Whether a type is the `Comparator` the runtime makes — a function value of two arguments.
pub(super) fn is_comparator(internal: crate::types::TypeName) -> bool {
    runtime_functional_interface(internal).is_some()
}

/// Whether a superclass is `kotlin.Number`, the other base the runtime owns that a source class
/// may extend.
///
/// It carries NO state — every member it declares is an abstract conversion — so a subclass of it
/// is laid out exactly as a subclass of `kotlin.Any` is, and the descriptor exists already: boxed
/// primitives point at it so that `is Number` has something to compare.
pub(super) fn is_number_base(owner: crate::types::TypeName) -> bool {
    classifier_matches(owner, "kotlin/Number")
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
pub(super) fn runtime_function(signature: FunctionSignature<'_>) -> Option<String> {
    if signature.receiver.is_some() {
        return None;
    }
    if signature.owner.package_matches("kotlin") {
        return match (
            signature.name,
            signature.params,
            signature.ret.canonical_semantic(),
        ) {
            // `TODO()`, a throw a program writes on purpose: a `Nothing`, so the caller's own
            // bottom-value contract takes over from here. `require`, `check` and `error` are
            // [`precondition`]'s, which the caller asks first; they are not named twice.
            ("TODO", [], Ty::Nothing) => Some("kt_not_implemented".to_string()),
            ("TODO", [reason], Ty::Nothing) if classifier_is(*reason, "kotlin/String") => {
                Some("kt_not_implemented_reason".to_string())
            }
            _ => None,
        };
    }
    if signature.owner.package_matches("kotlin/math") {
        return match (
            signature.name,
            signature.params,
            signature.ret.canonical_semantic(),
        ) {
            // `kotlin.math.abs`, one per width it is declared over. The operand crosses at its OWN
            // width and the answer comes back at it: `abs` of a `Long` is a `Long`, and computing it
            // at any other width would change what the minimum answers.
            ("abs", [Ty::Int], Ty::Int) => Some("kt_abs_int".to_string()),
            ("abs", [Ty::Long], Ty::Long) => Some("kt_abs_long".to_string()),
            ("abs", [Ty::Float], Ty::Float) => Some("kt_abs_float".to_string()),
            ("abs", [Ty::Double], Ty::Double) => Some("kt_abs_double".to_string()),
            _ => None,
        };
    }
    if signature.owner.package_matches("kotlin/collections") {
        // The overflow guard `forEachIndexed` and its relatives carry. A jar provider presents
        // those as INLINE declarations, so their bodies are spliced into the caller and this call
        // comes with them; a klib provider answers the walk itself and never mentions it. Kotlin's
        // own is `throw ArithmeticException("Index overflow has happened.")`.
        return matches!(
            (signature.name, signature.params, signature.ret),
            ("throwIndexOverflow", [], Ty::Unit)
        )
        .then(|| "kt_throw_index_overflow".to_string());
    }
    None
}

/// The exact `kotlin.internal.getProgressionLastElement` overload selected for a counted-loop
/// header. Its implementation is part of the Native range runtime because the backend itself uses
/// the same calculation when materializing a progression object.
pub(super) fn progression_last_element(signature: FunctionSignature<'_>) -> Option<&'static str> {
    if !signature.owner.package_matches("kotlin/internal")
        || signature.name != "getProgressionLastElement"
        || signature.receiver.is_some()
    {
        return None;
    }
    match (signature.params, signature.ret.canonical_semantic()) {
        ([Ty::Int, Ty::Int, Ty::Int], Ty::Int) => Some("kt_progression_last_int"),
        ([Ty::Long, Ty::Long, Ty::Long], Ty::Long) => Some("kt_progression_last_long"),
        ([Ty::UInt, Ty::UInt, Ty::Int], Ty::UInt) => Some("kt_progression_last_uint"),
        ([Ty::ULong, Ty::ULong, Ty::Long], Ty::ULong) => Some("kt_progression_last_ulong"),
        _ => None,
    }
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

/// Whether this names the REIFIED `assertFailsWith`, whose class operand is its type argument.
///
/// Two spellings arrive, because two providers do different things with the same declaration. A
/// JVM provider splices the inline body, and what reaches a backend is the non-inline half
/// kotlin-test names `assertFailsWithAny`. A klib provider publishes no inline-ness it has no body
/// for, so the declaration arrives under its own name.
///
/// The SHAPE is what separates the reified overload from its siblings, and it has to: the others
/// take a `KClass` operand and this one takes none — its type argument is resolved into the call's
/// RETURN type before a backend sees it, which is where the caller reads the class to test against.
/// So only `(block)` and `(message, block)` are admitted, and an `(exceptionClass, …)` overload
/// falls through to the ordinary declining path rather than silently ignoring its first argument.
pub(super) fn is_assert_fails_with(owner: DeclarationOwner, name: &str, params: &[Ty]) -> bool {
    if !owner.package_matches("kotlin/test") {
        return false;
    }
    if name == "assertFailsWithAny" {
        return true;
    }
    name == "assertFailsWith"
        && matches!(params.last(), Some(Ty::Fun(_)))
        && match params {
            [_] => true,
            [message, _] => is_string_type(message),
            _ => false,
        }
}

/// A `build…` function: a fresh subject the block fills, answered once it has.
///
/// `buildString { append(1) }` is `StringBuilder().apply { … }.toString()` written shorter, and the
/// rearrangement is the same one the scope functions above get — the difference is only that the
/// subject is MADE here rather than written by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Builder {
    /// `buildString(builderAction: StringBuilder.() -> Unit): String`.
    Text,
    /// `buildList(builderAction: MutableList<E>.() -> Unit): List<E>`.
    List,
}

/// Which `build…` function a selected top-level declaration is, or `None` for anything else.
///
/// The optional `capacity` is Kotlin's own second overload of each, and it is a HINT: a program
/// cannot read it back, so passing it on or dropping it are both correct and it is passed on.
pub(super) fn builder_scope(owner: DeclarationOwner, name: &str, params: &[Ty]) -> Option<Builder> {
    let builder = if owner.package_matches("kotlin/text") && name == "buildString" {
        Builder::Text
    } else if owner.package_matches("kotlin/collections") && name == "buildList" {
        Builder::List
    } else {
        return None;
    };
    // The BLOCK is what makes this the declaration it looks like; `buildList`'s siblings
    // `buildSet` and `buildMap` have the same shape and are not answered, so they are not named
    // above rather than being separated here.
    match params {
        [Ty::Fun(_)] => Some(builder),
        [Ty::Int, Ty::Fun(_)] => Some(builder),
        _ => None,
    }
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

/// Which member of `kotlin.Enum` an accessor names, or `None` for anything else.
///
/// Every enum constant answers `name` and `ordinal` from the storage its base contributes. The
/// accessor arrives as the property's Kotlin name, so these are the only two spellings.
pub(super) fn enum_member(owner: TypeName, accessor: &str) -> Option<&'static str> {
    if !classifier_matches(owner, "kotlin/Enum") {
        return None;
    }
    match accessor {
        "name" => Some("name"),
        "ordinal" => Some("ordinal"),
        _ => None,
    }
}

/// Whether a declaration's owner is the file facade `lazy` and `Lazy.getValue` live in. Both are
/// top-level declarations of `kotlin`, so they reach a backend as members of `kotlin/LazyKt`.
pub(super) fn is_lazy_facade(owner: DeclarationOwner) -> bool {
    owner.package_is(crate::types::wk::kotlin_package())
}

/// Whether a declaration's owner is the file facade `to` lives in. `kotlin.to` is a top-level
/// extension, so it reaches a backend as a member of `kotlin/TuplesKt` — the `kotlin/io/ConsoleKt`
/// situation again, normalized in the same place.
pub(super) fn is_tuples_facade(owner: DeclarationOwner) -> bool {
    owner.package_is(crate::types::wk::kotlin_package())
}

/// Whether a declaration's owner is the collections file facade `listOf` and its neighbours live
/// in. They are top-level functions of `kotlin.collections`, so they reach a backend as members of
/// the facade class the stdlib declares them in — the `kotlin/io/ConsoleKt` situation again, and
/// normalized in the same place.
pub(super) fn is_collections_facade(owner: DeclarationOwner) -> bool {
    owner.package_is(crate::types::wk::kotlin_collections_package())
}

/// Whether a selected top-level range declaration belongs to `kotlin.ranges`.
pub(super) fn is_ranges_facade(owner: DeclarationOwner) -> bool {
    owner.package_is(crate::types::wk::kotlin_ranges_package())
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

/// Whether this is the selected `Comparable.compareTo` declaration.
///
/// Native answers a call whose receiver is only known as `Comparable` through the runtime
/// descriptor. This matches the complete semantic declaration signature (owner, callable name,
/// parameter types, and result), not a provider-assigned role; the caller separately decides
/// whether a program-defined implementor can stand behind the receiver.
pub(super) fn is_comparable_compare_to(signature: FunctionSignature<'_>) -> bool {
    signature.owner.classifier_matches("kotlin/Comparable")
        && signature.name == "compareTo"
        && matches!(signature.params, [Ty::TyParam(..)])
        && signature.ret == Ty::Int
}

/// Whether a type name is `kotlin.Comparable` or the base whose comparison an enum inherits.
///
/// Both are what a DECLARED class naming one of them makes observable: an object of the program's
/// standing behind a `Comparable` receiver, which the runtime's descriptor tables cannot order.
pub(super) fn is_comparable_supertype(internal: crate::types::TypeName) -> bool {
    internal == crate::types::wk::comparable() || internal == crate::types::wk::kotlin_enum()
}

/// One SHAPE of the runtime's collections.
///
/// A shape groups the types whose objects are interchangeable at a call site: a class standing
/// behind one type of a shape could stand behind any other type of the same shape, and behind no
/// type of another. `Set` shares the `Iterable` shape because a set implementor answers a
/// `Collection` and an `Iterable` receiver too; `Map` has its own because Kotlin's map is no
/// `Collection`, and `Sequence` has its own for the same reason.
///
/// This is what makes a file's declaration of its own collection a question about the RECEIVER
/// rather than about the file: a file that declares a `Sequence` of its own endangers a receiver
/// typed by a sequence and no list receiver anywhere.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum CollectionShape {
    /// Anything a `for` loop walks directly: a list, a set, a range, an array, text.
    Iterable,
    /// The iterator itself, which a walk asks `hasNext` and `next`.
    Iterator,
    /// A map, which is neither an `Iterable` nor an iterator.
    Map,
    /// ONE entry of a map, which `entries` hands out and a destructuring reads.
    MapEntry,
    /// A lazy sequence, which is no `Iterable` either.
    Sequence,
    /// TEXT — `String`, `CharSequence`, `StringBuilder`. A `for` loop walks it as the `Iterable`
    /// shape is walked, but its OWN members are `length` and the indexed read rather than an
    /// iterator, so a class of the program behind one is reached differently from one behind a
    /// list. That is the whole reason it is a shape of its own.
    Text,
}

/// Native realization selected by one complete stdlib collection signature.
///
/// These are declarations whose bodies are not available through the active JVM provider. The
/// key remains the Kotlin package, extension receiver, value parameters, and result together; a
/// same-named user function or another overload cannot enter this table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CollectionAlgorithm {
    Map,
    ForEach,
    WithIndex,
    JoinToStringDefaults,
    Any,
    All,
    None,
    IsNotEmpty,
    IsEmpty,
    Count,
    CountMatching,
    Filter,
    FilterNot,
    First,
    FirstMatching,
    FirstOrNull,
    Last,
    LastMatching,
    Fold,
    ForEachIndexed,
    ToList,
    Reversed,
    IndexOf,
    Contains,
    SumOfInt,
    SumOfLong,
    SumOfDouble,
    PlusElement,
    PlusAll,
    AsSequence,
    ToTypedArray,
    SortedWith,
    SortWith,
    PlusAssignElement,
    PlusAssignAll,
}

fn classifier_is(ty: Ty, expected: &str) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(|owner| classifier_matches(owner, expected))
}

fn collection_element(ty: Ty) -> Option<Ty> {
    let ty = ty.non_null();
    if let Some(element) = ty.array_read_elem() {
        return Some(element.canonical_semantic());
    }
    match ty {
        Ty::Obj(owner, [element])
            if classifier_matches(owner, "kotlin/collections/Iterable")
                || classifier_matches(owner, "kotlin/collections/Iterator")
                || classifier_matches(owner, "kotlin/collections/Collection")
                || classifier_matches(owner, "kotlin/collections/List")
                || classifier_matches(owner, "kotlin/collections/Set")
                || classifier_matches(owner, "kotlin/collections/MutableCollection")
                || classifier_matches(owner, "kotlin/collections/MutableList") =>
        {
            Some(
                element
                    .projection_inner()
                    .unwrap_or(*element)
                    .canonical_semantic(),
            )
        }
        Ty::Obj(owner, [key, value]) if classifier_matches(owner, "kotlin/collections/Map") => {
            Some(Ty::obj_args(
                "kotlin/collections/Map$Entry",
                &[
                    key.projection_inner().unwrap_or(*key).canonical_semantic(),
                    value
                        .projection_inner()
                        .unwrap_or(*value)
                        .canonical_semantic(),
                ],
            ))
        }
        _ => None,
    }
}

fn unary_function(ty: Ty, parameter: Ty) -> Option<Ty> {
    let Ty::Fun(function) = ty.non_null() else {
        return None;
    };
    (function.context_count == 0
        && !function.has_receiver
        && !function.suspend
        && function.params.len() == 1
        && function.params[0].canonical_semantic() == parameter.canonical_semantic())
    .then_some(function.ret.canonical_semantic())
}

fn binary_function(ty: Ty, first: Ty, second: Ty) -> Option<Ty> {
    let Ty::Fun(function) = ty.non_null() else {
        return None;
    };
    (function.context_count == 0
        && !function.has_receiver
        && !function.suspend
        && function.params.len() == 2
        && function.params[0].canonical_semantic() == first.canonical_semantic()
        && function.params[1].canonical_semantic() == second.canonical_semantic())
    .then_some(function.ret.canonical_semantic())
}

fn list_result(ty: Ty, element: Ty) -> bool {
    matches!(
        ty.non_null(),
        Ty::Obj(owner, [actual])
            if classifier_matches(owner, "kotlin/collections/List")
                && actual.canonical_semantic() == element.canonical_semantic()
    )
}

fn indexed_result(ty: Ty, shape: &str, element: Ty) -> bool {
    let Ty::Obj(owner, [entry]) = ty.non_null() else {
        return false;
    };
    if !classifier_matches(owner, shape) {
        return false;
    }
    matches!(
        entry.non_null(),
        Ty::Obj(indexed, [actual])
            if classifier_matches(indexed, "kotlin/collections/IndexedValue")
                && actual.canonical_semantic() == element.canonical_semantic()
    )
}

fn single_argument_result(ty: Ty, shape: &str, element: Ty) -> bool {
    matches!(
        ty.non_null(),
        Ty::Obj(owner, [actual])
            if classifier_matches(owner, shape)
                && actual.canonical_semantic() == element.canonical_semantic()
    )
}

fn iterable_argument(ty: Ty, element: Ty) -> bool {
    if ty
        .array_read_elem()
        .is_some_and(|actual| actual.canonical_semantic() == element.canonical_semantic())
    {
        return true;
    }
    matches!(
        ty.non_null(),
        Ty::Obj(owner, [actual])
            if (classifier_matches(owner, "kotlin/collections/Iterable")
                || classifier_matches(owner, "kotlin/sequences/Sequence"))
                && actual.projection_inner().unwrap_or(*actual).canonical_semantic()
                    == element.canonical_semantic()
    )
}

fn comparator_of(ty: Ty, element: Ty) -> bool {
    matches!(
        ty.non_null(),
        Ty::Obj(owner, [actual])
            if classifier_matches(owner, "kotlin/Comparator")
                && actual.projection_inner().unwrap_or(*actual).canonical_semantic()
                    == element.canonical_semantic()
    )
}

/// Match the exact Kotlin declaration signature of a collection algorithm realized by the Native
/// runtime while the active provider cannot publish its body.
pub(super) fn collection_algorithm(
    signature: FunctionSignature<'_>,
) -> Option<CollectionAlgorithm> {
    // Text walk adapters are declarations of `kotlin.text`, not `kotlin.collections`. Keep their
    // complete signatures here rather than treating every text function as a collection operation
    // or keying on the call-site receiver's representation.
    if signature
        .owner
        .package_is(crate::types::wk::kotlin_text_package())
    {
        let receiver = signature.receiver?;
        if !classifier_is(receiver, "kotlin/CharSequence") || !signature.params.is_empty() {
            return None;
        }
        return match signature.name {
            "asSequence"
                if single_argument_result(signature.ret, "kotlin/sequences/Sequence", Ty::Char) =>
            {
                Some(CollectionAlgorithm::AsSequence)
            }
            "withIndex"
                if indexed_result(signature.ret, "kotlin/collections/Iterable", Ty::Char) =>
            {
                Some(CollectionAlgorithm::WithIndex)
            }
            _ => None,
        };
    }
    // A sequence's `withIndex` is lazy and returns another sequence. The runtime wrapper already
    // preserves that lazy iterator boundary; only this exact declaration may select it.
    if signature
        .owner
        .package_is(crate::types::wk::kotlin_sequences_package())
    {
        let receiver = signature.receiver?.non_null();
        let Ty::Obj(owner, [element]) = receiver else {
            return None;
        };
        let element = element
            .projection_inner()
            .unwrap_or(*element)
            .canonical_semantic();
        return (classifier_matches(owner, "kotlin/sequences/Sequence")
            && signature.name == "withIndex"
            && signature.params.is_empty()
            && indexed_result(signature.ret, "kotlin/sequences/Sequence", element))
        .then_some(CollectionAlgorithm::WithIndex);
    }
    if !signature
        .owner
        .package_is(crate::types::wk::kotlin_collections_package())
    {
        return None;
    }
    let receiver = signature.receiver?;
    let element = collection_element(receiver)?;
    match (signature.name, signature.params) {
        ("map", [transform]) => {
            let output = unary_function(*transform, element)?;
            list_result(signature.ret, output).then_some(CollectionAlgorithm::Map)
        }
        ("forEach", [action]) => (unary_function(*action, element) == Some(Ty::Unit)
            && signature.ret == Ty::Unit)
            .then_some(CollectionAlgorithm::ForEach),
        ("withIndex", []) => {
            let result_shape = if classifier_is(receiver, "kotlin/collections/Iterator") {
                "kotlin/collections/Iterator"
            } else {
                "kotlin/collections/Iterable"
            };
            indexed_result(signature.ret, result_shape, element)
                .then_some(CollectionAlgorithm::WithIndex)
        }
        ("joinToString", [separator, prefix, postfix, limit, truncated, transform]) => {
            let transform = match transform.non_null() {
                Ty::Fun(function)
                    if function.context_count == 0
                        && !function.has_receiver
                        && !function.suspend
                        && function.params.len() == 1
                        && function.params[0].canonical_semantic()
                            == element.canonical_semantic()
                        && classifier_is(function.ret, "kotlin/CharSequence") =>
                {
                    true
                }
                _ => false,
            };
            (classifier_is(*separator, "kotlin/CharSequence")
                && classifier_is(*prefix, "kotlin/CharSequence")
                && classifier_is(*postfix, "kotlin/CharSequence")
                && limit.canonical_semantic() == Ty::Int
                && classifier_is(*truncated, "kotlin/CharSequence")
                && transform
                && classifier_is(signature.ret, "kotlin/String"))
            .then_some(CollectionAlgorithm::JoinToStringDefaults)
        }
        ("any", []) if signature.ret == Ty::Boolean => Some(CollectionAlgorithm::IsNotEmpty),
        ("none", []) if signature.ret == Ty::Boolean => Some(CollectionAlgorithm::IsEmpty),
        ("any", [predicate])
            if unary_function(*predicate, element) == Some(Ty::Boolean)
                && signature.ret == Ty::Boolean =>
        {
            Some(CollectionAlgorithm::Any)
        }
        ("all", [predicate])
            if unary_function(*predicate, element) == Some(Ty::Boolean)
                && signature.ret == Ty::Boolean =>
        {
            Some(CollectionAlgorithm::All)
        }
        ("none", [predicate])
            if unary_function(*predicate, element) == Some(Ty::Boolean)
                && signature.ret == Ty::Boolean =>
        {
            Some(CollectionAlgorithm::None)
        }
        ("count", []) if signature.ret == Ty::Int => Some(CollectionAlgorithm::Count),
        ("count", [predicate])
            if unary_function(*predicate, element) == Some(Ty::Boolean)
                && signature.ret == Ty::Int =>
        {
            Some(CollectionAlgorithm::CountMatching)
        }
        ("filter", [predicate])
            if unary_function(*predicate, element) == Some(Ty::Boolean)
                && list_result(signature.ret, element) =>
        {
            Some(CollectionAlgorithm::Filter)
        }
        ("filterNot", [predicate])
            if unary_function(*predicate, element) == Some(Ty::Boolean)
                && list_result(signature.ret, element) =>
        {
            Some(CollectionAlgorithm::FilterNot)
        }
        ("first", []) if signature.ret.canonical_semantic() == element.canonical_semantic() => {
            Some(CollectionAlgorithm::First)
        }
        ("first", [predicate])
            if unary_function(*predicate, element) == Some(Ty::Boolean)
                && signature.ret.canonical_semantic() == element.canonical_semantic() =>
        {
            Some(CollectionAlgorithm::FirstMatching)
        }
        ("firstOrNull", [predicate])
            if unary_function(*predicate, element) == Some(Ty::Boolean)
                && signature.ret.is_nullable()
                && signature.ret.non_null().canonical_semantic()
                    == element.non_null().canonical_semantic() =>
        {
            Some(CollectionAlgorithm::FirstOrNull)
        }
        ("last", []) if signature.ret.canonical_semantic() == element.canonical_semantic() => {
            Some(CollectionAlgorithm::Last)
        }
        ("last", [predicate])
            if unary_function(*predicate, element) == Some(Ty::Boolean)
                && signature.ret.canonical_semantic() == element.canonical_semantic() =>
        {
            Some(CollectionAlgorithm::LastMatching)
        }
        ("fold", [initial, operation])
            if binary_function(*operation, *initial, element)
                == Some(initial.canonical_semantic())
                && signature.ret.canonical_semantic() == initial.canonical_semantic() =>
        {
            Some(CollectionAlgorithm::Fold)
        }
        ("forEachIndexed", [action])
            if binary_function(*action, Ty::Int, element) == Some(Ty::Unit)
                && signature.ret == Ty::Unit =>
        {
            Some(CollectionAlgorithm::ForEachIndexed)
        }
        ("toList", []) if list_result(signature.ret, element) => Some(CollectionAlgorithm::ToList),
        ("reversed", []) if list_result(signature.ret, element) => {
            Some(CollectionAlgorithm::Reversed)
        }
        ("indexOf", [value])
            if value.canonical_semantic() == element.canonical_semantic()
                && signature.ret == Ty::Int =>
        {
            Some(CollectionAlgorithm::IndexOf)
        }
        ("contains", [value])
            if value.canonical_semantic() == element.canonical_semantic()
                && signature.ret == Ty::Boolean =>
        {
            Some(CollectionAlgorithm::Contains)
        }
        ("sumOf", [selector])
            if unary_function(*selector, element) == Some(Ty::Int) && signature.ret == Ty::Int =>
        {
            Some(CollectionAlgorithm::SumOfInt)
        }
        ("sumOf", [selector])
            if unary_function(*selector, element) == Some(Ty::Long)
                && signature.ret == Ty::Long =>
        {
            Some(CollectionAlgorithm::SumOfLong)
        }
        ("sumOf", [selector])
            if unary_function(*selector, element) == Some(Ty::Double)
                && signature.ret == Ty::Double =>
        {
            Some(CollectionAlgorithm::SumOfDouble)
        }
        ("plus", [operand])
            if operand.canonical_semantic() == element.canonical_semantic()
                && list_result(signature.ret, element) =>
        {
            Some(CollectionAlgorithm::PlusElement)
        }
        ("plus", [operand])
            if iterable_argument(*operand, element) && list_result(signature.ret, element) =>
        {
            Some(CollectionAlgorithm::PlusAll)
        }
        ("asSequence", [])
            if single_argument_result(signature.ret, "kotlin/sequences/Sequence", element) =>
        {
            Some(CollectionAlgorithm::AsSequence)
        }
        ("toTypedArray", []) if single_argument_result(signature.ret, "kotlin/Array", element) => {
            Some(CollectionAlgorithm::ToTypedArray)
        }
        ("sortedWith", [comparator])
            if comparator_of(*comparator, element) && list_result(signature.ret, element) =>
        {
            Some(CollectionAlgorithm::SortedWith)
        }
        ("sortWith", [comparator])
            if comparator_of(*comparator, element) && signature.ret == Ty::Unit =>
        {
            Some(CollectionAlgorithm::SortWith)
        }
        ("plusAssign", [operand]) if signature.ret == Ty::Unit => {
            let Ty::Obj(owner, [receiver_element]) = receiver.non_null() else {
                return None;
            };
            if !classifier_matches(owner, "kotlin/collections/MutableCollection") {
                return None;
            }
            let receiver_element = receiver_element.projection_inner()?;
            if receiver_element.canonical_semantic() == operand.canonical_semantic() {
                return Some(CollectionAlgorithm::PlusAssignElement);
            }
            let all_elements = operand
                .array_read_elem()
                .or_else(|| {
                    let Ty::Obj(owner, [element]) = operand.non_null() else {
                        return None;
                    };
                    (classifier_matches(owner, "kotlin/collections/Iterable")
                        || classifier_matches(owner, "kotlin/sequences/Sequence"))
                    .then_some(element.projection_inner().unwrap_or(*element))
                })
                .map(Ty::canonical_semantic);
            (all_elements == Some(receiver_element.canonical_semantic()))
                .then_some(CollectionAlgorithm::PlusAssignAll)
        }
        _ => None,
    }
}

/// Runtime realization of one exact selected iteration declaration.
///
/// `shape` is physical receiver representation: it says which runtime-owned objects can answer the
/// call. Declaration identity remains the complete semantic signature. In particular, an unrelated
/// `iterator`, `hasNext`, or `next` with the same arity never becomes an iteration operation merely
/// because its receiver happens to have a walkable layout.
pub(super) fn iteration_runtime_member(
    shape: CollectionShape,
    declaration_shape: Option<CollectionShape>,
    result_shape: Option<CollectionShape>,
    signature: FunctionSignature<'_>,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    let FunctionSignature {
        owner: _,
        name,
        receiver: _,
        params,
        ret,
        overridden: _,
    } = signature;
    if !params.is_empty() {
        return None;
    }

    let iterable_declaration = matches!(
        declaration_shape,
        Some(
            CollectionShape::Iterable
                | CollectionShape::Map
                | CollectionShape::Sequence
                | CollectionShape::Text
        )
    );
    if matches!(
        shape,
        CollectionShape::Iterable
            | CollectionShape::Map
            | CollectionShape::Sequence
            | CollectionShape::Text
    ) && iterable_declaration
        && name == "iterator"
        && result_shape == Some(CollectionShape::Iterator)
    {
        return Some((
            "kt_iterable_iterator",
            vec![Ty::obj("kotlin/Any")],
            Ty::obj("kotlin/Any"),
        ));
    }

    if shape != CollectionShape::Iterator {
        return None;
    }
    if declaration_shape != Some(CollectionShape::Iterator) {
        return None;
    }
    if name == "hasNext" && ret == Ty::Boolean {
        return Some((
            "kt_iterator_has_next",
            vec![Ty::obj("kotlin/Any")],
            Ty::Boolean,
        ));
    }
    let primitive_next = signature
        .owner
        .semantic_classifier()
        .and_then(primitive_iterator_element);
    let next = match name {
        "next" if matches!(ret, Ty::TyParam(..)) || primitive_next == Some(ret) => true,
        "nextBoolean" if ret == Ty::Boolean => true,
        "nextByte" if ret == Ty::Byte => true,
        "nextChar" if ret == Ty::Char => true,
        "nextShort" if ret == Ty::Short => true,
        "nextInt" if ret == Ty::Int => true,
        "nextLong" if ret == Ty::Long => true,
        "nextFloat" if ret == Ty::Float => true,
        "nextDouble" if ret == Ty::Double => true,
        _ => false,
    };
    next.then(|| {
        (
            "kt_iterator_next",
            vec![Ty::obj("kotlin/Any")],
            Ty::obj("kotlin/Any"),
        )
    })
}

/// The semantic element returned by a Kotlin primitive iterator's boxed `next()` override.
///
/// The provider hierarchy establishes that the declaration is an iterator; the exact classifier
/// identity establishes its element width. This is a native representation boundary, not source
/// resolution: the runtime has one object protocol for these concrete stdlib iterator classes.
fn primitive_iterator_element(owner: TypeName) -> Option<Ty> {
    Some(
        if classifier_matches(owner, "kotlin/collections/BooleanIterator") {
            Ty::Boolean
        } else if classifier_matches(owner, "kotlin/collections/ByteIterator") {
            Ty::Byte
        } else if classifier_matches(owner, "kotlin/collections/CharIterator") {
            Ty::Char
        } else if classifier_matches(owner, "kotlin/collections/ShortIterator") {
            Ty::Short
        } else if classifier_matches(owner, "kotlin/collections/IntIterator") {
            Ty::Int
        } else if classifier_matches(owner, "kotlin/collections/LongIterator") {
            Ty::Long
        } else if classifier_matches(owner, "kotlin/collections/FloatIterator") {
            Ty::Float
        } else if classifier_matches(owner, "kotlin/collections/DoubleIterator") {
            Ty::Double
        } else {
            return None;
        },
    )
}

/// Whether this names `kotlin.CharSequence`, under either spelling a provider may hand over.
///
/// It wears a runtime descriptor for the reason `Number` and `Comparable` do: no instances of its
/// own, and both the string and the builder point at it, so a cast or an `is` against it has
/// something to compare.
pub(super) fn is_char_sequence(internal: crate::types::TypeName) -> bool {
    classifier_matches(internal, "kotlin/CharSequence")
}

/// `x.indices` — the range of an indexable value's positions, which the provider presents as an
/// extension property of the arrays or text file facade rather than a member.
///
/// Answering it needs the receiver's own `size`, so only the caller can decide whether THIS
/// receiver has one; this says only that the declaration named is that extension property.
///
/// `name` is the PROPERTY's, not its accessor's. `indices` is realized five ways — `getIndices`
/// for text, and one value-class-mangled spelling per unsigned array width — so a table written in
/// accessor spellings would have to list all five and would still be guessing at the sixth.
pub(super) fn is_indices(owner: DeclarationOwner, name: &str) -> bool {
    name == "indices"
        && (owner.package_is(crate::types::wk::kotlin_collections_package())
            || owner.package_is(crate::types::wk::kotlin_text_package()))
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

/// The runtime marker an `is` against one of Kotlin's REFLECTION types asks about.
///
/// A property reference is an object of a type of its own — the generator emits one per property —
/// so the type written at the site is never the object's, exactly as for a function value. These
/// markers are what the two have in common.
pub(super) fn reflection_type_descriptor(owner: crate::types::TypeName) -> Option<&'static str> {
    [
        ("kotlin/reflect/KCallable", "kt_type_kcallable"),
        ("kotlin/reflect/KProperty", "kt_type_kproperty"),
        ("kotlin/reflect/KProperty0", "kt_type_kproperty0"),
        ("kotlin/reflect/KProperty1", "kt_type_kproperty1"),
        ("kotlin/reflect/KProperty2", "kt_type_kproperty2"),
        (
            "kotlin/reflect/KMutableProperty",
            "kt_type_kmutable_property",
        ),
        (
            "kotlin/reflect/KMutableProperty0",
            "kt_type_kmutable_property0",
        ),
        (
            "kotlin/reflect/KMutableProperty1",
            "kt_type_kmutable_property1",
        ),
        (
            "kotlin/reflect/KMutableProperty2",
            "kt_type_kmutable_property2",
        ),
    ]
    .into_iter()
    .find_map(|(classifier, descriptor)| owner.matches(classifier).then_some(descriptor))
}

/// Every marker a PROPERTY REFERENCE of this shape wears, flattened as `KType.interfaces` requires.
///
/// Kotlin's hierarchy is `KCallable` → `KProperty` → `KPropertyN`, with `KMutableProperty` and
/// `KMutablePropertyN` beside them for a `var`. An interface's own bases are not walked at an `is`,
/// so every one of them is named rather than only the most derived.
pub(super) fn property_reference_markers(mutable: bool, arity: usize) -> Vec<&'static str> {
    let mut markers = vec!["kt_type_kcallable", "kt_type_kproperty"];
    markers.extend(match arity {
        0 => Some("kt_type_kproperty0"),
        1 => Some("kt_type_kproperty1"),
        2 => Some("kt_type_kproperty2"),
        _ => None,
    });
    if mutable {
        markers.push("kt_type_kmutable_property");
        markers.extend(match arity {
            0 => Some("kt_type_kmutable_property0"),
            1 => Some("kt_type_kmutable_property1"),
            2 => Some("kt_type_kmutable_property2"),
            _ => None,
        });
    }
    markers
}

/// A member of the collections facade the runtime answers for an ARRAY receiver, as
/// `(symbol, carried, answer)`.
///
/// Every one of these is an extension, and the facade declares the same names over lists, sequences
/// and ranges — so the owner cannot say which receiver this is and the CALLER asks the receiver.
/// What an entry names is the runtime function for the array case only.
///
/// `toList` and `reversed` answer a LIST of boxes; `reversedArray` answers an array wearing the
/// receiver's own descriptor, which is the whole of the difference between the last two. The
/// `content…` three are the questions `Arrays.equals`/`hashCode`/`toString` answer — an array's own
/// `equals` is identity, and these exist precisely because a program sometimes wants the other one.
pub(super) fn array_member(
    owner: DeclarationOwner,
    name: &str,
    params: &[Ty],
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    // The unsigned arrays get their own package: Kotlin declares `UIntArray.reversed()` in
    // `kotlin.collections.unsigned`, apart from the signed one it answers identically to. The
    // runtime reads the element's type from the array's descriptor, so both reach one entry.
    if !owner.package_is(crate::types::wk::kotlin_collections_package())
        && !owner.package_is(crate::types::wk::kotlin_collections_unsigned_package())
    {
        return None;
    }
    let reference = Ty::nullable(Ty::obj("kotlin/Any"));
    Some(match (name, params) {
        ("toList", []) => ("kt_array_to_list", vec![reference], reference),
        ("reversed", []) => ("kt_array_reversed", vec![reference], reference),
        ("reversedArray", []) => ("kt_array_reversed_array", vec![reference], reference),
        // The operand is declared nullable, and so may the receiver be: `contentEquals` is one of
        // the few stdlib extensions written over `Array<T>?`, because comparing two arrays that
        // may be absent is exactly what it is for. The runtime takes both as they come.
        ("contentEquals", [_]) => (
            "kt_array_content_equals",
            vec![reference, reference],
            Ty::Boolean,
        ),
        ("contentHashCode", []) => ("kt_array_content_hash_code", vec![reference], Ty::Int),
        // The LENGTH is the whole of these two, and it is the same question whichever element
        // width the array has.
        ("isEmpty", []) => ("kt_array_is_empty", vec![reference], Ty::Boolean),
        ("isNotEmpty", []) => ("kt_array_is_not_empty", vec![reference], Ty::Boolean),
        ("contentToString", []) => (
            "kt_array_content_to_string",
            vec![reference],
            Ty::obj("kotlin/String"),
        ),
        _ => return None,
    })
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
/// beside it rather than on the value class, and receiver-type dispatch does not see them.
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
    // this package and is handled from the receiver's checked type.
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

/// The runtime function answering a `KClass` name accessor, or `None` for anything else.
///
/// Both spellings of each are taken for the reason `is_text_length` takes both: which one
/// a provider presents a property's accessor under is the provider's business, not this table's.
pub(super) fn class_name_accessor(
    owner: crate::types::TypeName,
    name: &str,
) -> Option<&'static str> {
    if !owner.matches("kotlin/reflect/KClass") {
        return None;
    }
    match name {
        "simpleName" => Some("kt_class_simple_name"),
        "qualifiedName" => Some("kt_class_qualified_name"),
        _ => None,
    }
}

/// Whether this is an object the runtime realizes ENTIRELY, so it has no instance of its own.
///
/// `kotlin.properties.Delegates` is one: it holds no state, and every member of it is answered
/// directly (see [`runtime_companion_member`]), so nothing ever reads the value a reference to it
/// would carry. A provider that materializes the receiver before the call reaches that table needs
/// something to materialize, and for such an object the honest answer is the null reference —
/// there is no object, and nothing dereferences it.
pub(super) fn is_stateless_runtime_object(classifier: crate::types::TypeName) -> bool {
    // The stdlib's standard delegates. `Delegates` declares no state and every member of it this
    // runtime answers takes its arguments alone.
    classifier.matches("kotlin/properties/Delegates")
}

/// The runtime entry point answering the companion object of a BUILT-IN type, if this names one.
///
/// Each is declared in no file krusty compiles and carries no state — every member of one is a
/// constant the frontend folds — so what a program can observe about it is its IDENTITY. The
/// runtime holds one static object per companion, with a descriptor of its own so that
/// `Int.Companion === Long.Companion` is false.
///
/// Apart from [`is_stateless_runtime_object`], which answers a NULL for an object nothing reads:
/// that will not do here, because `o === Int.Companion` is exactly what the corpus asks.
pub(super) fn builtin_companion(classifier: crate::types::TypeName) -> Option<&'static str> {
    [
        ("kotlin/Byte$Companion", "kt_byte_companion"),
        ("kotlin/Short$Companion", "kt_short_companion"),
        ("kotlin/Int$Companion", "kt_int_companion"),
        ("kotlin/Long$Companion", "kt_long_companion"),
        ("kotlin/Char$Companion", "kt_char_companion"),
        ("kotlin/Boolean$Companion", "kt_boolean_companion"),
        ("kotlin/Float$Companion", "kt_float_companion"),
        ("kotlin/Double$Companion", "kt_double_companion"),
        ("kotlin/String$Companion", "kt_string_companion"),
    ]
    .into_iter()
    .find_map(|(owner, symbol)| classifier.matches(owner).then_some(symbol))
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
pub(super) fn runtime_member(signature: FunctionSignature<'_>) -> Option<&'static str> {
    if let Some(symbol) = any_runtime_symbol(signature) {
        return Some(symbol);
    }
    // `removeSuffix` is a top-level extension of `kotlin.text`, so it arrives as a member of that
    // package's file facade; everything it takes and answers is a reference, which is this path.
    if signature.owner.package_matches("kotlin/text") {
        match (signature.name, signature.params) {
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
    if signature
        .owner
        .classifier_matches("kotlin/text/StringBuilder")
    {
        return match (signature.name, signature.params) {
            // A builder's `append` takes one of a dozen overloads on the JVM and one function here:
            // every operand is rendered through its own `toString`, which is the same answer for all of
            // them, and the reference path has already boxed whichever primitive arrived.
            ("append", [_]) => Some("kt_string_builder_append"),
            ("appendLine", [_]) => Some("kt_string_builder_append_line"),
            ("appendLine", []) => Some("kt_string_builder_append_new_line"),
            _ => None,
        };
    }
    None
}

/// The runtime function realizing the selected declaration when it is one of `Any`'s three
/// runtime-dispatched members: `kotlin/Any.toString(): String`, `kotlin/Any.hashCode(): Int` or
/// `kotlin/Any.equals(Any?): Boolean` itself, or a declaration that overrides one of them.
///
/// Both are exact declaration identities: the selected declaration's own owner and signature, and
/// the overridden declarations the member hierarchy froze for it. A member that merely has the same
/// name and shape on another owner, with no recorded override of `Any`'s, is not one of them, and
/// nothing at the call site takes part, so an inlined copy of the call asks the same question.
fn any_runtime_symbol(signature: FunctionSignature<'_>) -> Option<&'static str> {
    let own = signature.owner.classifier_matches("kotlin/Any").then(|| {
        any_declaration_symbol(
            signature.name,
            signature.receiver,
            signature.params,
            signature.ret,
        )
    });
    own.flatten().or_else(|| {
        signature.overridden.iter().find_map(|overridden| {
            classifier_matches(overridden.owner, "kotlin/Any")
                .then(|| {
                    any_declaration_symbol(
                        &overridden.name,
                        overridden.receiver,
                        &overridden.params,
                        overridden.ret,
                    )
                })
                .flatten()
        })
    })
}

/// The runtime function for one of `Any`'s own declarations, by its complete declared signature.
fn any_declaration_symbol(
    name: &str,
    receiver: Option<Ty>,
    params: &[Ty],
    ret: Ty,
) -> Option<&'static str> {
    if receiver.is_some() {
        return None;
    }
    let nullable_any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
    match (name, params, ret) {
        ("toString", [], result)
            if !matches!(result, Ty::Nullable(_)) && is_string_type(&result) =>
        {
            Some("kt_to_string")
        }
        ("hashCode", [], Ty::Int) => Some("kt_hash_code"),
        ("equals", [parameter], Ty::Boolean) if *parameter == nullable_any => Some("kt_equals"),
        _ => None,
    }
}

/// Whether the exact selected declaration is one of `Any`'s three runtime-dispatched members; see
/// [`any_runtime_symbol`].
pub(super) fn is_any_runtime_member(signature: FunctionSignature<'_>) -> bool {
    any_runtime_symbol(signature).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(path: &str) -> DeclarationOwner {
        DeclarationOwner::classifier(crate::types::type_name(path))
    }

    fn facade(path: &str) -> DeclarationOwner {
        let physical = crate::types::type_name(path);
        DeclarationOwner {
            physical,
            semantic: Some(SemanticCallableOwner::Package(
                physical.parent().unwrap_or(TypeName::ROOT),
            )),
        }
    }

    fn package(path: &str) -> DeclarationOwner {
        let physical = crate::types::type_name(path);
        DeclarationOwner {
            physical,
            semantic: Some(SemanticCallableOwner::Package(physical)),
        }
    }

    fn function<'a>(
        owner: DeclarationOwner,
        name: &'a str,
        params: &'a [Ty],
        ret: Ty,
    ) -> FunctionSignature<'a> {
        FunctionSignature::new(owner, name, params, ret)
    }

    #[test]
    fn comparable_intrinsic_requires_the_complete_declaration_signature() {
        let parameter = Ty::ty_param("T", Ty::obj_name(crate::types::wk::any()));
        let parameters = [parameter];
        let comparable = member("kotlin/Comparable");
        assert!(is_comparable_compare_to(FunctionSignature::new(
            comparable,
            "compareTo",
            &parameters,
            Ty::Int,
        )));
        assert!(is_comparable_compare_to(FunctionSignature::new(
            member("java/lang/Comparable"),
            "compareTo",
            &parameters,
            Ty::Int,
        )));

        assert!(!is_comparable_compare_to(FunctionSignature::new(
            comparable,
            "compareTo",
            &[Ty::String],
            Ty::Int,
        )));
        assert!(!is_comparable_compare_to(FunctionSignature::new(
            comparable,
            "compareTo",
            &parameters,
            Ty::Long,
        )));
        assert!(!is_comparable_compare_to(FunctionSignature::new(
            comparable,
            "compare",
            &parameters,
            Ty::Int,
        )));
    }

    #[test]
    fn iteration_runtime_members_require_complete_declaration_signatures() {
        let element = Ty::ty_param("T", Ty::obj_name(crate::types::wk::any()));
        let iterator = Ty::obj_args("kotlin/collections/Iterator", &[element]);
        let no_params = [];
        assert!(iteration_runtime_member(
            CollectionShape::Iterable,
            Some(CollectionShape::Iterable),
            Some(CollectionShape::Iterator),
            FunctionSignature::new(
                member("kotlin/collections/Iterable"),
                "iterator",
                &no_params,
                iterator,
            ),
        )
        .is_some());
        assert!(iteration_runtime_member(
            CollectionShape::Iterable,
            None,
            Some(CollectionShape::Iterator),
            FunctionSignature::new(member("sample/Iterable"), "iterator", &no_params, iterator),
        )
        .is_none());
        assert!(iteration_runtime_member(
            CollectionShape::Iterable,
            Some(CollectionShape::Iterable),
            None,
            FunctionSignature::new(
                member("kotlin/collections/Iterable"),
                "iterator",
                &no_params,
                Ty::Int,
            ),
        )
        .is_none());

        assert!(iteration_runtime_member(
            CollectionShape::Iterator,
            Some(CollectionShape::Iterator),
            None,
            FunctionSignature::new(
                member("java/util/Iterator"),
                "hasNext",
                &no_params,
                Ty::Boolean,
            ),
        )
        .is_some());
        assert!(iteration_runtime_member(
            CollectionShape::Iterator,
            Some(CollectionShape::Iterator),
            None,
            FunctionSignature::new(
                member("kotlin/collections/ByteIterator"),
                "nextByte",
                &no_params,
                Ty::Byte,
            ),
        )
        .is_some());
        assert!(iteration_runtime_member(
            CollectionShape::Iterator,
            Some(CollectionShape::Iterator),
            None,
            FunctionSignature::new(
                member("kotlin/collections/ByteIterator"),
                "nextByte",
                &no_params,
                Ty::Int,
            ),
        )
        .is_none());
    }

    #[test]
    fn collection_algorithms_require_complete_declaration_signatures() {
        let element = Ty::ty_param("T", Ty::obj_name(crate::types::wk::any()));
        let iterable = Ty::obj_args("kotlin/collections/Iterable", &[element]);
        let predicate = Ty::fun(vec![element], Ty::Boolean);
        let list = Ty::obj_args("kotlin/collections/List", &[element]);
        let parameters = [predicate];
        let exact =
            FunctionSignature::new(package("kotlin/collections"), "filter", &parameters, list)
                .with_receiver(Some(iterable));
        assert_eq!(
            collection_algorithm(exact),
            Some(CollectionAlgorithm::Filter)
        );

        let wrong_result = FunctionSignature::new(
            package("kotlin/collections"),
            "filter",
            &parameters,
            Ty::Boolean,
        )
        .with_receiver(Some(iterable));
        assert_eq!(collection_algorithm(wrong_result), None);

        let wrong_owner = FunctionSignature::new(package("sample"), "filter", &parameters, list)
            .with_receiver(Some(iterable));
        assert_eq!(collection_algorithm(wrong_owner), None);

        let text_sequence = Ty::obj_args("kotlin/sequences/Sequence", &[Ty::Char]);
        let exact_text =
            FunctionSignature::new(package("kotlin/text"), "asSequence", &[], text_sequence)
                .with_receiver(Some(Ty::obj("kotlin/CharSequence")));
        assert_eq!(
            collection_algorithm(exact_text),
            Some(CollectionAlgorithm::AsSequence)
        );
        assert_eq!(
            collection_algorithm(
                FunctionSignature::new(package("kotlin/text"), "asSequence", &[], text_sequence,)
                    .with_receiver(Some(Ty::String)),
            ),
            None,
            "the selected declaration receiver, not the call-site string layout, is the key"
        );
    }

    #[test]
    fn jvm_builtin_aliases_are_normalized_only_at_the_intrinsic_boundary() {
        assert!(classifier_matches(
            crate::types::type_name("java/util/ArrayList"),
            "kotlin/collections/ArrayList"
        ));
        assert!(classifier_matches(
            crate::types::type_name("java/lang/Comparable"),
            "kotlin/Comparable"
        ));
        assert!(!classifier_matches(
            crate::types::type_name("sample/ArrayList"),
            "kotlin/collections/ArrayList"
        ));
    }

    /// Both providers' spellings of the same top-level declaration name the same package.
    #[test]
    fn a_top_level_declaration_names_its_package_under_either_spelling() {
        // The JVM provider's: the file facade a top-level function was compiled into.
        assert!(facade("kotlin/io/ConsoleKt").package_matches("kotlin/io"));
        assert!(facade("kotlin/text/StringsKt").package_matches("kotlin/text"));
        // The klib provider's: the package itself, because a klib has no facades.
        assert!(package("kotlin/io").package_matches("kotlin/io"));
        assert!(package("kotlin/collections").package_matches("kotlin/collections"));
        assert!(package("kotlin").package_matches("kotlin"));
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
    fn string_plus_has_no_name_based_runtime_fallback() {
        assert_eq!(
            runtime_member(function(
                member("java/lang/String"),
                "plus",
                &[Ty::String],
                Ty::String,
            ),),
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

    fn any_declaration(
        name: &str,
        receiver: Option<Ty>,
        params: &[Ty],
        ret: Ty,
    ) -> crate::types::OverriddenDeclaration {
        crate::types::OverriddenDeclaration {
            owner: crate::types::type_name("kotlin/Any"),
            name: name.into(),
            receiver,
            params: params.into(),
            ret,
        }
    }

    #[test]
    fn any_runtime_members_are_keyed_by_exact_declaration_identities() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let to_string = [any_declaration("toString", None, &[], Ty::String)];
        let hash_code = [any_declaration("hashCode", None, &[], Ty::Int)];
        let equals = [any_declaration("equals", None, &[any], Ty::Boolean)];

        // `Any`'s own declarations, by owner and complete signature.
        assert_eq!(
            runtime_member(function(member("kotlin/Any"), "toString", &[], Ty::String)),
            Some("kt_to_string")
        );
        assert_eq!(
            runtime_member(function(member("kotlin/Any"), "hashCode", &[], Ty::Int)),
            Some("kt_hash_code")
        );
        assert_eq!(
            runtime_member(function(
                member("kotlin/Any"),
                "equals",
                &[any],
                Ty::Boolean
            )),
            Some("kt_equals")
        );
        assert_eq!(
            runtime_member(function(member("kotlin/Any"), "hashCode", &[], Ty::String)),
            None,
            "the result is part of Any's declaration signature"
        );

        // Overrides, by the overridden declaration the hierarchy recorded.
        for builder in ["kotlin/text/StringBuilder", "java/lang/StringBuilder"] {
            assert_eq!(
                runtime_member(
                    function(member(builder), "toString", &[], Ty::String)
                        .with_overridden(&to_string)
                ),
                Some("kt_to_string"),
                "the builder's toString overrides Any.toString"
            );
        }
        assert_eq!(
            runtime_member(
                function(member("kotlin/Pair"), "hashCode", &[], Ty::Int)
                    .with_overridden(&hash_code)
            ),
            Some("kt_hash_code")
        );
        assert_eq!(
            runtime_member(
                function(member("sample/Declared"), "equals", &[any], Ty::Boolean)
                    .with_overridden(&equals)
            ),
            Some("kt_equals")
        );

        // The same name and shape without a recorded override of Any's declaration.
        assert_eq!(
            runtime_member(function(
                member("sample/Declared"),
                "equals",
                &[any],
                Ty::Boolean
            )),
            None,
            "a matching shape is no override evidence"
        );
        assert_eq!(
            runtime_member(function(member("kotlin/Int"), "toString", &[], Ty::String)),
            None,
            "a member with no recorded override is not Any.toString"
        );
        let unrelated = [crate::types::OverriddenDeclaration {
            owner: crate::types::type_name("sample/Base"),
            ..any_declaration("toString", None, &[], Ty::String)
        }];
        assert_eq!(
            runtime_member(
                function(member("sample/Declared"), "toString", &[], Ty::String)
                    .with_overridden(&unrelated)
            ),
            None,
            "overriding another classifier's same-shaped declaration is not overriding Any's"
        );
        assert_eq!(
            runtime_member(
                function(facade("sample/DeclaredKt"), "toString", &[], Ty::String)
                    .with_overridden(&[])
            ),
            None,
            "a package function overrides nothing"
        );
        let extension = [any_declaration("toString", Some(Ty::Int), &[], Ty::String)];
        assert_eq!(
            runtime_member(
                function(member("sample/Declared"), "toString", &[], Ty::String)
                    .with_receiver(Some(Ty::Int))
                    .with_overridden(&extension)
            ),
            None,
            "a member extension is not Any.toString"
        );
        assert_eq!(
            runtime_member(function(
                member("kotlin/String"),
                "repeat",
                &[Ty::Int],
                Ty::String
            )),
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
            runtime_function(function(
                facade("kotlin/text/StringsKt"),
                "repeat",
                &[Ty::String, Ty::Int],
                Ty::String,
            )),
            None,
            "a missing runtime function must produce a diagnostic, never a call to a symbol that \
             does not exist"
        );
        assert_eq!(
            runtime_function(function(
                facade("kotlin/io/ConsoleKt"),
                "readLine",
                &[],
                Ty::nullable(Ty::String),
            )),
            None
        );
    }

    #[test]
    fn a_mod_is_answered_only_where_its_operand_width_is_its_result() {
        // Computed at the wider width, which is also the declared result.
        assert_eq!(
            floor_mod(facade("kotlin/NumbersKt"), "mod", Ty::Int, &[Ty::Long]),
            Some(("kt_mod_long", Ty::Long))
        );
        assert_eq!(
            floor_mod(facade("kotlin/NumbersKt"), "mod", Ty::Float, &[Ty::Double],),
            Some(("kt_mod_double", Ty::Double))
        );
        assert_eq!(
            floor_mod(facade("kotlin/NumbersKt"), "mod", Ty::Byte, &[Ty::Byte]),
            Some(("kt_mod_int", Ty::Int))
        );
        // Computed at the receiver's wider width and narrowed after: reading the receiver at the
        // divisor's width would drop its high bits, so these decline.
        assert_eq!(
            floor_mod(facade("kotlin/NumbersKt"), "mod", Ty::Long, &[Ty::Int]),
            None
        );
        assert_eq!(
            floor_mod(facade("kotlin/NumbersKt"), "mod", Ty::Double, &[Ty::Float],),
            None
        );
        // An integer and a floating-point operand meet in neither.
        assert_eq!(
            floor_mod(facade("kotlin/NumbersKt"), "mod", Ty::Int, &[Ty::Double]),
            None
        );
    }

    #[test]
    fn collection_roles_keep_their_special_bridges() {
        let collection = |kind, mutable| crate::types::MappedCollection { kind, mutable };
        assert!(has_special_bridge(
            collection(crate::types::CollectionKind::Collection, false),
            "contains"
        ));
        assert!(has_special_bridge(
            collection(crate::types::CollectionKind::Collection, true),
            "remove"
        ));
        assert!(has_special_bridge(
            collection(crate::types::CollectionKind::Map, false),
            "remove"
        ));
        assert!(!has_special_bridge(
            collection(crate::types::CollectionKind::Collection, false),
            "get"
        ));
    }

    #[test]
    fn a_facade_append_is_vararg_and_only_the_builders_own_append_is_answered() {
        assert_eq!(
            runtime_member(function(
                facade("kotlin/text/StringsKt"),
                "append",
                &[Ty::array(Ty::String)],
                Ty::obj("kotlin/text/StringBuilder"),
            ),),
            None
        );
        assert_eq!(
            runtime_member(function(
                member("kotlin/text/StringBuilder"),
                "append",
                &[Ty::String],
                Ty::obj("kotlin/text/StringBuilder"),
            ),),
            Some("kt_string_builder_append")
        );
    }

    #[test]
    fn a_precondition_is_named_by_one_table_only() {
        for name in ["require", "check"] {
            assert_eq!(
                runtime_function(function(
                    facade("kotlin/PreconditionsKt"),
                    name,
                    &[Ty::Boolean],
                    Ty::Unit,
                )),
                None
            );
            assert!(precondition(facade("kotlin/PreconditionsKt"), name, &[Ty::Boolean]).is_some());
        }
    }

    #[test]
    fn an_inline_overflow_guard_requires_its_complete_declaration_signature() {
        let exact = function(
            package("kotlin/collections"),
            "throwIndexOverflow",
            &[],
            Ty::Unit,
        );
        assert_eq!(
            runtime_function(exact).as_deref(),
            Some("kt_throw_index_overflow")
        );
        assert_eq!(
            runtime_function(function(
                package("kotlin/collections"),
                "throwIndexOverflow",
                &[],
                Ty::Int,
            )),
            None,
            "the result is part of the declaration identity"
        );
        assert_eq!(
            runtime_function(
                function(
                    package("kotlin/collections"),
                    "throwIndexOverflow",
                    &[],
                    Ty::Unit,
                )
                .with_receiver(Some(Ty::obj("sample/Receiver"))),
            ),
            None,
            "a same-shaped extension is not the receiver-less stdlib helper"
        );
    }
}
