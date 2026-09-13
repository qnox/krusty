//! A type parameter that appears in BOTH the receiver and an argument takes the join of the two.
//!
//! `operator fun <T> Collection<T>.plus(elements: Iterable<T>): List<T>` puts `T` in two covariant
//! input positions. `servers + infra`, with `List<Server>` and `List<Infra>` under a common sealed
//! parent, therefore infers `T = Op` — both sides are LOWER bounds on `T`, so the answer is their
//! least upper bound. krusty pinned `T` from the receiver alone and then rejected the argument as
//! `List<Infra>` where `Iterable<Server>` was expected, which is not a type kotlinc ever forms here.
//!
//! Concatenating heterogeneous branches of a sealed hierarchy is ordinary Kotlin, and one such
//! expression cost a corpus module every class it would have emitted.

use super::common;

const SEALED: &str = "sealed class Op {\n\
    \x20 data class Server(val n: String) : Op()\n\
    \x20 data class Infra(val n: String) : Op()\n\
    }\n";

fn assert_both_accept(user: &str, what: &str) {
    let source = format!("{SEALED}{user}");
    let result =
        common::compiler_diagnostics(&[("Join.kt", source.as_str())], &[common::stdlib_jar()]);
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

/// The declared result supplies the join, so the expectation alone could explain it.
#[test]
fn plus_joins_the_receiver_and_the_argument_against_an_expected_type() {
    assert_both_accept(
        "fun combine(servers: List<Op.Server>, infra: List<Op.Infra>): List<Op> = servers + infra\n",
        "`plus` with an expected result",
    );
}

/// With NO expected type the join has to come from the two inputs themselves.
#[test]
fn plus_joins_the_receiver_and_the_argument_without_an_expected_type() {
    assert_both_accept(
        "fun use(a: List<Op.Server>, b: List<Op.Infra>) {\n\
         \x20 val all = a + b\n\
         \x20 println(all)\n\
         }\n",
        "`plus` with no expected result",
    );
}

/// Chained, so the second `plus` sees an already-joined receiver.
#[test]
fn a_chain_of_joins_keeps_widening() {
    assert_both_accept(
        "fun combine(a: List<Op.Server>, b: List<Op.Infra>, c: List<Op.Infra>): List<Op> =\n\
         \x20 a + b + c\n",
        "a chained `plus`",
    );
}

/// Control: unrelated element types still join — to `Any`, which is what kotlinc infers — so the
/// fix widens rather than forcing a common supertype that does not exist.
#[test]
fn unrelated_element_types_join_to_any() {
    assert_both_accept(
        "fun use(a: List<String>, b: List<Int>) {\n\
         \x20 val all: List<Any> = a + b\n\
         \x20 println(all)\n\
         }\n",
        "unrelated element types",
    );
}

/// Control: a narrower DECLARED result is still an error — the join must not be accepted as any
/// element type the caller names.
#[test]
fn a_narrower_declared_result_is_still_rejected() {
    let source = format!(
        "{SEALED}fun combine(a: List<Op.Server>, b: List<Op.Infra>): List<Op.Server> = a + b\n"
    );
    let result =
        common::compiler_diagnostics(&[("Narrow.kt", source.as_str())], &[common::stdlib_jar()]);
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject a result narrower than the join"
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty must reject a result narrower than the join"
    );
}

/// The runtime contract: the joined list really holds both branches, in order.
#[test]
fn the_joined_list_holds_both_branches() {
    common::expect_box_ok_files_with_stdlib(
        &[(
            "Join.kt",
            "sealed class Op {\n\
             \x20 data class Server(val n: String) : Op()\n\
             \x20 data class Infra(val n: String) : Op()\n\
             }\n\
             fun combine(a: List<Op.Server>, b: List<Op.Infra>): List<Op> = a + b\n\
             fun box(): String {\n\
             \x20 val all = combine(listOf(Op.Server(\"s\")), listOf(Op.Infra(\"i\")))\n\
             \x20 val rendered = all.joinToString(\",\") {\n\
             \x20\x20 when (it) {\n\
             \x20\x20\x20 is Op.Server -> \"S:${it.n}\"\n\
             \x20\x20\x20 is Op.Infra -> \"I:${it.n}\"\n\
             \x20\x20 }\n\
             \x20 }\n\
             \x20 return if (rendered == \"S:s,I:i\") \"OK\" else rendered\n\
             }\n",
        )],
        "ReceiverAndArgumentJoin",
    );
}

// DELIBERATELY NOT COVERED: the argument being a NESTED generic call whose result awaits this very
// parameter — `a + xs.map { Op.Infra(it) }`. Such an argument is a `CallArgKind::ExpectedTypeCallable`
// and is excluded from this unification on purpose: its provisional result is not evidence yet.
// `ArrayList()` reads provisionally as `ArrayList<Any>` and `emptySet()` as `Set<T>`, and feeding
// either into the join widens the formal and then LOSES the element type the enclosing expectation
// would have supplied. Letting it contribute regresses
// `fir::body_check::call_tests::collection_plus_contextualizes_a_generic_constructor_argument_through_its_supertype`
// and `fir::body_check::assignment_tests::plus_assign_contextually_types_nested_generic_rhs_calls`.
// Solving it properly means running the nested call's own constraints and the outer join as ONE
// system, which is a separate change.
