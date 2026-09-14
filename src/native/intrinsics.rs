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
    /// A floating-point value, which the runtime cannot render (see `super::runtime`). Kept
    /// distinct from the other two so it DECLINES rather than falling through to `Any?`, which
    /// would ask for a box that deliberately does not exist.
    Unrenderable,
}

fn console_operand(ty: Ty) -> ConsoleOperand {
    match ty {
        Ty::Byte => ConsoleOperand::Scalar("byte"),
        Ty::Short => ConsoleOperand::Scalar("short"),
        Ty::Int => ConsoleOperand::Scalar("int"),
        Ty::Long => ConsoleOperand::Scalar("long"),
        Ty::Char => ConsoleOperand::Scalar("char"),
        Ty::Boolean => ConsoleOperand::Scalar("boolean"),
        Ty::Float | Ty::Double => ConsoleOperand::Unrenderable,
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
                ConsoleOperand::Unrenderable => None,
            }
        }
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
    fn printing_a_floating_point_value_is_declined_rather_than_routed_through_any() {
        // The runtime has no `kt_box_double`, because Kotlin's `Double.toString` is the
        // shortest-round-trip algorithm and nothing here implements it. Falling through to the
        // `Any?` overload would ask for that box and fail at LINK time, with no diagnostic naming
        // the construct.
        assert_eq!(
            runtime_function("kotlin/io/ConsoleKt", "println", &[Ty::Double]),
            None
        );
        assert_eq!(
            runtime_function("kotlin/io/ConsoleKt", "print", &[Ty::Float]),
            None
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
