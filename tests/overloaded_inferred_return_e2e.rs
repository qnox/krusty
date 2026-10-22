//! Two overloads of the same name, each an expression body with an INFERRED (unannotated) return of a
//! different type (`fun f(x: Int) = x + 1` : Int, `fun f(s: String) = s + "!"` : String). The inferred
//! returns are recorded per `(name, parameter types)`, so a call binds the right overload's return.
//! Before the fix the override map was keyed by name alone, so the second overload clobbered the first
//! and `f("hi")` was mis-typed as `Int` ("operator cannot be applied to Int and String").

use super::common;
use std::path::PathBuf;

fn overload_e2e_env(_label: &str) -> Option<(PathBuf, PathBuf)> {
    let java_home = common::java_home();
    let stdlib = common::stdlib_jar();
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    Some((stdlib, jdk))
}

fn compile_overload_case(src: &str, label: &str, expect_msg: &str) -> Option<String> {
    let (stdlib, jdk) = overload_e2e_env(label)?;
    Some(
        common::compile_and_run_box(src, "F", &[stdlib], Some(&jdk))
            .unwrap_or_else(|| panic!("{expect_msg}")),
    )
}

#[test]
fn overloaded_inferred_returns_dont_clobber() {
    let src = "fun f(x: Int) = x + 1\n\
fun f(s: String) = s + \"!\"\n\
fun box(): String {\n\
if (f(1) != 2) return \"fa\"\n\
if (f(\"hi\") != \"hi!\") return \"fb\"\n\
if (f(\"hi\").length != 3) return \"fc\"\n\
return \"OK\"\n\
}\n";
    let Some(out) = compile_overload_case(
        src,
        "overloaded_inferred_return_e2e",
        "krusty must keep overloaded inferred returns distinct (f(Int):Int, f(String):String)",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn overloaded_top_level_defaults_use_selected_decl() {
    let src = "fun choose(x: Int = 1): String = \"int:$x\"\n\
fun choose(s: String, suffix: String = \"K\"): String = s + suffix\n\
fun box(): String = choose(s = \"O\")\n";
    let Some(out) = compile_overload_case(
        src,
        "overloaded_top_level_defaults_use_selected_decl",
        "lowering must use the checker-selected overloaded top-level declaration",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn overloaded_top_level_spread_uses_selected_decl() {
    let src = "fun choose(vararg x: Long): String = \"long\"\n\
fun choose(vararg x: Int): String = \"OK\"\n\
fun box(): String {\n\
    val parts = intArrayOf(1, 2)\n\
    return choose(*parts)\n\
}\n";
    let Some(out) = compile_overload_case(
        src,
        "overloaded_top_level_spread_uses_selected_decl",
        "spread lowering must use the checker-selected overloaded top-level vararg",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn overloaded_top_level_inline_uses_selected_decl() {
    let src = "inline fun choose(x: Int): String = \"int\"\n\
inline fun choose(s: String): String = s\n\
fun box(): String = choose(\"OK\")\n";
    let Some(out) = compile_overload_case(
        src,
        "overloaded_top_level_inline_uses_selected_decl",
        "inline lowering must use the checker-selected overloaded top-level declaration",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn selected_overload_return_not_refined_by_generic_name_match() {
    let src = "fun <T> choose(x: T): T = x\n\
fun choose(x: String): Int = 42\n\
fun box(): String = if (choose(\"x\") == 42) \"OK\" else \"fail\"\n";
    let Some(out) = compile_overload_case(
        src,
        "selected_overload_return_not_refined_by_generic_name_match",
        "return refinement must use the checker-selected top-level overload",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn a_rejected_member_call_demands_its_inferred_return() {
    const SRC: &str = "class Api { fun accept(value: String) = \"x\" }\n\
        fun use(api: Api) = api.accept(null).length\n";
    let result = common::compiler_diagnostics(&[("RejectedInferred.kt", SRC)], &[]);
    let expected = [common::CompilerError {
        file: "RejectedInferred.kt".to_string(),
        line: 2,
        column: 32,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the null argument"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(result.krusty_code, 0, "krusty accepted the null argument");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_rejected_member_extension_uses_its_generic_dispatch_receiver() {
    const SRC: &str = "class Api<T>(val x: T) {\n\
        fun String.accept(value: String) = x\n\
        }\n\
        fun use(api: Api<Int>) = with(api) { \"\".accept(null).inc() }\n";
    let result = common::compiler_diagnostics(&[("RejectedMemberExtension.kt", SRC)], &[]);
    let expected = [common::CompilerError {
        file: "RejectedMemberExtension.kt".to_string(),
        line: 4,
        column: 48,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the rejected member extension"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the rejected member extension"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_shape_matching_member_extension_follows_a_non_mapping_direct_member() {
    const SRC: &str = "class Token { fun choose(left: Int, right: Int): String = \"\" }\n\
        class Scope { fun Token.choose(value: String): String = \"\" }\n\
        fun use(token: Token, scope: Scope) = with(scope) { token.choose(null).length }\n";
    let result =
        common::compiler_diagnostics(&[("RejectedMemberExtensionAfterMember.kt", SRC)], &[]);
    let expected = [common::CompilerError {
        file: "RejectedMemberExtensionAfterMember.kt".to_string(),
        line: 3,
        column: 66,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the rejected member extension"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the rejected member extension"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn an_outer_dispatch_member_extension_follows_a_non_mapping_inner_dispatch() {
    const SRC: &str = r#"class Token { fun choose(left: Int, right: Int): String = "" }
class Scope { fun Token.choose(value: String): String = "" }
class Noise { fun Token.choose(left: Int, right: Int): String = "" }
fun use(token: Token, scope: Scope, noise: Noise) = with(scope) { with(noise) { token.choose(null).length } }
"#;
    let result = common::compiler_diagnostics(&[("RejectedOuterMemberExtension.kt", SRC)], &[]);
    let expected = [common::CompilerError {
        file: "RejectedOuterMemberExtension.kt".to_string(),
        line: 4,
        column: 94,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the rejected outer member extension"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the rejected outer member extension"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_mapped_outer_extension_follows_a_non_mapping_local_extension() {
    const SRC: &str = r#"class Token
fun Token.choose(value: String): String = ""
fun use(token: Token) = run {
    fun Token.choose(left: Int, right: Int): String = ""
    token.choose(null).length
}
"#;
    let result = common::compiler_diagnostics(&[("RejectedOuterExtension.kt", SRC)], &[]);
    let expected = [common::CompilerError {
        file: "RejectedOuterExtension.kt".to_string(),
        line: 5,
        column: 18,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the rejected outer extension"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the rejected outer extension"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn an_overriding_member_extension_owns_rejected_call_recovery() {
    const SRC: &str = r#"class Token
open class Base { open fun Token.choose(value: String): String = "base" }
class Derived : Base() { override fun Token.choose(value: String): String = "derived" }
fun use(token: Token) = with(Derived()) { token.choose(null).length }
"#;
    let result =
        common::compiler_diagnostics(&[("RejectedOverridingMemberExtension.kt", SRC)], &[]);
    let expected = [common::CompilerError {
        file: "RejectedOverridingMemberExtension.kt".to_string(),
        line: 4,
        column: 56,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the rejected overriding member extension"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the rejected overriding member extension"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_mapped_inherited_member_extension_outlives_a_non_mapping_derived_extension() {
    const SRC: &str = r#"class Token
open class Base { fun Token.choose(value: String): String = "base" }
class Derived : Base() { fun Token.choose(left: Int, right: Int): Int = 0 }
fun use(token: Token) = with(Derived()) { token.choose(null).length }
"#;
    let result = common::compiler_diagnostics(&[("RejectedInheritedMemberExtension.kt", SRC)], &[]);
    let expected = [common::CompilerError {
        file: "RejectedInheritedMemberExtension.kt".to_string(),
        line: 4,
        column: 56,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the rejected inherited member extension"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the rejected inherited member extension"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn an_ordinary_dispatch_receiver_precedes_a_context_receiver_during_recovery() {
    const SRC: &str = r#"class Token
class Context { fun Token.choose(value: String): String = "" }
class Host {
    fun Token.choose(value: Any): Int = 0
    context(context: Context)
    fun use(token: Token) = token.choose(null).inc()
}
"#;
    let result =
        common::compiler_diagnostics(&[("RejectedOrdinaryBeforeContextExtension.kt", SRC)], &[]);
    let expected = [common::CompilerError {
        file: "RejectedOrdinaryBeforeContextExtension.kt".to_string(),
        line: 6,
        column: 42,
        message: "null cannot be a value of a non-null type 'Any'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the rejected ordinary member extension"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the rejected ordinary member extension"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_rejected_member_reports_its_type_argument_arity_once() {
    const SRC: &str = r#"class Api { fun <T, U> pick(value: String): Int = 0 }
fun use(api: Api) = api.pick<String>(null).inc()
"#;
    common::assert_errors_match_kotlinc(&[("RejectedMemberTypeArity.kt", SRC)], &[]);
}

#[test]
fn rejected_member_arity_uses_the_most_specific_overload() {
    const SRC: &str = r#"class Api {
    fun <T, U> pick(value: Any): Long = 0
    fun <T, U> pick(value: String): Int = 0
}
fun use(api: Api) = api.pick<String>(null).inc()
"#;
    common::assert_errors_match_kotlinc(&[("RejectedMemberOverloadTypeArity.kt", SRC)], &[]);
}

#[test]
fn rejected_member_arity_prefers_fewer_omitted_defaults() {
    const SRC: &str = r#"class Api {
    fun <T, U> pick(value: String, extra: Int = 0): Long = 0
    fun <T, U> pick(value: String): Int = 0
}
fun use(api: Api) = api.pick<String>(null).inc()
"#;
    common::assert_errors_match_kotlinc(&[("RejectedMemberDefaultTypeArity.kt", SRC)], &[]);
}

#[test]
fn wrong_type_arity_local_overloads_leave_an_applicable_top_level_extension() {
    const SRC: &str = r#"class Token
fun <T> Token.pick(value: String): String = value
fun box(): String {
    fun <T, U> Token.pick(value: Any): Long = 0
    fun <T, U> Token.pick(value: String): Int = 0
    return Token().pick<String>("OK")
}
"#;
    let Some(out) = compile_overload_case(
        SRC,
        "wrong_type_arity_local_overloads_leave_top_level_extension",
        "wrong-arity local overloads must leave the applicable top-level extension",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn a_lexical_companion_precedes_a_context_receiver_during_recovery() {
    const SRC: &str = r#"class Token
class Context { fun Token.choose(value: String): String = "" }
class Host {
    companion object { fun Token.choose(value: Any): Int = 0 }
    context(context: Context)
    fun use(token: Token) = token.choose(null).inc()
}
"#;
    common::assert_errors_match_kotlinc(&[("RejectedCompanionBeforeContext.kt", SRC)], &[]);
}

#[test]
fn rejected_index_recovery_does_not_reintroduce_a_non_operator_member_extension() {
    const SRC: &str = r#"class Target
class Scope { fun Target.get(value: String): Int = 0 }
operator fun Target.get(index: Int): String = "OK"
fun box(): String = with(Scope()) { Target()[1] }
"#;
    let Some(out) = compile_overload_case(
        SRC,
        "rejected_index_non_operator_member_extension",
        "a rejected non-operator member extension must not hide an applicable operator extension",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

/// An inferred return selects among overloads whose members omit different defaults. No single
/// argument mapping fits the family, so each declaration is mapped on its own and the one omitting
/// the fewest defaults wins: `mk()` is `mk(source)`, as kotlinc selects. Signature inference used to
/// give up with "module signatures were not finalized".
const FEWEST_OMITTED_DEFAULTS: &str =
    "fun mk(id: String = \"\", source: Any? = null): String = \"two\"\n\
fun mk(source: Any? = null): String = \"one\"\n\
fun n() = mk()\n\
fun box(): String = if (n() == \"one\") \"OK\" else n()\n";

#[test]
fn an_inferred_return_selects_the_overload_omitting_the_fewest_defaults() {
    common::expect_box_same_as_kotlinc(FEWEST_OMITTED_DEFAULTS, "FewestOmittedDefaults");
}

#[test]
fn an_inferred_return_over_omitted_defaults_is_byte_identical_to_kotlinc() {
    let result = common::byte_diff_against_kotlinc_cp(
        "FewestOmittedDefaults",
        FEWEST_OMITTED_DEFAULTS,
        "FewestOmittedDefaultsKt",
        &[common::stdlib_jar()],
    )
    .expect("kotlinc and the stdlib are provisioned");
    assert_eq!(result, Ok(()));
}

/// Two overloads omitting the same number of defaults stay kotlinc's overload ambiguity.
#[test]
fn an_inferred_return_over_equally_many_omitted_defaults_is_ambiguous() {
    let result = common::compiler_diagnostics(
        &[(
            "Main.kt",
            "fun mk(id: String = \"\"): Int = 1\n\
             fun mk(source: Any? = null): Int = 2\n\
             fun n() = mk()\n",
        )],
        &[],
    );
    common::expect_identical_rejection(&result, "equally_many_omitted_defaults");
}

/// Specificity of the parameters the supplied arguments map to outranks the defaults a declaration
/// omits: `h(x = null)` binds `String?` over `Any?` even though that overload omits two defaults,
/// both for an inferred return and in a body with a declared one.
const SPECIFIC_OVER_FEWER_DEFAULTS: &str =
    "fun f(a: Int = 0, b: Long = 0, x: String): String = \"O\"\n\
fun f(c: Boolean = false, x: Any): String = \"f\"\n\
fun g(a: Int = 0, x: String = \"\"): String = \"K\"\n\
fun g(x: Any = 1): String = \"g\"\n\
fun h(a: Int = 0, b: Int = 0, x: String?): String = \"\"\n\
fun h(x: Any?): String = \"h\"\n\
fun n() = f(x = \"\") + g(x = \"\") + h(x = null)\n\
fun m(): String = h(x = null)\n\
fun box(): String = n() + m()\n";

#[test]
fn an_inferred_return_prefers_the_more_specific_overload_to_fewer_omitted_defaults() {
    common::expect_box_same_as_kotlinc(SPECIFIC_OVER_FEWER_DEFAULTS, "SpecificOverFewerDefaults");
}

#[test]
fn an_inferred_return_over_a_more_specific_overload_is_byte_identical_to_kotlinc() {
    let result = common::byte_diff_against_kotlinc_cp(
        "SpecificOverFewerDefaults",
        SPECIFIC_OVER_FEWER_DEFAULTS,
        "SpecificOverFewerDefaultsKt",
        &[common::stdlib_jar()],
    )
    .expect("kotlinc and the stdlib are provisioned");
    assert_eq!(result, Ok(()));
}
