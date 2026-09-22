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
//! owner becomes a Kotlin package, which [`declaration_package`] now reads as readily as a
//! facade — both providers exist at once while the klib one grows, so both spellings arrive.

use crate::types::Ty;

/// Undo the JVM provider's mapping of Kotlin built-ins onto their Java counterparts.
///
/// Part of the same temporary bridge as [`declaration_package`]: `kotlin.String` reaches a backend
/// spelled `java/lang/String` because the signatures were read out of a JVM jar. Normalizing here
/// keeps every table below written in Kotlin names, so nothing has to be rewritten when the
/// provider becomes klib-based.
fn kotlin_owner(owner: &str) -> &str {
    match owner {
        "java/lang/String" => "kotlin/String",
        "java/lang/Object" => "kotlin/Any",
        "java/lang/Comparable" => "kotlin/Comparable",
        "java/lang/Number" => "kotlin/Number",
        "java/lang/Boolean" => "kotlin/Boolean",
        "java/lang/Throwable" => "kotlin/Throwable",
        "java/lang/Error" => "kotlin/Error",
        "java/lang/Exception" => "kotlin/Exception",
        "java/lang/RuntimeException" => "kotlin/RuntimeException",
        "java/lang/IllegalStateException" => "kotlin/IllegalStateException",
        "java/lang/IllegalArgumentException" => "kotlin/IllegalArgumentException",
        "java/lang/AssertionError" => "kotlin/AssertionError",
        "java/lang/NullPointerException" => "kotlin/NullPointerException",
        "java/lang/ClassCastException" => "kotlin/ClassCastException",
        "java/lang/IndexOutOfBoundsException" => "kotlin/IndexOutOfBoundsException",
        "java/lang/ArithmeticException" => "kotlin/ArithmeticException",
        "java/lang/UnsupportedOperationException" => "kotlin/UnsupportedOperationException",
        "java/lang/NumberFormatException" => "kotlin/NumberFormatException",
        "java/util/NoSuchElementException" => "kotlin/NoSuchElementException",
        "java/util/ConcurrentModificationException" => "kotlin/ConcurrentModificationException",
        "java/lang/Enum" => "kotlin/Enum",
        // A jar presents Kotlin's `Comparator` as the Java interface it is an alias for.
        "java/util/Comparator" => "kotlin/Comparator",
        // The collections. Kotlin has no `java.util.ArrayList`: `kotlin.collections.ArrayList` is
        // the type, and this spelling is only how a JVM jar presents it.
        "java/util/ArrayList" => "kotlin/collections/ArrayList",
        "java/util/List" => "kotlin/collections/List",
        "java/util/Collection" => "kotlin/collections/Collection",
        "java/util/Iterator" => "kotlin/collections/Iterator",
        "java/lang/Iterable" => "kotlin/collections/Iterable",
        // The tables, on the same footing. `kotlin.collections.HashMap` is a TYPEALIAS to the Java
        // class rather than a mapped builtin, so a jar provider hands over the Java name for it
        // and there is nothing else to normalize it to.
        "java/util/Map" => "kotlin/collections/Map",
        "java/util/HashMap" => "kotlin/collections/HashMap",
        "java/util/LinkedHashMap" => "kotlin/collections/LinkedHashMap",
        "java/util/Map$Entry" => "kotlin/collections/Map$Entry",
        "java/util/Set" => "kotlin/collections/Set",
        "java/util/HashSet" => "kotlin/collections/HashSet",
        "java/util/LinkedHashSet" => "kotlin/collections/LinkedHashSet",
        // The text types, on the same footing. Kotlin has no `java.lang.StringBuilder` and no
        // `java.lang.CharSequence`: `kotlin.text.StringBuilder` and `kotlin.CharSequence` are the
        // types, and these spellings are only how a JVM jar presents them. `AbstractStringBuilder`
        // is where the JVM declares the builder's own members, so it arrives under that name too.
        "java/lang/StringBuilder" | "java/lang/AbstractStringBuilder" => {
            "kotlin/text/StringBuilder"
        }
        "java/lang/CharSequence" => "kotlin/CharSequence",
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
    Some(match kotlin_owner(&owner.render()) {
        "kotlin/Throwable" => "kt_type_throwable",
        "kotlin/Error" => "kt_type_error",
        "kotlin/Exception" => "kt_type_exception",
        "kotlin/RuntimeException" => "kt_type_runtime_exception",
        "kotlin/IllegalStateException" => "kt_type_illegal_state_exception",
        "kotlin/IllegalArgumentException" => "kt_type_illegal_argument_exception",
        "kotlin/NotImplementedError" => "kt_type_not_implemented_error",
        "kotlin/AssertionError" => "kt_type_assertion_error",
        "kotlin/NullPointerException" => "kt_type_null_pointer_exception",
        "kotlin/ClassCastException" => "kt_type_class_cast_exception",
        "kotlin/IndexOutOfBoundsException" => "kt_type_index_out_of_bounds_exception",
        "kotlin/ArithmeticException" => "kt_type_arithmetic_exception",
        "kotlin/UnsupportedOperationException" => "kt_type_unsupported_operation_exception",
        "kotlin/NumberFormatException" => "kt_type_number_format_exception",
        "kotlin/NoSuchElementException" => "kt_type_no_such_element_exception",
        "kotlin/ConcurrentModificationException" => "kt_type_concurrent_modification_exception",
        "kotlin/UninitializedPropertyAccessException" => {
            "kt_type_uninitialized_property_access_exception"
        }
        _ => return None,
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
pub(super) fn has_special_bridge(owner: crate::types::TypeName, name: &str) -> bool {
    let over_a_map = is_map_type(owner) || is_map_entry_type(owner);
    let over_a_collection = is_list_type(owner)
        || is_set_type(owner)
        || matches!(iteration_role(owner), Some(IterationRole::Iterable));
    match name {
        "get" | "remove" | "containsKey" | "containsValue" | "getOrDefault" => over_a_map,
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
    (kotlin_owner(&classifier.render()) == "kotlin/Comparator").then_some("compare")
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
    kotlin_owner(&owner.render()) == "kotlin/Number"
}

/// Is this owner `kotlin.Boolean` itself, whatever the realization spells it?
///
/// `Boolean::not` names the declaration `!b` names, and a realization of it goes out under
/// `java.lang.Boolean`; the receiver's type already says the operand is a `Boolean`, and this is
/// what says the DECLARATION is the builtin's rather than some library extension sharing the name.
pub(super) fn is_boolean_base(owner: &str) -> bool {
    kotlin_owner(owner) == "kotlin/Boolean"
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
        Ty::Obj(owner, _) => match kotlin_owner(&owner.render()) {
            "kotlin/String" => Some(ThrowableMessage::Verbatim),
            // A `cause`, under any name in the hierarchy.
            name if throwable_descriptor(crate::types::type_name(name)).is_some() => None,
            _ => Some(ThrowableMessage::Rendered),
        },
        // `AssertionError(42)` and its siblings: Java gives each width its own overload, and every
        // one of them reports the value's text.
        _ if ty.is_jvm_scalar() => Some(ThrowableMessage::Rendered),
        _ => None,
    }
}

/// The package a TOP-LEVEL declaration belongs to, under either provider's spelling.
///
/// A JVM provider names the file facade the declaration was compiled into
/// (`kotlin/collections/CollectionsKt` for `listOf`), because on the JVM a top-level function IS a
/// static method of that class. A klib names the package itself (`kotlin/collections`): a klib has
/// no facades, they are a JVM artifact, and a non-JVM target should never have had to know about
/// them. So a facade's last segment is dropped and anything else is already the package.
///
/// Every caller asks this of a declaration it knows to be top-level, and compares the answer
/// against a package it names. A member of a real class therefore cannot be mistaken for one: its
/// owner comes back unchanged, and a class's qualified name is never equal to a package's.
fn declaration_package(owner: &str) -> &str {
    match owner.rsplit_once('/') {
        Some((package, facade)) if facade.ends_with("Kt") => package,
        _ => owner,
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

/// The runtime function realizing a selected dependency callable, or `None` when the native
/// runtime does not implement that declaration yet.
pub(super) fn runtime_function(owner: &str, name: &str, params: &[Ty]) -> Option<String> {
    match (declaration_package(kotlin_owner(owner)), name, params) {
        ("kotlin/io", "println", []) => Some("kt_println_unit".to_string()),
        ("kotlin/io", name @ ("print" | "println"), [argument]) => {
            match console_operand(*argument) {
                ConsoleOperand::Scalar(suffix) => Some(format!("kt_{name}_{suffix}")),
                ConsoleOperand::Reference => Some(format!("kt_{name}_any")),
            }
        }
        // The throws a program writes on purpose. Each is a `Nothing` or a check that diverges, so
        // the caller's own bottom-value contract takes over from here; the runtime's part is the
        // diagnosable exit this target gives a throw until it has exceptions.
        //
        // Only the forms that take no `lazyMessage` are named. That parameter is a LAMBDA of an
        // `inline` declaration whose body is not here to splice, and Kotlin lets such a lambda
        // return from the enclosing function — so invoking it as an ordinary function value would
        // be a miscompile rather than a slower answer, and the form declines instead.
        ("kotlin", "TODO", []) => Some("kt_not_implemented".to_string()),
        ("kotlin", "TODO", [_]) => Some("kt_not_implemented_reason".to_string()),
        ("kotlin", "error", [_]) => Some("kt_illegal_state".to_string()),
        ("kotlin", "require", [Ty::Boolean]) => Some("kt_require".to_string()),
        ("kotlin", "check", [Ty::Boolean]) => Some("kt_check".to_string()),
        // `kotlin.math.abs`, one per width it is declared over. The operand crosses at its OWN
        // width and the answer comes back at it: `abs` of a `Long` is a `Long`, and computing it
        // at any other width would change what the minimum answers.
        ("kotlin/math", "abs", [Ty::Int]) => Some("kt_abs_int".to_string()),
        ("kotlin/math", "abs", [Ty::Long]) => Some("kt_abs_long".to_string()),
        ("kotlin/math", "abs", [Ty::Float]) => Some("kt_abs_float".to_string()),
        ("kotlin/math", "abs", [Ty::Double]) => Some("kt_abs_double".to_string()),
        // The overflow guard `forEachIndexed` and its relatives carry. A jar provider presents
        // those as INLINE declarations, so their bodies are spliced into the caller and this call
        // comes with them; a klib provider answers the walk itself and never mentions it. Kotlin's
        // own is `throw ArithmeticException("Index overflow has happened.")`.
        ("kotlin/collections", "throwIndexOverflow", []) => {
            Some("kt_throw_index_overflow".to_string())
        }
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
    owner: &str,
    name: &str,
    params: &[Ty],
) -> Option<(&'static str, usize)> {
    if declaration_package(kotlin_owner(owner)) != "kotlin/test" {
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

/// The scope functions that have NO receiver to arrive on: `run { … }` and `with(x) { … }`.
///
/// The same rearrangement as the member ones beside them, reached by the other path because these
/// take their subject as an argument rather than as a receiver. Like the others they are `inline`,
/// so what arrives here is the shape with no body to splice — a block that is an ordinary function
/// value — which is exactly what a klib-backed compilation produces for every one of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TopLevelScope {
    /// `run(block: () -> R): R` — the block takes nothing.
    Block,
    /// `with(receiver: T, block: T.() -> R): R` — the block takes the value written before it.
    WithReceiver,
}

/// Which of the two a selected top-level declaration is, or `None` for anything else.
///
/// Keyed on the SHAPE as well as the name, because `kotlin.run` is two declarations: this one and
/// `T.run(block: T.() -> R): R`, which has a receiver and is handled as a member.
pub(super) fn top_level_scope(owner: &str, name: &str, params: &[Ty]) -> Option<TopLevelScope> {
    if declaration_package(kotlin_owner(owner)) != "kotlin" {
        return None;
    }
    match (name, params) {
        ("run", [Ty::Fun(_)]) => Some(TopLevelScope::Block),
        ("with", [_, Ty::Fun(_)]) => Some(TopLevelScope::WithReceiver),
        _ => None,
    }
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
pub(super) fn is_assert_fails_with(owner: &str, name: &str, params: &[Ty]) -> bool {
    if declaration_package(kotlin_owner(owner)) != "kotlin/test" {
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
pub(super) fn builder_scope(owner: &str, name: &str, params: &[Ty]) -> Option<Builder> {
    let builder = match (declaration_package(kotlin_owner(owner)), name) {
        ("kotlin/text", "buildString") => Builder::Text,
        ("kotlin/collections", "buildList") => Builder::List,
        _ => return None,
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
pub(super) fn precondition(owner: &str, name: &str, params: &[Ty]) -> Option<Precondition> {
    if declaration_package(kotlin_owner(owner)) != "kotlin" {
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
        Ty::Obj(owner, _) => kotlin_owner(&owner.render()) == "kotlin/String",
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
pub(super) fn float_predicate(owner: &str, name: &str) -> Option<FloatPredicate> {
    // Kotlin declares each of these TWICE: as a member of the primitive (`Double.isNaN()`) and as
    // an extension on it in the numbers facade. Which spelling reaches a backend is the provider's
    // choice, not the program's, so both are read here.
    //
    // Only the facade one was, and the package helper used to answer `None` for an owner not ending
    // in `Kt` — so an owner of `kotlin/Double` fell through and the call declined by name. The
    // member spelling is the one the corpus actually produces.
    let owner = kotlin_owner(owner);
    let declared_here =
        matches!(owner, "kotlin/Double" | "kotlin/Float") || declaration_package(owner) == "kotlin";
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
/// Every enum constant answers `name` and `ordinal` from the storage its base contributes, and the
/// accessor reaches a backend spelled as the provider named it — `getName` on `java/lang/Enum`,
/// since the signatures come from a JVM jar. Normalizing that here keeps the one place that knows
/// the spelling the same one that knows every other.
pub(super) fn enum_member(owner: &str, accessor: &str) -> Option<&'static str> {
    if kotlin_owner(owner) != "kotlin/Enum" {
        return None;
    }
    match accessor {
        "name" => Some("name"),
        "ordinal" => Some("ordinal"),
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
    if declaration_package(kotlin_owner(owner)) != "kotlin" {
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
/// Recognized here rather than where ranges are lowered, for the same reason [`declaration_package`]
/// lives here: the facade is the JVM provider's spelling, not Kotlin's.
pub(super) fn is_range_until(owner: &str, name: &str, arity: usize) -> bool {
    is_ranges_facade(owner, name, arity, "until")
}

/// Whether a declaration is `downTo` on the ranges facade — the descending counterpart of `until`,
/// and built the same way: an extension of the facade rather than a member of what it answers.
pub(super) fn is_range_down_to(owner: &str, name: &str, arity: usize) -> bool {
    is_ranges_facade(owner, name, arity, "downTo")
}

/// Whether a declaration is `step` on the ranges facade. Its receiver is the range or progression
/// being stepped, so unlike `until` and `downTo` the RECEIVER says which width this is.
pub(super) fn is_range_step(owner: &str, name: &str, arity: usize) -> bool {
    is_ranges_facade(owner, name, arity, "step")
}

/// Whether a declaration is `reversed` on the ranges facade.
pub(super) fn is_range_reversed(owner: &str, name: &str, arity: usize) -> bool {
    declaration_package(kotlin_owner(owner)) == "kotlin/ranges" && name == "reversed" && arity == 0
}

fn is_ranges_facade(owner: &str, name: &str, arity: usize, wanted: &str) -> bool {
    declaration_package(kotlin_owner(owner)) == "kotlin/ranges" && name == wanted && arity == 1
}

/// Whether a getter is `KCallable.name` — the one member of the reflection surface whose answer a
/// program can have without any reflection metadata existing, because the declaration it names is
/// written in the same file.
pub(super) fn is_callable_name(owner: crate::types::TypeName, name: &str) -> bool {
    name == "name"
        && [
            "kotlin/reflect/KCallable",
            "kotlin/reflect/KFunction",
            "kotlin/reflect/KProperty",
        ]
        .iter()
        .any(|candidate| owner.matches(candidate))
}

/// Whether a declaration's owner is where the stdlib's delegate operators on a property reference
/// live. `kotlin.getValue`/`kotlin.setValue` are top-level extensions of `KProperty0`/`KProperty1`,
/// so a JVM provider presents them on the file facade `PropertyReferenceDelegates.kt` compiles to
/// and a klib provider, which has no facades, presents them on the package itself.
///
/// The bare package `kotlin` is admitted even though `Lazy.getValue` is also a top-level `kotlin`
/// declaration of that name, because the owner is not what tells them apart: only a call whose
/// RECEIVER is one of the four property-reference types reaches these slots, and the caller checks
/// that before asking anything else.
pub(super) fn is_property_delegates_facade(owner: &str) -> bool {
    matches!(
        kotlin_owner(owner),
        "kotlin/PropertyReferenceDelegatesKt" | "kotlin/properties" | "kotlin"
    )
}

/// Whether a declaration's owner is the file facade `lazy` and `Lazy.getValue` live in. Both are
/// top-level declarations of `kotlin`, so they reach a backend as members of `kotlin/LazyKt`.
pub(super) fn is_lazy_facade(owner: &str) -> bool {
    matches!(kotlin_owner(owner), "kotlin/LazyKt" | "kotlin")
}

/// Whether a declaration's owner is the file facade `to` lives in. `kotlin.to` is a top-level
/// extension, so it reaches a backend as a member of `kotlin/TuplesKt` — the `kotlin/io/ConsoleKt`
/// situation again, normalized in the same place.
pub(super) fn is_tuples_facade(owner: &str) -> bool {
    matches!(kotlin_owner(owner), "kotlin/TuplesKt" | "kotlin")
}

/// Whether a declaration's owner is the collections file facade `listOf` and its neighbours live
/// in. They are top-level functions of `kotlin.collections`, so they reach a backend as members of
/// the facade class the stdlib declares them in — the `kotlin/io/ConsoleKt` situation again, and
/// normalized in the same place.
pub(super) fn is_collections_facade(owner: &str) -> bool {
    declaration_package(kotlin_owner(owner)) == "kotlin/collections"
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
///
/// The concrete ranges are `Iterable` here as much as the interfaces are, because the runtime's own
/// `kt_iterable_*` walk dispatches on the DESCRIPTOR and reaches a range as readily as a list. They
/// are safe to name for the same reason the interfaces are: a file declaring a class that extends
/// one of them overrides a dependency method and is declined whole. A member that must not be
/// answered this way — one whose result depends on the receiver being a list — is read by
/// `list_symbol` behind an `is_list` check, never through this.
pub(super) fn iteration_role(internal: crate::types::TypeName) -> Option<IterationRole> {
    // The table below is written in Kotlin names; a JVM jar's spelling is normalized to one first.
    let internal = crate::types::type_name(kotlin_owner(&internal.render()));
    [
        ("kotlin/collections/Iterable", IterationRole::Iterable),
        ("kotlin/collections/Collection", IterationRole::Iterable),
        ("kotlin/collections/List", IterationRole::Iterable),
        // The growable list. It is iterated exactly as a read-only list is: the runtime hands out
        // the one list iterator, whose cursor is an index and whose bound is `kt_list_size`, which
        // both shapes answer.
        (
            "kotlin/collections/MutableIterable",
            IterationRole::Iterable,
        ),
        (
            "kotlin/collections/MutableCollection",
            IterationRole::Iterable,
        ),
        ("kotlin/collections/MutableList", IterationRole::Iterable),
        ("kotlin/collections/ArrayList", IterationRole::Iterable),
        (
            "kotlin/collections/MutableIterator",
            IterationRole::Iterator,
        ),
        // The SETS. A set is walked as the list of its elements, which is its insertion order —
        // and a `Map` is walked as its ENTRIES, which is what Kotlin's `Map.iterator()` extension
        // answers, so both reach the same descriptor dispatch a list does.
        ("kotlin/collections/Set", IterationRole::Iterable),
        ("kotlin/collections/MutableSet", IterationRole::Iterable),
        ("kotlin/collections/HashSet", IterationRole::Iterable),
        ("kotlin/collections/LinkedHashSet", IterationRole::Iterable),
        ("kotlin/collections/Map", IterationRole::Iterable),
        ("kotlin/collections/MutableMap", IterationRole::Iterable),
        ("kotlin/collections/HashMap", IterationRole::Iterable),
        ("kotlin/collections/LinkedHashMap", IterationRole::Iterable),
        // A SEQUENCE. Iterating one is the one member `Sequence` declares, and the wrapper the
        // runtime makes holds the source it walks — so the role is the same and the dispatch is
        // the descriptor's. Which OTHER members a sequence may be asked is narrower than an
        // iterable's, and that is [`lists::is_sequence`]'s to enforce, not this table's.
        ("kotlin/sequences/Sequence", IterationRole::Iterable),
        ("kotlin/ranges/IntRange", IterationRole::Iterable),
        ("kotlin/ranges/LongRange", IterationRole::Iterable),
        ("kotlin/ranges/CharRange", IterationRole::Iterable),
        // A PROGRESSION is a walk with a step, and `10 downTo 1` is typed by one rather than by
        // the range above it. The runtime needs nothing new for it: one struct serves a range and
        // a progression — a plain range is the one whose step is 1 — and the object a `downTo`
        // builds wears the very descriptor a range does, so every `kt_iterable_*` walk already
        // reaches it. What was missing is only the STATIC name, which is what a call site has.
        ("kotlin/ranges/IntProgression", IterationRole::Iterable),
        ("kotlin/ranges/LongProgression", IterationRole::Iterable),
        ("kotlin/ranges/CharProgression", IterationRole::Iterable),
        // The unsigned pair the runtime owns, and their progressions. Kotlin declares exactly two
        // unsigned ranges, since `UByte.rangeTo` and `UShort.rangeTo` both answer a `UIntRange`.
        ("kotlin/ranges/UIntRange", IterationRole::Iterable),
        ("kotlin/ranges/ULongRange", IterationRole::Iterable),
        ("kotlin/ranges/UIntProgression", IterationRole::Iterable),
        ("kotlin/ranges/ULongProgression", IterationRole::Iterable),
        ("kotlin/collections/Iterator", IterationRole::Iterator),
        // The primitive iterators an array hands out. Each is a concrete stdlib class rather than
        // an interface, and naming them is safe for the reason the interfaces are: the only objects
        // wearing one here are the runtime's own walks, and a file declaring its own subclass of
        // one overrides a dependency method and is declined whole. `IntIterator`, `LongIterator`
        // and `CharIterator` are deliberately ABSENT — a range's iterator wears those, and they are
        // read by the narrow protocol before this is consulted at all.
        ("kotlin/collections/ByteIterator", IterationRole::Iterator),
        ("kotlin/collections/ShortIterator", IterationRole::Iterator),
        (
            "kotlin/collections/BooleanIterator",
            IterationRole::Iterator,
        ),
        ("kotlin/collections/FloatIterator", IterationRole::Iterator),
        ("kotlin/collections/DoubleIterator", IterationRole::Iterator),
    ]
    .into_iter()
    .find_map(|(candidate, role)| internal.matches(candidate).then_some(role))
}

/// Which role a TYPE plays, including the two the runtime walks that are not `Iterable` at all.
///
/// Neither an array nor a `CharSequence` is a `kotlin.collections.Iterable`, and Kotlin still lets
/// a program reach every `Iterable` member on one, through an extension declared for it. The
/// runtime walks both — an array's element at its own width, a string's by UTF-16 unit — so the
/// role is what matters at a call site and the interface list is not.
pub(super) fn iteration_role_of(ty: Ty) -> Option<IterationRole> {
    let ty = ty.non_null();
    if ty.is_array() {
        return Some(IterationRole::Iterable);
    }
    let internal = ty.obj_internal()?;
    // The BUILDER is walkable text too. `for (c in StringBuilder("OK"))` is Kotlin's own, and the
    // walk is the same one a string takes: the chars iterator's bound is `kt_string_length`, which
    // answers for either shape — and re-reads it every step, which is what lets a loop that
    // shortens the builder stop where Kotlin's stops.
    //
    // Read through [`kotlin_owner`] rather than by listing both providers' spellings, which is
    // what that function is for: a jar hands over `java.lang.StringBuilder` for a type Kotlin
    // calls `kotlin.text.StringBuilder`, and normalizing once beats a list that has to grow.
    if matches!(
        kotlin_owner(&internal.render()),
        "kotlin/String" | "kotlin/CharSequence" | "kotlin/text/StringBuilder"
    ) {
        return Some(IterationRole::Iterable);
    }
    iteration_role(internal)
}

/// Whether a type name is a list the native runtime builds.
pub(super) fn is_list_type(internal: crate::types::TypeName) -> bool {
    matches!(
        kotlin_owner(&internal.render()),
        "kotlin/collections/List"
            | "kotlin/collections/Collection"
            // The MUTABLE ones read the same way: every question `List` answers, a `MutableList`
            // answers identically, and the runtime gives both one entry point.
            | "kotlin/collections/MutableList"
            | "kotlin/collections/MutableCollection"
            | "kotlin/collections/ArrayList"
    )
}

/// Whether a type name is one an `is` answers with the runtime's LIST marker.
///
/// Narrower than [`is_list_type`], which also admits `Collection` because every question a list
/// answers a collection answers the same way. A CHECK cannot take that: a set is a `Collection` and
/// wears no list marker, so answering `x is Collection<*>` from it would say `false` of an object
/// that is one. Only the spellings every object wearing the marker really is.
pub(super) fn is_list_check_type(internal: crate::types::TypeName) -> bool {
    matches!(
        kotlin_owner(&internal.render()),
        "kotlin/collections/List"
            | "kotlin/collections/MutableList"
            | "kotlin/collections/ArrayList"
    )
}

/// The growable list the runtime provides, if this names one.
///
/// `kotlin.collections.ArrayList` is declared in no file krusty compiles, so constructing one takes
/// the path `Any()` and the throwables already take: the runtime allocates it.
pub(super) fn is_array_list(internal: crate::types::TypeName) -> bool {
    kotlin_owner(&internal.render()) == "kotlin/collections/ArrayList"
}

/// The growable MAP or SET the runtime provides, if this names one, as the runtime's suffix.
///
/// Declared in no file krusty compiles, so constructing one takes the path `Any()`, the throwables
/// and `ArrayList` already take: the runtime allocates it. The ordered and unordered spellings are
/// one object here — the map this runtime builds is insertion-ordered, and an unordered one leaves
/// its order unspecified, of which insertion order is one.
pub(super) fn runtime_table(internal: crate::types::TypeName) -> Option<&'static str> {
    match kotlin_owner(&internal.render()) {
        "kotlin/collections/HashMap" | "kotlin/collections/LinkedHashMap" => Some("map"),
        "kotlin/collections/HashSet" | "kotlin/collections/LinkedHashSet" => Some("set"),
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
    owner: &str,
    name: &str,
    params: &[Ty],
) -> Option<(&'static str, Ty, Ty)> {
    if declaration_package(kotlin_owner(owner)) != "kotlin" || name != "fromBits" {
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
    owner: &str,
    name: &str,
    params: &[Ty],
    receiver: Ty,
) -> Option<(&'static str, Ty)> {
    if declaration_package(kotlin_owner(owner)) != "kotlin" || !params.is_empty() {
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
pub(super) fn is_comparable_compare_to(owner: &str, name: &str, params: &[Ty]) -> bool {
    kotlin_owner(owner) == "kotlin/Comparable" && name == "compareTo" && params.len() == 1
}

/// Whether a type name is `kotlin.Comparable` or the base whose comparison an enum inherits.
///
/// Both are what a DECLARED class naming one of them makes observable: an object of the program's
/// standing behind a `Comparable` receiver, which the runtime's descriptor tables cannot order.
pub(super) fn is_comparable_supertype(internal: crate::types::TypeName) -> bool {
    matches!(
        kotlin_owner(&internal.render()),
        "kotlin/Comparable" | "kotlin/Enum"
    )
}

/// Whether a type name is the SEQUENCE the native runtime makes.
pub(super) fn is_sequence_type(internal: crate::types::TypeName) -> bool {
    kotlin_owner(&internal.render()) == "kotlin/sequences/Sequence"
}

/// Whether a type name is the MAP the native runtime builds.
///
/// `MutableMap`, `HashMap` and `LinkedHashMap` are one object there: the map that runtime builds is
/// growable and insertion-ordered, which satisfies all three — the unordered spellings leave their
/// order unspecified, and insertion order is one of the orders left unspecified.
pub(super) fn is_map_type(internal: crate::types::TypeName) -> bool {
    matches!(
        kotlin_owner(&internal.render()),
        "kotlin/collections/Map"
            | "kotlin/collections/MutableMap"
            | "kotlin/collections/HashMap"
            | "kotlin/collections/LinkedHashMap"
    )
}

/// Whether a type name is the SET the native runtime builds — the same four spellings, one level
/// down.
pub(super) fn is_set_type(internal: crate::types::TypeName) -> bool {
    matches!(
        kotlin_owner(&internal.render()),
        "kotlin/collections/Set"
            | "kotlin/collections/MutableSet"
            | "kotlin/collections/HashSet"
            | "kotlin/collections/LinkedHashSet"
    )
}

/// Whether a type name is a map ENTRY, which `entries` hands out and a destructuring reads.
pub(super) fn is_map_entry_type(internal: crate::types::TypeName) -> bool {
    matches!(
        kotlin_owner(&internal.render()),
        "kotlin/collections/Map$Entry" | "kotlin/collections/MutableMap$MutableEntry"
    )
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

/// The shape a type NAME belongs to. The narrow kinds are asked first: every one of them is an
/// `Iterable` by [`iteration_role`], which walks a map as its entries and a sequence as its source,
/// and that role is about walking rather than about which objects are interchangeable.
pub(super) fn collection_shape(internal: crate::types::TypeName) -> Option<CollectionShape> {
    if is_map_entry_type(internal) {
        return Some(CollectionShape::MapEntry);
    }
    if is_map_type(internal) {
        return Some(CollectionShape::Map);
    }
    if is_sequence_type(internal) {
        return Some(CollectionShape::Sequence);
    }
    if is_list_type(internal) || is_set_type(internal) {
        return Some(CollectionShape::Iterable);
    }
    // TEXT is walkable here and is no collection at all, so [`iteration_role`] does not name it —
    // [`iteration_role_of`] reaches it from the TYPE instead.
    if matches!(
        kotlin_owner(&internal.render()),
        "kotlin/String" | "kotlin/CharSequence" | "kotlin/text/StringBuilder"
    ) {
        return Some(CollectionShape::Text);
    }
    match iteration_role(internal)? {
        IterationRole::Iterable => Some(CollectionShape::Iterable),
        IterationRole::Iterator => Some(CollectionShape::Iterator),
    }
}

/// The shape a TYPE belongs to. An ARRAY names no collection type and is walkable all the same,
/// which is why this takes a type where [`collection_shape`] takes a name — the same split
/// [`iteration_role_of`] makes over [`iteration_role`].
pub(super) fn collection_shape_of(ty: Ty) -> Option<CollectionShape> {
    let ty = ty.non_null();
    if ty.is_array() {
        return Some(CollectionShape::Iterable);
    }
    collection_shape(ty.obj_internal()?)
}

/// A qualified name with the FILE FACADE a nested class is qualified by removed, or `None` when it
/// names no facade.
///
/// `castAnonymousClassKt$box$1` is the JVM's binary name for an anonymous object inside a top-level
/// `box`, and it is right there — but there is no facade class on the native target at all: a
/// top-level property is a global and a top-level function is a symbol, neither owned by anything.
/// Kotlin/Native names that object `box$1`.
///
/// A facade is recognized by its `Kt` suffix, which is the same test [`declaration_package`] makes for
/// the same reason. That spelling is a JVM provider detail, which is why the test lives here.
pub(super) fn without_file_facade(qualified: &str) -> Option<String> {
    let (package, tail) = match qualified.rfind('.') {
        Some(at) => (&qualified[..=at], &qualified[at + 1..]),
        None => ("", qualified),
    };
    let (facade, nested) = tail.split_once('$')?;
    (facade.ends_with("Kt") && !nested.is_empty()).then(|| format!("{package}{nested}"))
}

/// Whether this names `kotlin.CharSequence`, under either spelling a provider may hand over.
///
/// It wears a runtime descriptor for the reason `Number` and `Comparable` do: no instances of its
/// own, and both the string and the builder point at it, so a cast or an `is` against it has
/// something to compare.
pub(super) fn is_char_sequence(internal: crate::types::TypeName) -> bool {
    ["kotlin/CharSequence", "java/lang/CharSequence"]
        .iter()
        .any(|candidate| internal.matches(candidate))
}

/// The string builder the runtime provides, if this names one.
///
/// Like [`is_array_list`], `kotlin.text.StringBuilder` is declared in no file krusty compiles, so
/// constructing one is the runtime's job rather than the generator's.
pub(super) fn is_string_builder(internal: crate::types::TypeName) -> bool {
    kotlin_owner(&internal.render()) == "kotlin/text/StringBuilder"
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
pub(super) fn is_indices(owner: &str, name: &str) -> bool {
    name == "indices"
        && matches!(
            declaration_package(kotlin_owner(owner)),
            "kotlin/collections" | "kotlin/text"
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
    // `kotlin.text`'s top-level extensions on `String`, which reach a backend as members of that
    // package's file facade — the `kotlin/io/ConsoleKt` situation, read through the same helper so
    // a facade kotlinc split in two (`StringsKt__StringsKt`) is the same answer.
    if declaration_package(kotlin_owner(owner)) == "kotlin/text" {
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
    match (kotlin_owner(owner), name, params) {
        // `s[i]`. It arrives under EITHER name for the reason [`kotlin_owner`] exists: a mapped
        // builtin whose realization names a different physical member hands over that physical
        // name, and `kotlin.CharSequence.get` is realized as `java.lang.CharSequence.charAt`. The
        // Kotlin spelling still reaches here from a source that did not go through a realization,
        // so both are the same member rather than one replacing the other.
        (
            "kotlin/String" | "kotlin/CharSequence" | "kotlin/text/StringBuilder",
            "get" | "charAt",
            [Ty::Int],
        ) => Some(("kt_string_get", vec![reference, Ty::Int], Ty::Char)),
        // `sb.setLength(n)` counts UTF-16 units, so the operand is an `Int` the generator must not
        // box to hand over. It answers nothing, which is why it is not one of the builder's
        // reference-carried members below.
        ("kotlin/text/StringBuilder", "setLength", [Ty::Int]) => Some((
            "kt_string_builder_set_length",
            vec![reference, Ty::Int],
            Ty::Unit,
        )),
        // `s.subSequence(a, b)` is `s.substring(a, b)`; the return type only says less about the
        // result, which the call site already knows.
        ("kotlin/String" | "kotlin/CharSequence", "subSequence", [Ty::Int, Ty::Int]) => Some((
            "kt_string_substring",
            vec![reference, Ty::Int, Ty::Int],
            Ty::obj("kotlin/String"),
        )),
        // `isEmpty` and its three relatives are INLINE extensions in `kotlin.text`, so a jar
        // provider presents them as members of the receiver's own type rather than of the text
        // facade — the same declaration under a second spelling, exactly as `charAt` is `get`.
        ("kotlin/String" | "kotlin/CharSequence" | "kotlin/text/StringBuilder", "isEmpty", []) => {
            Some(("kt_string_is_empty", vec![reference], Ty::Boolean))
        }
        (
            "kotlin/String" | "kotlin/CharSequence" | "kotlin/text/StringBuilder",
            "isNotEmpty",
            [],
        ) => Some(("kt_string_is_not_empty", vec![reference], Ty::Boolean)),
        ("kotlin/String" | "kotlin/CharSequence" | "kotlin/text/StringBuilder", "isBlank", []) => {
            Some(("kt_string_is_blank", vec![reference], Ty::Boolean))
        }
        (
            "kotlin/String" | "kotlin/CharSequence" | "kotlin/text/StringBuilder",
            "isNotBlank",
            [],
        ) => Some(("kt_string_is_not_blank", vec![reference], Ty::Boolean)),
        // The ANSWER is an `Int`, so this cannot go through the reference-carried member path
        // below: `a < b` would box the very comparison it is asking about.
        ("kotlin/String", "compareTo", [_]) => {
            Some(("kt_string_compare_to", vec![reference, reference], Ty::Int))
        }
        // `kotlin.Number`'s six conversions. A site that could type its value only as a `Number`
        // hands over a box, and which primitive is inside is the descriptor's answer — so the
        // runtime reads it rather than the generator guessing from the static type.
        //
        // Each arrives under EITHER name, for the reason [`kotlin_owner`] exists and exactly as
        // `kotlin.CharSequence.get`/`java.lang.CharSequence.charAt` does: a mapped builtin whose
        // realization names a different physical member hands over that physical name. Observed:
        // `toByte`/`toShort` come through as `byteValue`/`shortValue` while `toInt`/`toLong` keep
        // the Kotlin spelling, so neither list is the one to write alone. The two spellings are the
        // SAME member, not one replacing the other.
        ("kotlin/Number", "toByte" | "byteValue", []) => {
            Some(("kt_number_to_byte", vec![reference], Ty::Byte))
        }
        ("kotlin/Number", "toShort" | "shortValue", []) => {
            Some(("kt_number_to_short", vec![reference], Ty::Short))
        }
        ("kotlin/Number", "toInt" | "intValue", []) => {
            Some(("kt_number_to_int", vec![reference], Ty::Int))
        }
        ("kotlin/Number", "toLong" | "longValue", []) => {
            Some(("kt_number_to_long", vec![reference], Ty::Long))
        }
        ("kotlin/Number", "toFloat" | "floatValue", []) => {
            Some(("kt_number_to_float", vec![reference], Ty::Float))
        }
        ("kotlin/Number", "toDouble" | "doubleValue", []) => {
            Some(("kt_number_to_double", vec![reference], Ty::Double))
        }
        _ => None,
    }
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
    owner: &str,
    name: &str,
    params: &[Ty],
) -> Option<&'static str> {
    let owner = kotlin_owner(owner);
    if declaration_package(owner) != "kotlin/text"
        && !matches!(owner, "kotlin/String" | "kotlin/CharSequence")
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
    if !matches!(*text, Ty::Obj(named, _) if is_char_sequence(named) || named.matches("kotlin/String"))
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
pub(super) fn boxed_step(owner: &str, name: &str, params: &[Ty]) -> Option<(Ty, i64)> {
    if !params.is_empty() {
        return None;
    }
    let step = match name {
        "inc" => 1,
        "dec" => -1,
        _ => return None,
    };
    let ty = match owner {
        "java/lang/Byte" | "kotlin/Byte" => Ty::Byte,
        "java/lang/Short" | "kotlin/Short" => Ty::Short,
        "java/lang/Integer" | "kotlin/Int" => Ty::Int,
        "java/lang/Long" | "kotlin/Long" => Ty::Long,
        "java/lang/Character" | "kotlin/Char" => Ty::Char,
        "java/lang/Float" | "kotlin/Float" => Ty::Float,
        "java/lang/Double" | "kotlin/Double" => Ty::Double,
        _ => return None,
    };
    Some((ty, step))
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

/// A dependency type's KOTLIN name, whichever spelling a provider presented it under.
///
/// `java.lang.CharSequence` and `kotlin.CharSequence` are one type; which one a call carries is
/// the provider's business, so anything keyed on the type has to ask for the Kotlin name first.
pub(super) fn kotlin_name_of(owner: crate::types::TypeName) -> String {
    kotlin_owner(&owner.render()).to_string()
}

/// Whether a name is one of Kotlin's function types (`kotlin.Function0`..`Function22`, or the
/// arity-less `kotlin.Function` they all extend).
///
/// The one dependency type whose member this target gives a FIXED slot: a function value's
/// `invoke` sits right after `kotlin.Any`'s three, and the runtime names that number itself.
pub(super) fn is_function_type_name(owner: crate::types::TypeName) -> bool {
    let rendered = kotlin_owner(&owner.render()).to_string();
    let Some(suffix) = rendered.strip_prefix("kotlin/Function") else {
        return false;
    };
    suffix.is_empty() || suffix.chars().all(|digit| digit.is_ascii_digit())
}

/// The runtime marker an `is` against a FUNCTION TYPE asks about, for a type that names one.
///
/// A function value is an object of a type of its own — one per lambda and per callable reference —
/// so the type written at the site is never the object's. The markers stand in for it: each
/// function value's descriptor names its arity's and the bare `kotlin.Function` beside it. A
/// `suspend` function type spells the same names and is left to the paths that read it, which
/// decline before reaching here.
pub(super) fn function_type_descriptor(owner: crate::types::TypeName) -> Option<&'static str> {
    const ARITIES: [&str; 23] = [
        "kt_type_function0",
        "kt_type_function1",
        "kt_type_function2",
        "kt_type_function3",
        "kt_type_function4",
        "kt_type_function5",
        "kt_type_function6",
        "kt_type_function7",
        "kt_type_function8",
        "kt_type_function9",
        "kt_type_function10",
        "kt_type_function11",
        "kt_type_function12",
        "kt_type_function13",
        "kt_type_function14",
        "kt_type_function15",
        "kt_type_function16",
        "kt_type_function17",
        "kt_type_function18",
        "kt_type_function19",
        "kt_type_function20",
        "kt_type_function21",
        "kt_type_function22",
    ];
    let rendered = kotlin_owner(&owner.render()).to_string();
    let suffix = rendered.strip_prefix("kotlin/Function")?;
    if suffix.is_empty() {
        return Some("kt_type_function");
    }
    ARITIES.get(suffix.parse::<usize>().ok()?).copied()
}

/// The runtime marker an `is` against one of Kotlin's REFLECTION types asks about.
///
/// A property reference is an object of a type of its own — the generator emits one per property —
/// so the type written at the site is never the object's, exactly as for a function value. These
/// markers are what the two have in common.
pub(super) fn reflection_type_descriptor(owner: crate::types::TypeName) -> Option<&'static str> {
    Some(match kotlin_owner(&owner.render()) {
        "kotlin/reflect/KCallable" => "kt_type_kcallable",
        "kotlin/reflect/KProperty" => "kt_type_kproperty",
        "kotlin/reflect/KProperty0" => "kt_type_kproperty0",
        "kotlin/reflect/KProperty1" => "kt_type_kproperty1",
        "kotlin/reflect/KProperty2" => "kt_type_kproperty2",
        "kotlin/reflect/KMutableProperty" => "kt_type_kmutable_property",
        "kotlin/reflect/KMutableProperty0" => "kt_type_kmutable_property0",
        "kotlin/reflect/KMutableProperty1" => "kt_type_kmutable_property1",
        "kotlin/reflect/KMutableProperty2" => "kt_type_kmutable_property2",
        _ => return None,
    })
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

/// Whether a classifier is `kotlin.properties.ReadOnlyProperty`, which this runtime never builds —
/// so every object behind the type is a class of the file being compiled.
pub(super) fn is_read_only_property_name(owner: crate::types::TypeName) -> bool {
    owner.matches("kotlin/properties/ReadOnlyProperty")
}

/// Whether a classifier is `kotlin.Pair`, which the runtime owns rather than any file declaring.
pub(super) fn is_pair_name(owner: crate::types::TypeName) -> bool {
    owner.matches("kotlin/Pair")
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
    owner: &str,
    name: &str,
    params: &[Ty],
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    // The unsigned arrays get their own package: Kotlin declares `UIntArray.reversed()` in
    // `kotlin.collections.unsigned`, apart from the signed one it answers identically to. The
    // runtime reads the element's type from the array's descriptor, so both reach one entry.
    let package = declaration_package(kotlin_owner(owner));
    if !matches!(
        package,
        "kotlin/collections" | "kotlin/collections/unsigned"
    ) {
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
        ("contentToString", []) => (
            "kt_array_content_to_string",
            vec![reference],
            Ty::obj("kotlin/String"),
        ),
        _ => return None,
    })
}

/// `a.mod(b)` — the remainder carrying the DIVISOR's sign, as (runtime symbol, operand type).
///
/// Kotlin declares one for every numeric pair, and the RESULT names the type they meet in:
/// `Int.mod(Long)` answers a `Long`, so both operands are read at that width. The narrow integers
/// answer at their own width and are computed at `Int`, which is exact — the answer's magnitude is
/// below the divisor's, so nothing is lost on the way back down.
///
/// Not [`scalar_member`]: that table hands its receiver over as a reference, and the receiver here
/// is a number.
pub(super) fn floor_mod(owner: &str, name: &str, params: &[Ty]) -> Option<(&'static str, Ty)> {
    if declaration_package(kotlin_owner(owner)) != "kotlin" || name != "mod" {
        return None;
    }
    // A single numeric operand is the whole of the declaration set. `Char` is absent because
    // Kotlin declares no `mod` for it, and a `Unit` or reference argument names something else.
    match params {
        [Ty::Byte | Ty::Short | Ty::Int] => Some(("kt_mod_int", Ty::Int)),
        [Ty::Long] => Some(("kt_mod_long", Ty::Long)),
        [Ty::Float] => Some(("kt_mod_float", Ty::Float)),
        [Ty::Double] => Some(("kt_mod_double", Ty::Double)),
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
pub(super) fn experimental_bitwise(owner: &str, name: &str, params: &[Ty]) -> Option<BitwiseOp> {
    if declaration_package(kotlin_owner(owner)) != "kotlin/experimental" {
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
pub(super) fn unsigned_conversion(owner: &str, name: &str) -> Option<Ty> {
    // A top-level extension of `kotlin`, under either provider's spelling: a JVM provider names
    // the file facade kotlinc split them across, a klib names the package. The declarations these
    // four names can denote in `kotlin` are exactly these, so the package is discrimination
    // enough; `UInt.toUInt()` is a MEMBER of its own type and answers `kotlin/UInt`, which is not
    // this package and is handled by `unsigned_owner`.
    if declaration_package(kotlin_owner(owner)) != "kotlin" {
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

/// Whether an accessor is TEXT's `length` — `String`'s, `CharSequence`'s or a builder's.
///
/// One question about the same thing, and the runtime answers all three from one place (see
/// `kt_text_of`) — including for text the PROGRAM declared, whose own `length` the descriptor
/// records. `String.length` reaches here only through an ACCESSOR: written in source it is a
/// compiler-supplied operation, because the frontend recognizes the form, and a synthesized
/// accessor is a call.
pub(super) fn is_text_length(owner: crate::types::TypeName, name: &str) -> bool {
    // The provider presents it under the Kotlin name of the property, not the JVM accessor's:
    // `length`, where `kotlin.Enum`'s two arrive as `getName`/`getOrdinal`. Both spellings are
    // taken because which one a provider uses is the provider's business, not this table's.
    name == "length"
        && (["kotlin/CharSequence", "java/lang/CharSequence"]
            .iter()
            .any(|candidate| owner.matches(candidate))
            || matches!(
                kotlin_owner(&owner.render()),
                "kotlin/String" | "kotlin/text/StringBuilder"
            ))
}

/// Whether an accessor is `Throwable.message`.
///
/// The property is declared on `kotlin.Throwable` itself, so a subclass reading it — the program's
/// own or one of the runtime's — arrives here under the root's owner. Both spellings are taken for
/// the reason [`is_text_length`] gives: which one a provider uses is its business.
///
/// `cause` is deliberately NOT here. This `Throwable` has no cause field, so answering it would be
/// answering `null` to a program that passed one, and that declines instead.
pub(super) fn is_throwable_message(owner: crate::types::TypeName, name: &str) -> bool {
    throwable_field(owner, name) == Some("kt_throwable_message")
}

/// The runtime reader for one of `Throwable`'s two fields, or `None` for any other accessor.
///
/// Both are read by the runtime rather than by an offset here, because the class is the runtime's
/// and so is its layout. A SUBCLASS of it declared in this file is read the same way: its storage
/// begins with the base's, which is exactly what makes one reader answer for both.
pub(super) fn throwable_field(owner: crate::types::TypeName, name: &str) -> Option<&'static str> {
    if kotlin_owner(&owner.render()) != "kotlin/Throwable" {
        return None;
    }
    match name {
        "message" => Some("kt_throwable_message"),
        "cause" => Some("kt_throwable_cause"),
        _ => None,
    }
}

/// The runtime function answering `Result.isSuccess` / `Result.isFailure`, or `None`.
///
/// Both are questions about the one reference a `Result` IS: whether it is the failure marker.
pub(super) fn result_predicate(owner: crate::types::TypeName, name: &str) -> Option<&'static str> {
    if kotlin_owner(&owner.render()) != "kotlin/Result" {
        return None;
    }
    match name {
        "isSuccess" => Some("kt_result_is_success"),
        "isFailure" => Some("kt_result_is_failure"),
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
/// `Result.Companion` is the only one: it holds no state, and every member of it is answered
/// directly (see [`runtime_companion_member`]), so nothing ever reads the value a reference to it
/// would carry. A provider that materializes the receiver before the call reaches that table needs
/// something to materialize, and for such an object the honest answer is the null reference —
/// there is no object, and nothing dereferences it.
pub(super) fn is_stateless_runtime_object(classifier: crate::types::TypeName) -> bool {
    matches!(
        kotlin_owner(&classifier.render()),
        // The stdlib's standard delegates. `Delegates` declares no state and every member of it
        // this runtime answers takes its arguments alone.
        "kotlin/Result$Companion" | "kotlin/properties/Delegates"
    )
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
    let suffix = match kotlin_owner(&classifier.render()) {
        "kotlin/Byte$Companion" => "byte",
        "kotlin/Short$Companion" => "short",
        "kotlin/Int$Companion" => "int",
        "kotlin/Long$Companion" => "long",
        "kotlin/Char$Companion" => "char",
        "kotlin/Boolean$Companion" => "boolean",
        "kotlin/Float$Companion" => "float",
        "kotlin/Double$Companion" => "double",
        "kotlin/String$Companion" => "string",
        _ => return None,
    };
    Some(match suffix {
        "byte" => "kt_byte_companion",
        "short" => "kt_short_companion",
        "int" => "kt_int_companion",
        "long" => "kt_long_companion",
        "char" => "kt_char_companion",
        "boolean" => "kt_boolean_companion",
        "float" => "kt_float_companion",
        "double" => "kt_double_companion",
        _ => "kt_string_companion",
    })
}

/// A member the runtime answers with a REFERENCE, whatever the call site's own type.
///
/// `Result.getOrThrow()` on a `Result<Int>` answers the box that `Result` holds, and the site
/// wants the integer — so these are apart from [`runtime_member`], whose entries answer exactly
/// what their call sites ask for and need no conversion.
pub(super) fn runtime_reference_member(
    owner: &str,
    name: &str,
    params: &[Ty],
) -> Option<&'static str> {
    // `getOrThrow` is a top-level extension, so it arrives as a member of the `kotlin` facade
    // rather than of `Result`.
    if declaration_package(kotlin_owner(owner)) == "kotlin"
        && name == "getOrThrow"
        && params.is_empty()
    {
        return Some("kt_result_get_or_throw");
    }
    match (kotlin_owner(owner), name, params) {
        ("kotlin/Result", "getOrNull", []) => Some("kt_result_get_or_null"),
        ("kotlin/Result", "exceptionOrNull", []) => Some("kt_result_exception_or_null"),
        ("kotlin/Result", "toString", []) => Some("kt_result_to_string"),
        _ => None,
    }
}

/// A member of a COMPANION the runtime realizes, whose receiver carries nothing.
///
/// `Result.Companion` has no state and no instance in any file krusty compiles, so there is no
/// receiver to pass and none to evaluate — the call is its arguments alone. That is why these are
/// apart from [`runtime_member`], which leads every call with the receiver.
pub(super) fn runtime_companion_member(
    owner: &str,
    name: &str,
    params: &[Ty],
) -> Option<&'static str> {
    match (kotlin_owner(owner), name, params) {
        ("kotlin/Result$Companion", "success", [_]) => Some("kt_result_success"),
        ("kotlin/Result$Companion", "failure", [_]) => Some("kt_result_failure"),
        // `Delegates.notNull()`. `Delegates` is an OBJECT of the stdlib, carrying nothing, and the
        // delegate it answers with starts empty — so this is the same shape: no receiver to read
        // and no operand to pass.
        ("kotlin/properties/Delegates", "notNull", []) => Some("kt_not_null_var"),
        // `observable(initial) { property, old, new -> … }`: the initial value and the callback,
        // both references, and the `KProperty` it later hands that callback is the one the
        // delegation passes to `setValue` — nothing here reads it.
        ("kotlin/properties/Delegates", "observable", [_, _]) => Some("kt_observable"),
        _ => None,
    }
}

pub(super) fn runtime_member(owner: &str, name: &str, params: &[Ty]) -> Option<&'static str> {
    // `removeSuffix` is a top-level extension of `kotlin.text`, so it arrives as a member of that
    // package's file facade; everything it takes and answers is a reference, which is this path.
    if declaration_package(kotlin_owner(owner)) == "kotlin/text" {
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
            ("append", [_]) => return Some("kt_string_builder_append"),
            _ => {}
        }
    }
    // `kotlin.Result` is a value class over `Any?` and its representation is Kotlin's own — a
    // success IS the value, a failure a marker holding the exception — so every member is a
    // question about that one reference and the runtime answers it directly. `getOrThrow` is a
    // top-level extension, so it arrives as a member of the `kotlin` facade rather than of
    // `Result`.
    match (kotlin_owner(owner), name, params) {
        ("kotlin/String", "plus", [_]) => Some("kt_string_plus"),
        // A builder's `append` takes one of a dozen overloads on the JVM and one function here:
        // every operand is rendered through its own `toString`, which is the same answer for all of
        // them, and the reference path has already boxed whichever primitive arrived.
        ("kotlin/text/StringBuilder", "append", [_]) => Some("kt_string_builder_append"),
        ("kotlin/text/StringBuilder", "appendLine", [_]) => Some("kt_string_builder_append_line"),
        ("kotlin/text/StringBuilder", "appendLine", []) => {
            Some("kt_string_builder_append_new_line")
        }

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

    /// Both providers' spellings of the same top-level declaration name the same package.
    #[test]
    fn a_top_level_declaration_names_its_package_under_either_spelling() {
        // The JVM provider's: the file facade a top-level function was compiled into.
        assert_eq!(declaration_package("kotlin/io/ConsoleKt"), "kotlin/io");
        assert_eq!(declaration_package("kotlin/text/StringsKt"), "kotlin/text");
        // The klib provider's: the package itself, because a klib has no facades.
        assert_eq!(declaration_package("kotlin/io"), "kotlin/io");
        assert_eq!(
            declaration_package("kotlin/collections"),
            "kotlin/collections"
        );
        assert_eq!(declaration_package("kotlin"), "kotlin");
    }

    /// The delegate operators on a property reference, under either provider's spelling.
    ///
    /// The klib spelling is the bare package, which `Lazy.getValue` also answers to — the owner
    /// does not separate them and is not asked to. Fourteen corpus cases delegated to a property
    /// reference and were declined because only the JVM facade was admitted.
    #[test]
    fn the_reference_delegate_operators_are_found_under_either_spelling() {
        assert!(is_property_delegates_facade(
            "kotlin/PropertyReferenceDelegatesKt"
        ));
        assert!(is_property_delegates_facade("kotlin/properties"));
        assert!(is_property_delegates_facade("kotlin"));
        // A different package, and a CLASS in the right one, are both still no.
        assert!(!is_property_delegates_facade("kotlin/collections"));
        assert!(!is_property_delegates_facade("kotlin/Lazy"));
        assert!(!is_property_delegates_facade("kotlin/text/StringsKt"));
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
            assert_eq!(boxed_step(jvm, "inc", &[]), Some((stepped, 1)));
            assert_eq!(boxed_step(kotlin, "inc", &[]), Some((stepped, 1)));
            assert_eq!(boxed_step(jvm, "dec", &[]), Some((stepped, -1)));
            assert_eq!(boxed_step(kotlin, "dec", &[]), Some((stepped, -1)));
        }
        // What the widened table must NOT admit. `Boolean` has no step at all, an argument means
        // the member is something else entirely, and a classifier that merely lives in `kotlin`
        // is not a primitive.
        assert_eq!(boxed_step("kotlin/Boolean", "inc", &[]), None);
        assert_eq!(boxed_step("kotlin/String", "inc", &[]), None);
        assert_eq!(boxed_step("kotlin/Int", "inc", &[Ty::Int]), None);
        assert_eq!(boxed_step("kotlin/Int", "plus", &[]), None);
    }

    /// A class comes back unchanged, so no comparison against a package can match it. This is what
    /// keeps a member of a real class from being read as a top-level declaration of the package
    /// that class lives in — `kotlin/collections/AbstractMutableList` must not answer
    /// `kotlin/collections` merely because it is declared there.
    #[test]
    fn a_class_is_never_mistaken_for_a_package() {
        assert_eq!(
            declaration_package("kotlin/text/Regex"),
            "kotlin/text/Regex"
        );
        assert_eq!(
            declaration_package("kotlin/collections/AbstractMutableList"),
            "kotlin/collections/AbstractMutableList"
        );
        assert_eq!(declaration_package("Ungrouped"), "Ungrouped");
        assert!(!is_collections_facade(
            "kotlin/collections/AbstractMutableList"
        ));
        assert!(is_collections_facade("kotlin/collections"));
        assert!(is_collections_facade("kotlin/collections/CollectionsKt"));
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
        assert!(is_indices("kotlin/collections/ArraysKt", "indices"));
        assert!(is_indices("kotlin/text/StringsKt", "indices"));
        assert!(!is_indices("kotlin/collections/ArraysKt", "size"));
        assert!(!is_indices("kotlin/collections/AbstractList", "indices"));
        // The accessor spelling is NOT the key: `indices` is realized as `getIndices` for text and
        // as a value-class-mangled name per unsigned array width, and none of those is what the
        // declaration is called.
        assert!(!is_indices("kotlin/collections/ArraysKt", "getIndices"));
    }

    #[test]
    fn an_unsigned_member_is_recognized_by_its_value_class() {
        assert_eq!(unsigned_owner("kotlin/UInt"), Some(Ty::UInt));
        assert_eq!(unsigned_owner("kotlin/ULong"), Some(Ty::ULong));
        assert_eq!(unsigned_owner("kotlin/Int"), None);
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
