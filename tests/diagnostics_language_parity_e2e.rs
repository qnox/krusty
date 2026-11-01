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
        let stem = filename.strip_suffix(".kts").expect("a script file name");
        let input = krusty::frontend::SourceInput::kotlin_script(source).with_file_stem(stem);
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

/// An initializer has no function to return from: an `init` block, in a class as in an enum
/// entry, and a property initializer, top-level as in a class.
#[test]
fn an_initializer_return_is_prohibited_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[(
            "Init.kt",
            "val top: Int = null ?: return\n\
             class C {\n\
             \x20   val member: Int = null ?: return\n\
             \x20   init { return }\n\
             }\n\
             enum class E {\n\
             \x20   A {\n\
             \x20       init { if (true) return }\n\
             \x20   };\n\
             }\n",
        )],
        &[],
    );
}

/// A `return` ends a script, except in a property initializer and in the statement the script
/// ends with, which kotlinc reads as the script's result when it is an expression in kotlinc's
/// tree: a `return` or a `for` loop, but not a `while` loop or a statement followed by a declaration.
#[test]
fn a_script_return_is_prohibited_in_its_result_position_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[
            (
                "Positions.kts",
                "if (args.isEmpty()) return\n\
                 val x: Int = null ?: return\n\
                 println(x)\n\
                 for (a in args) return\n",
            ),
            ("Trailing.kts", "println(1)\nreturn\n"),
            ("Declared.kts", "println(1)\nreturn\nfun g() {}\n"),
            ("Looped.kts", "println(1)\nwhile (true) return\n"),
        ],
        &[],
    );
}

/// Only a contextual one-parameter function type introduces a lambda's implicit `it`. Without
/// one (an `Any` or type-variable expectation, a `() -> R` parameter, or no expectation at all) a
/// lambda that omits its parameter list declares no parameters, so `it` is unresolved.
#[test]
fn implicit_it_without_a_function_type_is_unresolved_like_kotlinc() {
    let source = "fun <T> id(x: T): T = x\n\
                  fun anyExpected() { val f: Any = { it } }\n\
                  fun typeVariable() { id { it } }\n\
                  fun noParameters() { run { it } }\n\
                  fun element() { val l = listOf({ it }) }\n\
                  fun branch() { val g = if (true) { { it } } else null }\n\
                  fun bare() { val f = { it.toString() } }";
    let stdlib = common::stdlib_jar();
    let result =
        common::compiler_diagnostics(&[("ImplicitIt.kt", source)], std::slice::from_ref(&stdlib));
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty_errors, errors(&result.reference_stderr));
    assert_eq!(
        krusty_errors
            .iter()
            .map(|error| (error.line, error.column, error.message.as_str()))
            .collect::<Vec<_>>(),
        [
            (2, 36, "unresolved reference 'it'."),
            (
                3,
                22,
                "cannot infer type for type parameter 'T'. Specify it explicitly."
            ),
            (3, 27, "unresolved reference 'it'."),
            (
                4,
                22,
                "cannot infer type for type parameter 'R'. Specify it explicitly."
            ),
            (4, 28, "unresolved reference 'it'."),
            (
                5,
                25,
                "cannot infer type for type parameter 'T'. Specify it explicitly."
            ),
            (5, 34, "unresolved reference 'it'."),
            (6, 38, "unresolved reference 'it'."),
            (7, 24, "unresolved reference 'it'."),
        ]
    );
}

/// Every contextual one-parameter function type still binds `it`: a typed declaration, a branch
/// of one, a stdlib higher-order argument, a fun interface, a suspend function type, and an
/// explicit type argument.
#[test]
fn implicit_it_with_a_one_parameter_function_type_runs_like_kotlinc() {
    let source = "fun interface IntOp { fun apply(x: Int): Int }\n\
                  fun <T> id(x: T): T = x\n\
                  fun take(op: IntOp) = op.apply(2)\n\
                  fun box(): String {\n\
                      val f: (Int) -> Int = { it * 2 }\n\
                      val g: ((Int) -> Int)? = if (f(1) > 0) { { it + 1 } } else null\n\
                      val h = listOf(1, 2).map { it + 1 }.filter { it > 2 }\n\
                      val k = IntOp { it + 3 }\n\
                      val n: suspend (Int) -> Int = { it }\n\
                      val o = id<(String) -> Int> { it.length }\n\
                      val all = listOf(f(1), g!!(1), h.single(), k.apply(1), take { it * 5 }, o(\"ab\"))\n\
                      return if (all == listOf(2, 2, 3, 4, 10, 2) && n != null) \"OK\" else all.toString()\n\
                  }";
    common::expect_box_ok_with_stdlib(source, "ImplicitItContext");
}
