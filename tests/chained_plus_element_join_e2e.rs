//! A chain of `+` over lists whose elements are siblings of one hierarchy.
//!
//! `operator fun <T> Collection<T>.plus(elements: Iterable<T>): List<T>` shares one `T` across the
//! receiver, the argument and the result, so `T` has to be the JOIN of both operands. Two operands
//! worked; three did not:
//!
//! ```text
//! a.map { A(it) } + b.map { B(it) }                     ok
//! a.map { A(it) } + b.map { B(it) } + a.map { C(it) }   error: actual 'List<B>', but 'Iterable<A>'
//! ```
//!
//! The inner sum reaches the outer call as a nested generic call, which arrives as a provisional
//! result rather than a final type and is excluded from inference — so the shared `T` stayed pinned
//! at the receiver's element type and the argument was judged against `Iterable<A>`.
//!
//! The exclusion is right in general: `ArrayList()` reads provisionally as `ArrayList<Any>` and
//! `"k" to emptySet()` as `Pair<String, B>`, and letting either widen the formal discards the type
//! the enclosing expectation would have supplied. What separates them is whether the nested call's
//! own inputs FIX its result — `xs.map { B(it) }` binds its `R` from the transform it was given —
//! and whether the provisional is actually determined, which a result still mentioning a type
//! parameter is not.
use super::common;

const HIERARCHY: &str = "interface P\n\
                         class A(val v: Int) : P\n\
                         class B(val v: Int) : P\n\
                         class C(val v: Int) : P\n\
                         class Cfg(val a: List<Int>, val b: List<Int>, val c: Int?)\n";

fn diagnostics(body: &str) -> Vec<String> {
    let jdk = common::jdk_modules();
    let source = format!("{HIERARCHY}{body}");
    common::front_end_diagnostics(&source, &[common::stdlib_jar()], Some(jdk.as_path()))
}

/// The corpus shape: three summands, the last a `listOfNotNull` of optional entries.
#[test]
fn three_summands_join_their_element_types() {
    const BODY: &str = "fun sel(g: Cfg): List<P> =\n\
        \x20   g.a.map { A(it) } +\n\
        \x20       g.b.map { B(it) } +\n\
        \x20       listOfNotNull(g.c?.let { C(it) })\n";
    assert_eq!(diagnostics(BODY), Vec::<String>::new());
}

/// The same chain with no expected type at all, where the join is the only thing deciding `T`.
#[test]
fn a_chain_with_no_expected_type_still_joins() {
    const BODY: &str = "fun sel(g: Cfg) =\n\
        \x20   g.a.map { A(it) } + g.b.map { B(it) } + g.a.map { C(it) }\n";
    assert_eq!(diagnostics(BODY), Vec::<String>::new());
}

/// Two summands already worked; keep them working.
#[test]
fn two_summands_join_their_element_types() {
    const BODY: &str = "fun sel(g: Cfg): List<P> = g.a.map { A(it) } + g.b.map { B(it) }\n";
    assert_eq!(diagnostics(BODY), Vec::<String>::new());
}

/// The control that keeps the rule honest: elements with no common declared supertype join to
/// `Any`, and a declared `List<P>` return still rejects it — kotlinc reports exactly this.
#[test]
fn unrelated_elements_still_join_to_any_and_are_rejected() {
    const BODY: &str = "fun bad(): List<P> = listOf(A(1)) + listOf(1)\n";
    assert_eq!(
        diagnostics(BODY),
        vec!["return type mismatch: expected 'List<P>', actual 'List<Any>'.".to_string()]
    );
}

/// The second control: a nested call whose result its OWN inputs do not fix must stay excluded.
/// `"k" to emptySet()` reads as `Pair<String, B>` with `B` still free, and only the expectation can
/// finish it — feeding that to inference is what makes the map's value type collapse to `Any`.
#[test]
fn an_undetermined_nested_call_is_not_evidence() {
    const BODY: &str = "fun use(map: MutableMap<Any, Set<Any>>) { map[\"k\"] = emptySet() }\n\
        fun build(): Map<String, Set<Any>> = mapOf(\"k\" to emptySet())\n";
    assert_eq!(diagnostics(BODY), Vec::<String>::new());
}
