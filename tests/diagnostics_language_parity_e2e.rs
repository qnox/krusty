//! Exact shared-language diagnostic wording and location parity with kotlinc.

use super::common;
use super::diagnostics_parity_support::{errors, first_error};

#[test]
fn shared_diagnostic_wording_matches_kotlinc() {
    let source = "fun breakOutside() { break }\n\
                  fun continueOutside() { continue }\n\
                  fun varargs(vararg first: Int, vararg second: Int) {}\n\
                  open class Base\n\
                  class Derived : Base() { override fun absent() {} }\n\
                  val topLevelThis: Any = this\n\
                  class ConstructorThis(val value: Any = this)";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("SharedDiagnostics.kt", source)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        krusty_errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>(),
        [
            "'break' and 'continue' are only allowed inside loops.",
            "'break' and 'continue' are only allowed inside loops.",
            "multiple vararg parameters are prohibited.",
            "multiple vararg parameters are prohibited.",
            "'absent' overrides nothing.",
            "'this' is not defined in this context.",
            "cannot access '<this>' before the instance has been initialized.",
        ]
    );
}

#[test]
fn prohibited_script_returns_match_kotlinc() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    for (filename, source) in [
        ("ReturnStatement.kts", "return"),
        ("ReturnExpression.kts", "null ?: return"),
    ] {
        let input = krusty::frontend::SourceInput::kotlin_script(source);
        let krusty_diagnostics = common::front_end_diagnostics_inputs(
            &[input],
            std::slice::from_ref(&stdlib),
            Some(jdk.as_path()),
        );
        let (reference_code, reference_stderr) =
            common::kotlinc_named_source_result(filename, source);
        assert_ne!(
            reference_code, 0,
            "kotlinc unexpectedly accepted {source:?}"
        );
        let reference_error = first_error(&reference_stderr)
            .unwrap_or_else(|| panic!("kotlinc emitted no location diagnostic for {source:?}"));
        assert_eq!(
            krusty_diagnostics,
            [reference_error.message],
            "script source: {source:?}"
        );
        assert_eq!(
            krusty_diagnostics,
            ["'return' is prohibited here."],
            "script source: {source:?}"
        );
    }
}

#[test]
fn inherited_override_visibility_access_matches_kotlinc() {
    // An override written without a visibility modifier keeps the overridden member's visibility:
    // `plain` is protected in `Simple`, so the external call is rejected with the protected-member
    // diagnostic naming the subclass that now declares it.
    let source = "open class Base {\n\
                  \x20   protected open fun plain(): Int = 1\n\
                  }\n\
                  class Simple : Base() {\n\
                  \x20   override fun plain(): Int = 2\n\
                  }\n\
                  fun use(): Int = Simple().plain()\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("InheritedOverrideVisibility.kt", source)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        krusty_errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>(),
        ["cannot access 'fun plain(): Int': it is protected in 'Simple'."],
    );
}

#[test]
fn internal_override_visibility_access_matches_kotlinc_across_modules() {
    // An override written without a visibility modifier keeps the overridden member's `internal`,
    // so a dependent module cannot call it. Both compilers reject at the callee with the same
    // complete diagnostic.
    let lib = "open class Base {\n\
               \x20   internal open fun f(): Int = 1\n\
               }\n\
               class Derived : Base() {\n\
               \x20   override fun f(): Int = 2\n\
               }\n";
    let Some(library) = common::kotlinc_lib_out(&[("Lib.kt", lib)]) else {
        return;
    };
    let classpath = [library, common::stdlib_jar()];
    let result = common::compiler_diagnostics(
        &[("Use.kt", "fun use(): Int = Derived().f()\n")],
        &classpath,
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        kotlinc_errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>(),
        ["cannot access 'fun f(): Int': it is internal in 'Derived'."],
    );
}

#[test]
fn local_override_visibility_access_matches_kotlinc() {
    // kotlinc rejects the enclosing function's read of the local class's inherited-`protected`
    // override; the emitted class itself already carries the inherited visibility (see
    // `method_access_flags_e2e::local_override_keeps_overridden_visibility_like_kotlinc`).
    let source = "open class Base {\n\
                  \x20   protected open fun f(): Int = 1\n\
                  }\n\
                  fun box(): String {\n\
                  \x20   class Local : Base() {\n\
                  \x20       override fun f(): Int = 2\n\
                  \x20   }\n\
                  \x20   val touched = Local().f()\n\
                  \x20   return if (touched == 2) \"OK\" else \"fail\"\n\
                  }\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("LocalOverrideAccess.kt", source)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        kotlinc_errors
            .iter()
            .map(|error| (error.line, error.column, error.message.as_str()))
            .collect::<Vec<_>>(),
        [(
            8,
            27,
            "cannot access 'fun f(): Int': it is protected in file."
        )],
    );
}
