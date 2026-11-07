//! A modifier written inside a declaration kind that does not admit it
//! (`WRONG_MODIFIER_CONTAINING_DECLARATION`). kotlinc describes the containing declaration by its
//! targets (a class, a local class or object literal, an annotation class, an interface, an enum
//! class, an enum entry, a standalone or companion object, or the file) and allows each of
//! `private`, `internal`, `protected`, `inner`, `override`, `final` and `companion` only inside some
//! of them. The report is on the keyword and names the container.
//!
//! DIFFERENTIAL: each program goes through kotlinc and krusty, and the exit code and the complete
//! errors and warnings, with file, line, column and message, are compared in emission order.

use super::common;

fn assert_reported_like_kotlinc(source: &str) {
    let result = common::compiler_diagnostics_with_reference_args(
        &[("Use.kt", source)],
        &[common::stdlib_jar()],
        &[],
    );
    let reported = |code: i32, stderr: &str| {
        (
            code,
            common::compiler_errors(stderr),
            common::compiler_warnings(stderr),
        )
    };
    assert_eq!(
        reported(result.krusty_code, &result.krusty_stderr),
        reported(result.reference_code, &result.reference_stderr),
        "\nkrusty: {}\nkotlinc: {}",
        result.krusty_stderr,
        result.reference_stderr
    );
}

/// An annotation class's companion takes no visibility: the annotation class admits neither
/// `private`, `internal` nor `protected`, though it admits `companion` and `final`.
#[test]
fn an_annotation_class_admits_no_visibility_modifier() {
    assert_reported_like_kotlinc(
        r#"annotation class Hidden { private companion object }
annotation class Module { internal companion object }
annotation class Guarded { protected companion object }
annotation class Plain { companion object { const val LIMIT = 1 }; final class Nested }
annotation class Holder { inner class Inner }
"#,
    );
}

/// An interface admits `private` but not `internal`, `protected`, `inner` or `final`.
#[test]
fn an_interface_admits_only_private_and_override() {
    assert_reported_like_kotlinc(
        r#"interface Keyed { internal companion object }
interface Guarded { protected companion object }
interface Hidden { private companion object }
interface Members {
    protected fun guarded() {}
    internal fun module() {}
    private fun hidden() {}
    final fun closed() {}
    override fun toString(): String
    inner class Inner
    final val size: Int get() = 1
}
"#,
    );
}

/// An object admits `internal`, `private`, `override` and `final` but not `inner`; only a companion
/// also admits `protected`.
#[test]
fn an_object_admits_protected_only_as_a_companion() {
    assert_reported_like_kotlinc(
        r#"object Single {
    protected val guarded = 1
    internal val module = 2
    private fun hidden() {}
    final val closed = 3
    override fun toString() = ""
    inner class Inner
}
class Owner {
    companion object {
        protected val guarded = 1
        inner class Inner
    }
}
"#,
    );
}

/// An enum entry admits `internal` and `inner` but not `protected`; an inner class may nest in an
/// inner class, a local class or an object literal.
#[test]
fn an_enum_entry_and_local_scopes_follow_their_own_targets() {
    assert_reported_like_kotlinc(
        r#"enum class Mode {
    ON {
        protected fun guarded() {}
        internal fun module() {}
        inner class Inner
    };
    inner class Outer
}
class Outer {
    inner class Inner {
        protected fun guarded() {}
        internal fun module() {}
        inner class Deeper
    }
}
fun build(): Any {
    class Local {
        protected fun guarded() {}
        inner class Inner
    }
    return object {
        protected fun guarded() {}
        internal fun module() {}
        inner class Inner
    }
}
private class Closed(protected val value: Int) {
    protected constructor() : this(1)
}
"#,
    );
}
