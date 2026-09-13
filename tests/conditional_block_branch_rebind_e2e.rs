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
        "Mono.java",
        "package pub;\n\
         import java.util.function.Function;\n\
         public final class Mono<T> implements Pub<T> {\n\
         \x20 public static <T> Mono<T> empty() { return new Mono<T>(); }\n\
         \x20 public static <T> Mono<T> just(T value) { return new Mono<T>(); }\n\
         \x20 public <R> Mono<R> flatMap(Function<? super T, ? extends Mono<? extends R>> f) {\n\
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
