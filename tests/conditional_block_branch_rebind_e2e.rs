//! A conditional branch written as a BLOCK rebinds against its sibling like an expression branch.
//!
//! `if (c) A() else B()` lets an under-constrained branch take its type arguments from the other
//! branch — that is how `if (found) Mono.just(auth) else Mono.empty()` gives `Mono.empty()` its
//! element type. The rebinding asked the BRANCH expression for the generic signature to re-solve,
//! and a block is not a call, so wrapping either branch in braces silently disabled it: the branch
//! stayed at its unconstrained result and the join collapsed to `Mono<Any>`.
//!
//! Braces around a branch are not a semantic choice, and a multi-statement branch has no other
//! spelling, so this made an ordinary reactive body unrepresentable.

use super::common;

const JAVA: &[(&str, &str)] = &[
    ("Pub.java", "package pub;\npublic interface Pub<T> {}\n"),
    (
        "Box.java",
        "package pub;\n\
         public final class Box<T> {\n\
         \x20 public T get() { return null; }\n\
         }\n",
    ),
    (
        "Mono.java",
        "package pub;\n\
         import java.util.function.Function;\n\
         public final class Mono<T> implements Pub<T> {\n\
         \x20 public static <R> Mono<R> empty() { return new Mono<R>(); }\n\
         \x20 public static <T> Mono<T> just(T value) { return new Mono<T>(); }\n\
         \x20 public <R> Mono<R> flatMap(Function<? super T, ? extends Mono<? extends R>> f) {\n\
         \x20\x20 return new Mono<R>();\n\
         \x20 }\n\
         \x20 public <R> Mono<R> map(Function<? super T, ? extends R> f) {\n\
         \x20\x20 return new Mono<R>();\n\
         \x20 }\n\
         }\n",
    ),
];

fn assert_both_accept(user: &str, what: &str) {
    let java = JAVA
        .iter()
        .map(|(name, source)| ((*name).into(), (*source).into()))
        .collect::<Vec<(String, String)>>();
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(&[("Use.kt", user)], &classpath);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the {what} fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must accept the exact source kotlinc accepts for {what}"
    );
}

const PRELUDE: &str = "import pub.Mono\nimport pub.Pub\nclass Auth(val n: String)\n";

const NAMED_LAMBDA_LIBRARY: &str = "package dep\n\
class Cell<T> {\n\
  companion object {\n\
    fun <T> empty(): Cell<T> = error(\"unused\")\n\
  }\n\
}\n\
class Selector {\n\
  fun <A, R> choose(first: () -> A, second: () -> Cell<R>): Cell<R> = error(\"unused\")\n\
}\n";

const SAME_SPELLED_GENERIC_DECLARATIONS: &str = "package identities\n\
class Left<T>(val value: T)\n\
class Right<T>(val value: T)\n\
val <T> Left<T>.echo: T get() = value\n\
var <T> Right<T>.echo: T\n\
  get() = value\n\
  set(ignored) {}\n";

/// Control: expression branches already rebound, and must keep doing so.
#[test]
fn expression_branches_rebind_against_their_sibling() {
    assert_both_accept(
        &format!(
            "{PRELUDE}fun use(m: Mono<String>): Pub<Auth> =\n\
             \x20 m.flatMap {{ s -> if (s.isEmpty()) Mono.just(Auth(s)) else Mono.empty() }}\n"
        ),
        "expression branches",
    );
}

/// The fix: the same branches wrapped in braces.
#[test]
fn block_branches_rebind_against_their_sibling() {
    assert_both_accept(
        &format!(
            "{PRELUDE}fun use(m: Mono<String>): Pub<Auth> =\n\
             \x20 m.flatMap {{ s -> if (s.isEmpty()) {{ Mono.just(Auth(s)) }} else {{ Mono.empty() }} }}\n"
        ),
        "block branches",
    );
}

/// A multi-statement branch, which has no expression spelling at all.
#[test]
fn a_multi_statement_branch_rebinds_against_its_sibling() {
    assert_both_accept(
        &format!(
            "{PRELUDE}fun use(m: Mono<String>): Pub<Auth> =\n\
             \x20 m.flatMap {{ s ->\n\
             \x20\x20 if (s.isEmpty()) {{\n\
             \x20\x20\x20 println(s)\n\
             \x20\x20\x20 Mono.just(Auth(s))\n\
             \x20\x20 }} else {{\n\
             \x20\x20\x20 Mono.empty()\n\
             \x20\x20 }}\n\
             \x20 }}\n"
        ),
        "a multi-statement branch",
    );
}

/// The under-constrained branch may be the one in braces on EITHER side.
#[test]
fn either_side_may_be_the_block() {
    assert_both_accept(
        &format!(
            "{PRELUDE}fun use(m: Mono<String>): Pub<Auth> =\n\
             \x20 m.flatMap {{ s -> if (s.isEmpty()) {{ Mono.empty() }} else Mono.just(Auth(s)) }}\n"
        ),
        "a block on the empty side",
    );
}

/// `when` uses the same branch machinery.
#[test]
fn a_when_arm_written_as_a_block_rebinds_too() {
    assert_both_accept(
        &format!(
            "{PRELUDE}fun use(m: Mono<String>): Pub<Auth> =\n\
             \x20 m.flatMap {{ s ->\n\
             \x20\x20 when {{\n\
             \x20\x20\x20 s.isEmpty() -> {{ Mono.empty() }}\n\
             \x20\x20\x20 else -> {{ Mono.just(Auth(s)) }}\n\
             \x20\x20 }}\n\
             \x20 }}\n"
        ),
        "a `when` arm block",
    );
}

/// Control: a branch whose block produces NO value is still `Unit`, so a genuinely incompatible
/// pair is still rejected. Both compilers' complete diagnostics are pinned independently.
#[test]
fn an_incompatible_block_branch_is_still_rejected() {
    let java = JAVA
        .iter()
        .map(|(name, source)| ((*name).into(), (*source).into()))
        .collect::<Vec<(String, String)>>();
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(
        &[(
            "Use.kt",
            &format!(
                "{PRELUDE}fun use(m: Mono<String>): Pub<Auth> =\n\
                 \x20 m.flatMap {{ s -> if (s.isEmpty()) {{}} else {{ Mono.empty() }} }}\n"
            ),
        )],
        &classpath,
    );
    let reference_path = result
        .reference_stderr
        .split(':')
        .next()
        .expect("kotlinc names the rejected file");
    const SOURCE_LINE: &str = "  m.flatMap { s -> if (s.isEmpty()) {} else { Mono.empty() } }";
    const BRANCH_CARET: &str = "                   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^";
    const CALL_CARET: &str = "                                                   ^^^^^";
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (
            1,
            format!(
                "{reference_path}:5:20: error: return type mismatch: expected 'Mono<out Auth!>!', \
                 actual 'Any!'.\n{SOURCE_LINE}\n{BRANCH_CARET}\n{reference_path}:5:52: error: \
                 cannot infer type for type parameter 'T'. Specify it explicitly.\n{SOURCE_LINE}\n\
                 {CALL_CARET}\n"
            )
            .as_str()
        ),
    );
    let path = result
        .krusty_stderr
        .split(':')
        .next()
        .expect("krusty names the rejected file");
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (
            1,
            format!(
                "{path}:5:13: error: argument type mismatch: actual type is '(String!) -> Any!', \
                 but 'Function<in String, out Mono<out Auth>!>!' was expected.\nkrusty: \
                 1 error(s)\n"
            )
            .as_str()
        ),
    );
}

/// Without an enclosing result expectation, the selected `flatMap` call still owns an open `R`.
/// Its primary inference/return mismatch does not consume the independent open `R` selected for a
/// different declaration's terminal conditional branch. The deliberately same-spelled formals
/// prove that declaration identity, not `R`, owns each diagnostic. Every value-conditional form
/// must retain the complete ordered diagnostic set, including the branch call's exact location.
#[test]
fn open_selected_lambda_results_preserve_terminal_branch_diagnostics() {
    let java = JAVA
        .iter()
        .map(|(name, source)| ((*name).into(), (*source).into()))
        .collect::<Vec<(String, String)>>();
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    for (form, body) in [
        ("if", "if (s.isEmpty()) {} else { Mono.empty() }"),
        ("when", "when { s.isEmpty() -> {}; else -> Mono.empty() }"),
        ("elvis", "null ?: Mono.empty()"),
        ("try", "try {} catch (ignored: Exception) { Mono.empty() }"),
    ] {
        let user = format!(
            "import pub.Mono\n\
             fun use(m: Mono<String>) =\n\
             \x20 m.flatMap {{ s -> {body} }}\n"
        );
        let result = common::compiler_diagnostics(&[("Use.kt", &user)], &classpath);
        common::expect_identical_rejection(
            &result,
            &format!("open selected lambda {form} branch completion"),
        );
    }
}

/// The selected member retains declaration-ordered parameters, while source arguments retain
/// source order. Open-result ownership must cross the exact argument -> visible parameter ->
/// declaration parameter mapping rather than indexing either vector with the other's coordinate.
#[test]
fn reversed_named_lambdas_preserve_the_selected_parameters_open_result() {
    let Some(library) = common::kotlinc_library(NAMED_LAMBDA_LIBRARY) else {
        eprintln!("skipping: kotlinc unavailable");
        return;
    };
    let classpath = [library, common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(
        &[(
            "Use.kt",
            "import dep.Cell\n\
             import dep.Selector\n\
             fun use(selector: Selector) = selector.choose(\n\
             \x20 second = { if (true) {} else Cell.empty() },\n\
             \x20 first = { \"fixed\" }\n\
             )\n",
        )],
        &classpath,
    );
    common::expect_identical_rejection(&result, "reversed named selected lambda parameters");
}

#[test]
fn same_spelled_dependency_properties_and_constructors_keep_declaration_identity() {
    let Some(library) = common::kotlinc_library(SAME_SPELLED_GENERIC_DECLARATIONS) else {
        eprintln!("skipping: kotlinc unavailable");
        return;
    };
    let classpath = [library, common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(
        &[(
            "Use.kt",
            "import identities.Left\n\
             import identities.Right\n\
             import identities.echo\n\
             fun use(left: Left<String>, right: Right<Int>): String {\n\
             \x20 val fromLeft: String = left.echo\n\
             \x20 val fromRight: Int = right.echo\n\
             \x20 right.echo = fromRight\n\
             \x20 val builtLeft: Left<String> = Left(fromLeft)\n\
             \x20 val builtRight: Right<Int> = Right(fromRight)\n\
             \x20 return builtLeft.value + builtRight.value\n\
             }\n",
        )],
        &classpath,
    );
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the same-spelled declaration fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty conflated same-spelled property or constructor formals"
    );
}

#[test]
fn stable_symbolic_lambda_results_are_inference_evidence() {
    let java = JAVA
        .iter()
        .map(|(name, source)| ((*name).into(), (*source).into()))
        .collect::<Vec<(String, String)>>();
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    for (form, user) in [
        (
            "captured star projection",
            "import pub.Box\n\
             import pub.Mono\n\
             fun use(m: Mono<String>, box: Box<*>) = m.map { box.get() }\n",
        ),
        (
            "enclosing lexical type parameter",
            "import pub.Mono\n\
             fun <T> use(m: Mono<String>, value: T) = m.map { value }\n",
        ),
    ] {
        let result = common::compiler_diagnostics(&[("Use.kt", user)], &classpath);
        assert_eq!(
            (result.reference_code, result.reference_stderr.as_str()),
            (0, ""),
            "kotlinc rejected the {form} control"
        );
        assert_eq!(
            (result.krusty_code, result.krusty_stderr.as_str()),
            (0, ""),
            "krusty rejected the {form} control"
        );
    }
}
