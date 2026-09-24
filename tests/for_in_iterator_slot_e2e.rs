//! A `for` over an `Iterable` stores only the iterator. Kotlin desugars `for (e in xs)` to
//! `val it = xs.iterator(); while (it.hasNext()) { val e = it.next() … }`, so the subject is
//! evaluated once straight into `iterator()` and takes no local slot. Spilling the subject into a
//! temporary first shifted the iterator and every loop local up one slot from kotlinc's layout.
use super::common;

#[test]
fn iterable_loops_are_byte_identical_to_kotlinc() {
    let src = "\
fun total(xs: Iterable<String>): Int {
    var n = 0
    for (e in xs) {
        n += e.length
    }
    return n
}

fun last(xs: List<String>): String {
    var r = \"\"
    for (e in xs) {
        r = e
    }
    return r
}

fun make(): List<String> = listOf(\"a\")

fun fromCall(): Int {
    var n = 0
    for (e in make()) {
        n += e.length
    }
    return n
}
";
    match common::byte_diff_against_kotlinc_cp(
        "IterableLoops",
        src,
        "IterableLoopsKt",
        &[common::stdlib_jar()],
    ) {
        None => eprintln!("skip (IterableLoops: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(why)) => panic!("{why}"),
    }
}

/// The subject is evaluated once, before the first iteration: reassigning the variable it was read
/// from inside the body does not change what the loop walks.
#[test]
fn iterable_loop_subject_is_evaluated_once() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let src = "\
var calls = 0

fun subject(): List<Int> {
    calls++
    return listOf(1, 2, 3)
}

fun box(): String {
    var xs = listOf(1, 2)
    var sum = 0
    for (x in xs) {
        xs = listOf(10, 20, 30)
        sum += x
    }
    if (sum != 3) return \"FAIL reassigned: $sum\"
    for (x in subject()) {
        sum += x
    }
    if (calls != 1 || sum != 9) return \"FAIL call: $calls $sum\"
    return \"OK\"
}
";
    let out = common::compile_and_run_box(
        src,
        "IterableLoopSubject",
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(out.as_deref(), Some("OK"));
}
