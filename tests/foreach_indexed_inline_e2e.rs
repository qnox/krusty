//! `forEachIndexed { i, v -> … }` — the same iterator loop `forEach` expands to, plus the index
//! counter its lambda's first parameter reads.
//!
//! krusty expands a recognized inline iteration at IR level, which is what lets a suspension inside
//! the lambda join the CALLER's state machine. `CompilerIntrinsic::ForEachIndexed` was decoded by the
//! provider and typed by the resolver, and the legacy lowerer expanded it, but the checked-FIR path —
//! the production one — published a structural plan for `forEach` alone. `forEachIndexed` therefore
//! kept its lambda as a real function object, a suspend call inside it never received a continuation,
//! and emission failed with "call arity mismatch", which bails the whole FILE.

use super::common;

fn run(src: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    common::compile_and_run_box(
        src,
        "Main",
        &[stdlib, coroutines, jdk.clone()],
        Some(jdk.as_path()),
    )
}

/// The failing shape: a SUSPENDING call inside the `forEachIndexed` lambda.
#[test]
fn for_each_indexed_hosts_a_suspension_in_its_lambda() {
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun twice(value: Int): Int = value * 2\n\
        fun box(): String = runBlocking {\n\
        \x20   val seen = StringBuilder()\n\
        \x20   listOf(10, 20, 30).forEachIndexed { index, value ->\n\
        \x20       seen.append(\"$index:${twice(value)};\")\n\
        \x20   }\n\
        \x20   if (seen.toString() == \"0:20;1:40;2:60;\") \"OK\" else \"F:$seen\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("forEachIndexed hosting a suspension compiles and runs"),
        "OK"
    );
}

/// The index starts at zero and advances once per element, independently of the element values —
/// which is what distinguishes the expansion from `forEach` over an already-indexed sequence.
#[test]
fn for_each_indexed_counts_from_zero_over_every_element() {
    const SRC: &str = "fun box(): String {\n\
        \x20   val seen = StringBuilder()\n\
        \x20   listOf(\"a\", \"b\", \"c\").forEachIndexed { index, value -> seen.append(\"$index$value\") }\n\
        \x20   return if (seen.toString() == \"0a1b2c\") \"OK\" else \"F:$seen\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("forEachIndexed numbers its elements from zero"),
        "OK"
    );
}

/// An empty receiver runs the lambda no times and leaves the counter unused.
#[test]
fn for_each_indexed_over_an_empty_receiver_runs_nothing() {
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun twice(value: Int): Int = value * 2\n\
        fun box(): String = runBlocking {\n\
        \x20   var calls = 0\n\
        \x20   emptyList<Int>().forEachIndexed { _, value -> calls += twice(value) }\n\
        \x20   if (calls == 0) \"OK\" else \"F:$calls\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("forEachIndexed over an empty receiver runs its lambda no times"),
        "OK"
    );
}

/// Control: `forEach` over the same receiver keeps working — the index must be added to the loop,
/// not substituted for the element.
#[test]
fn for_each_still_binds_its_element() {
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun twice(value: Int): Int = value * 2\n\
        fun box(): String = runBlocking {\n\
        \x20   var total = 0\n\
        \x20   listOf(1, 2, 3).forEach { value -> total += twice(value) }\n\
        \x20   if (total == 12) \"OK\" else \"F:$total\"\n\
        }\n";
    assert_eq!(run(SRC).expect("forEach keeps binding its element"), "OK");
}
