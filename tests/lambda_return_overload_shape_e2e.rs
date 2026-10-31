//! `@OverloadResolutionByLambdaReturnType` families such as `flatMap` over `(T) -> Iterable<R>`
//! and `(T) -> Sequence<R>` take the same lambda inputs and differ only in the lambda's result.
//! The lambda body chooses the overload, so it is not checked against the first candidate's
//! result type, with or without an expected call result, and inside a SAM-converted lambda.
//!
//! The refinement is the specification's, not a general rule: a tie without the annotation is an
//! ambiguity, a unique most specific candidate keeps its own result for the lambda, and only the
//! nearest callable-tower rung competes.

use super::common;
use crate::diagnostics_parity_support::errors;

const SOURCE: &str = "fun interface Languages { fun of(x: Int): Sequence<String> }\n\
fun sequenceBody(s: Sequence<Int>): Sequence<String> = s.flatMap { sequenceOf(\"s$it\") }\n\
fun iterableBody(s: Sequence<Int>): Sequence<String> = s.flatMap { listOf(\"i$it\") }\n\
fun inferred(s: Sequence<Int>) = s.flatMap { p -> sequenceOf(\"n$p\") }\n\
fun listOfSequences(s: List<Int>): List<String> = s.flatMap { sequenceOf(\"l$it\") }\n\
fun combined(providers: Sequence<Languages>): Languages = Languages { x ->\n\
    providers.flatMap { provider ->\n\
        provider.of(x)\n\
    }\n\
}\n\
fun box(): String {\n\
    val s = sequenceOf(1, 2)\n\
    val all = sequenceBody(s).toList() + iterableBody(s).toList() + inferred(s).toList() +\n\
        listOfSequences(listOf(3)) +\n\
        combined(sequenceOf(Languages { sequenceOf(\"a$it\") })).of(4).toList()\n\
    return if (all.joinToString() == \"s1, s2, i1, i2, n1, n2, l3, a4\") \"OK\" else all.joinToString()\n\
}\n";

#[test]
fn a_lambda_body_selects_its_result_overload_like_kotlinc() {
    common::expect_box_ok_with_stdlib(SOURCE, "LambdaReturnOverload");
    let classes = common::classes_against_kotlinc_module(&[("LambdaReturnOverload.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

/// Each diagnostic as its message lines. kotlinc follows every message with the source line and
/// a caret line, which krusty does not print.
fn diagnostic_blocks(output: &str, echoes_source: bool) -> Vec<Vec<String>> {
    let mut blocks: Vec<Vec<String>> = Vec::new();
    for line in output.lines() {
        if line.starts_with("krusty: ") {
            continue;
        }
        if line.contains(": error: ") || line.contains(": warning: ") {
            blocks.push(vec![line
                .rsplit_once(".kt:")
                .map_or(line, |(_, at)| at)
                .to_string()]);
        } else if let Some(block) = blocks.last_mut() {
            block.push(line.to_string());
        }
    }
    if echoes_source {
        for block in &mut blocks {
            block.truncate(block.len().saturating_sub(2));
        }
    }
    blocks
}

fn assert_diagnostics_match_kotlinc(file: &str, source: &str, expected: &[&[&str]]) {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(&[(file, source)], std::slice::from_ref(&stdlib));
    assert_ne!(result.krusty_code, 0, "krusty accepted {file}");
    assert_ne!(result.reference_code, 0, "kotlinc accepted {file}");
    let mut krusty_output = result.krusty_stderr.clone();
    krusty_output.push_str(&result.krusty_stdout);
    assert_eq!(errors(&krusty_output), errors(&result.reference_stderr));
    let krusty = diagnostic_blocks(&krusty_output, false);
    assert_eq!(krusty, diagnostic_blocks(&result.reference_stderr, true));
    assert_eq!(
        krusty,
        expected
            .iter()
            .map(|block| block
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );
}

#[test]
fn an_unannotated_result_overload_tie_is_ambiguous_like_kotlinc() {
    // The distinct JVM names avoid a descriptor clash.
    let source = "@JvmName(\"pickString\")\n\
fun Int.pick(f: (Int) -> String): String = \"s\"\n\
@JvmName(\"pickInt\")\n\
fun Int.pick(f: (Int) -> Int): String = \"i\"\n\
fun bad() = 1.pick { it.toString() }\n\
fun named() = 1.pick { x -> x.toString() }\n\
fun declared(): String = 1.pick { x: Int -> x + 1 }\n";
    const CANDIDATES: [&str; 2] = [
        "fun Int.pick(f: (Int) -> String): String",
        "fun Int.pick(f: (Int) -> Int): String",
    ];
    assert_diagnostics_match_kotlinc(
        "UnannotatedPick.kt",
        source,
        &[
            &[
                "5:15: error: overload resolution ambiguity between candidates:",
                CANDIDATES[0],
                CANDIDATES[1],
            ],
            &["5:22: error: unresolved reference 'it'."],
            &[
                "6:17: error: overload resolution ambiguity between candidates:",
                CANDIDATES[0],
                CANDIDATES[1],
            ],
            &["6:24: error: cannot infer type for value parameter 'x'. Specify it explicitly."],
            &[
                "7:28: error: overload resolution ambiguity between candidates:",
                CANDIDATES[0],
                CANDIDATES[1],
            ],
        ],
    );
}

#[test]
fn a_unique_most_specific_candidate_keeps_its_result_like_kotlinc() {
    // `(Int) -> String` is more specific than `(Int) -> CharSequence`, so the lambda is checked
    // against `String` and is not refined to the `CharSequence` overload its body would fit.
    let source = "@file:OptIn(kotlin.experimental.ExperimentalTypeInference::class)\n\
@OverloadResolutionByLambdaReturnType\n\
@JvmName(\"pickChars\")\n\
fun Int.pick(f: (Int) -> CharSequence): String = \"c\"\n\
@OverloadResolutionByLambdaReturnType\n\
@JvmName(\"pickString\")\n\
fun Int.pick(f: (Int) -> String): String = \"s\"\n\
fun good() = 1.pick { StringBuilder(it) }\n";
    assert_diagnostics_match_kotlinc(
        "WinnerPick.kt",
        source,
        &[&["8:23: error: return type mismatch: expected 'String', actual 'StringBuilder'."]],
    );
}

#[test]
fn an_annotated_result_overload_tie_is_refined_by_the_lambda_body_like_kotlinc() {
    // Each overload lives in its own file facade, so the two need no distinct JVM names.
    let string_pick = "@file:OptIn(kotlin.experimental.ExperimentalTypeInference::class)\n\
\n\
@OverloadResolutionByLambdaReturnType\n\
fun Int.pick(f: (Int) -> String): String = \"s\" + f(this)\n";
    let int_pick = "@file:OptIn(kotlin.experimental.ExperimentalTypeInference::class)\n\
\n\
@OverloadResolutionByLambdaReturnType\n\
fun Int.pick(f: (Int) -> Int): String = \"i\" + f(this)\n";
    let main = "fun box(): String {\n\
    val picked = 1.pick { it + 1 } + 1.pick { it.toString() }\n\
    return if (picked == \"i2s1\") \"OK\" else picked\n\
}\n";
    let sources = [
        ("StringPick.kt", string_pick),
        ("IntPick.kt", int_pick),
        ("AnnotatedPick.kt", main),
    ];
    common::expect_box_ok_files_with_stdlib(&sources, "AnnotatedPick");
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "AnnotatedPickKt"),
        "OK"
    );
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_nearer_scope_rung_keeps_its_result_context_like_kotlinc() {
    // An explicit import and a current-package declaration both sit at rank zero of their own
    // scope rung, at the same receiver distance. The explicitly imported `List` overload is
    // nearer, so `emptyList()` is checked against `List<String>` and the farther `Set` overload
    // never competes for the lambda's result.
    let near = "package lib\n\
\n\
fun Int.pick(f: (Int) -> List<String>): String = \"near\" + f(this).size\n";
    let main = "package app\n\
\n\
import lib.pick\n\
\n\
fun Int.pick(f: (Int) -> Set<String>): String = \"far\" + f(this).size\n\
\n\
fun box(): String {\n\
    val picked = 1.pick { emptyList() }\n\
    return if (picked == \"near0\") \"OK\" else picked\n\
}\n";
    let sources = [("Near.kt", near), ("Main.kt", main)];
    common::expect_box_ok_files_with_stdlib(&sources, "NearerRungPick");
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "app.MainKt"),
        "OK"
    );
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

// The overloads differ in their JVM descriptors, so they need no distinct JVM names.
const PRIORITY_DECLARATIONS: &str =
    "fun Int.pick(f: (Int) -> String, suffix: String = \"!\"): String = \"d\" + f(this) + suffix\n\
fun Int.pick(f: (Int) -> Int): String = \"p\" + f(this)\n\
fun Int.tag(f: (Int) -> String, vararg extra: Int): String = \"v\" + f(this) + extra.size\n\
fun Int.tag(f: (Int) -> Int): String = \"t\" + f(this)\n";

#[test]
fn an_ordinary_winner_by_defaults_or_varargs_shapes_the_lambda_like_kotlinc() {
    // Both pairs tie on the lambda's parameter alone. Ordinary selection prefers the candidate
    // that omits no default and the one without a vararg, so their lambda results shape the
    // lambdas before any refinement is considered.
    let source = format!(
        "{PRIORITY_DECLARATIONS}fun box(): String {{\n\
    val r = 1.pick {{ it + 1 }} + 2.tag {{ it * 3 }}\n\
    return if (r == \"p2t6\") \"OK\" else r\n\
}}\n"
    );
    common::expect_box_ok_with_stdlib(&source, "PriorityPick");
    let classes = common::classes_against_kotlinc_module(&[("PriorityPick.kt", &source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn an_ordinary_winner_by_defaults_or_varargs_keeps_its_result_like_kotlinc() {
    let source = format!(
        "{PRIORITY_DECLARATIONS}fun bad() = 1.pick {{ it.toString() }}\n\
fun worse() = 2.tag {{ \"x$it\" }}\n"
    );
    assert_diagnostics_match_kotlinc(
        "PriorityMismatch.kt",
        &source,
        &[
            &["5:22: error: return type mismatch: expected 'Int', actual 'String'."],
            &["6:23: error: return type mismatch: expected 'Int', actual 'String'."],
        ],
    );
}
