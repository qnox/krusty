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

/// An inline function's inlinable parameter may only be invoked or passed on to an inline
/// parameter of another inline function. Every other read is `USAGE_IS_NOT_INLINABLE`: an
/// argument of a non-inline function (also through a safe call) or of a `noinline` parameter, the
/// receiver of a safe call, `!!` or an extension call, and a value stored in a local. A `noinline`
/// parameter is an ordinary value, and a contract only names the parameter.
#[test]
fn an_inline_parameter_is_only_invoked_or_inlined_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[(
            "InlineParameterUsage.kt",
            "fun plain(f: (Int) -> Unit): (Int) -> Int = { it }\n\
             inline fun inlined(f: (Int) -> Unit): (Int) -> Int = { it }\n\
             inline fun takesNoinline(noinline f: (Int) -> Unit) = f\n\
             inline fun ((Int) -> Unit).extension() {}\n\
             class Box {\n\
             \x20   inline fun call(f: (Int) -> Unit) { f(0) }\n\
             \x20   fun plainCall(f: (Int) -> Unit) { f(0) }\n\
             }\n\
             inline fun safe(box: Box?, i: Int?, s: (Int) -> Unit) {\n\
             \x20   box?.call(s)\n\
             \x20   box?.plainCall(s)\n\
             \x20   i?.let(s)\n\
             }\n\
             inline fun use(s: (Int) -> Unit, noinline n: ((Int) -> Unit)?, noinline k: (Int) -> Unit) {\n\
             \x20   plain(s)\n\
             \x20   inlined(s)\n\
             \x20   takesNoinline(s)\n\
             \x20   s(1)\n\
             \x20   s.invoke(2)\n\
             \x20   s?.invoke(3)\n\
             \x20   s!!.invoke(4)\n\
             \x20   s.extension()\n\
             \x20   val t = s\n\
             \x20   plain(k)\n\
             \x20   plain(n ?: {})\n\
             \x20   run { s(5) }\n\
             \x20   listOf(1).forEach(s)\n\
             \x20   if (t != k) println(t)\n\
             }\n\
             @OptIn(kotlin.contracts.ExperimentalContracts::class)\n\
             inline fun once(s: () -> Unit) {\n\
             \x20   kotlin.contracts.contract { callsInPlace(s, kotlin.contracts.InvocationKind.EXACTLY_ONCE) }\n\
             \x20   s()\n\
             }\n",
        )],
        &[],
    );
}

/// The forwarded-lambda program the box suite once ran: `c` passes its inlinable parameter to the
/// non-inline `callIt`, which needs it as a value, so kotlinc rejects the whole input. The runtime
/// behaviour is covered by the box suite's `noinline` variant.
#[test]
fn an_inline_parameter_forwarded_to_a_non_inline_function_is_rejected_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[(
            "InlineLambdaForwarded.kt",
            "inline fun a(f: () -> Int): Int = f()\n\
             inline fun b(f: () -> Int): Int = a(f) + 1\n\
             fun callIt(f: () -> Int): Int = f()\n\
             inline fun c(f: () -> Int): Int = callIt(f) * 2\n\
             fun box(): String {\n\
             \x20   if (b { 5 } != 6) return \"f0\"\n\
             \x20   if (c { 7 } != 14) return \"f1\"\n\
             \x20   return \"OK\"\n\
             }\n",
        )],
        &[],
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
