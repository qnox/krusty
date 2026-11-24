//! A subclass using a protected member of its superclass's companion object that is not exposed as
//! a static member: kotlinc reports `SUBCLASS_CANT_CALL_COMPANION_PROTECTED_NON_STATIC` instead of
//! an access error. `@JvmStatic`, `@JvmField` and `const` members are exposed on the superclass
//! and stay usable; a use outside any subclass is still an access error.

use super::common;

const SOURCE: &str = "open class Base {
    companion object {
        protected fun bar() {}
        protected fun Int.ext() {}
        protected val prop = 0
        protected var plain = 0
        protected lateinit var late: String
        @JvmStatic protected fun gav() {}
        @JvmStatic protected val jsv = 0
        @JvmField protected val field = 0
        protected const val konst = 1
        val open = 2
    }
}

class Sub : Base() {
    inner class Inner {
        fun use() {
            bar()
            late
        }
    }

    fun test() {
        bar()
        with(1) { ext() }
        prop
        plain
        plain = 2
        late
        Base.bar()
        Base.Companion.bar()
        val reference = ::bar
        val propertyReference = ::prop

        gav()
        jsv
        field
        konst
        open
    }
}

class Outside {
    fun test() {
        Base.bar()
    }
}
";

fn reports(source: &str, args: &[&str]) -> (Vec<String>, Vec<String>) {
    reports_with(source, &[], args)
}

fn reports_with(
    source: &str,
    libraries: &[std::path::PathBuf],
    args: &[&str],
) -> (Vec<String>, Vec<String>) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result =
        common::compiler_diagnostics_with_shared_args(&[("Use.kt", source)], libraries, &args);
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let render = |diagnostics: Vec<common::CompilerError>| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}: {}",
                    diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    (
        render(common::compiler_errors(&krusty_output)),
        render(common::compiler_errors(&result.reference_stderr)),
    )
}

#[test]
fn a_subclass_cannot_use_a_non_static_protected_companion_member() {
    let (krusty, kotlinc) = reports(SOURCE, &["-no-reflect"]);
    assert_eq!(krusty, kotlinc);
    assert_eq!(
        kotlinc
            .iter()
            .filter(|diagnostic| diagnostic.ends_with("is not yet supported."))
            .count(),
        12
    );
    assert_eq!(
        kotlinc.last().map(String::as_str),
        Some("46:14: cannot access 'fun bar(): Unit': it is protected in 'Base.Companion'.")
    );
}

const GENERIC: &str = "open class GenericBase<T> {
    companion object {
        protected fun bar() {}
    }
}

class GenericSub : GenericBase<String>() {
    fun test() {
        bar()
    }
}
";

/// A generic superclass is reported only once
/// `ReportSubclassCantCallCompanionProtectedNonStaticWithGenerics` is on.
#[test]
fn a_generic_superclass_is_reported_only_under_its_feature() {
    let (krusty, kotlinc) = reports(GENERIC, &["-no-reflect"]);
    assert_eq!(krusty, kotlinc);
    assert!(kotlinc.is_empty());
    if krusty::kotlin_version::target() < krusty::kotlin_version::KotlinVersion::V2_5_0_RC {
        return;
    }
    let (krusty, kotlinc) = reports(
        GENERIC,
        &[
            "-no-reflect",
            "-XXLanguage:+ReportSubclassCantCallCompanionProtectedNonStaticWithGenerics",
        ],
    );
    assert_eq!(krusty, kotlinc);
    assert_eq!(
        kotlinc,
        ["9:9: using protected members that are not '@JvmStatic' in the superclass companion is not yet supported."]
    );
}

const QUALIFIED_AND_CONSTANT: &str = "open class Base {
    companion object {
        protected var plain = 0
        protected const val konst = 1
    }
}

class Sub : Base() {
    fun test() {
        Base.Companion.plain = 2
        val reference = ::konst
        val qualified = Base.Companion::konst
    }
}
";

/// A write through the companion is reported like the unqualified one, and a reference to a
/// `const` is not, as a constant needs no annotation to be exposed.
#[test]
fn a_qualified_write_is_reported_and_a_constant_reference_is_not() {
    let (krusty, kotlinc) = reports(QUALIFIED_AND_CONSTANT, &["-no-reflect"]);
    assert_eq!(krusty, kotlinc);
    assert_eq!(
        kotlinc,
        ["10:24: using protected members that are not '@JvmStatic' in the superclass companion is not yet supported."]
    );
}

const DEPENDENCY: &str = "open class Base {
    companion object {
        protected fun bar() {}
        protected val prop = 0
        @JvmStatic protected fun gav() {}
        @JvmStatic protected val jsv = 0
        @JvmField protected val field = 0
        protected const val konst = 1
    }
}
";

const DEPENDENCY_USE: &str = "class Sub : Base() {
    fun test() {
        bar()
        prop
        gav()
        jsv
        field
        konst
    }
}
";

/// The superclass compiled into a dependency carries its static members as the platform
/// compiled them, so exposure is read from them rather than from annotations in this compilation.
/// kotlinc reports the two non-static uses and none of the exposed members.
#[test]
fn a_dependency_superclass_exposes_its_static_companion_members() {
    let library = common::kotlinc_library(DEPENDENCY).expect("kotlinc builds the dependency");
    let (krusty, kotlinc) = reports_with(DEPENDENCY_USE, &[library], &["-no-reflect"]);
    assert_eq!(krusty, kotlinc);
    assert_eq!(
        kotlinc,
        [
            "3:9: using protected members that are not '@JvmStatic' in the superclass companion is not yet supported.",
            "4:9: using protected members that are not '@JvmStatic' in the superclass companion is not yet supported.",
        ]
    );
}
