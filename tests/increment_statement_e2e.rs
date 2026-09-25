//! `i++`, `++i` and `i--` on an `Int` local in statement position are `iinc`, exactly like
//! `i += 1`: the increment carries its checked result type as an identity coercion around the same
//! arithmetic, and that coercion is no reason to fall back to load, add and store.
use super::common;

#[test]
fn local_increments_are_byte_identical_to_kotlinc() {
    let src = "\
fun steps(k: Int): Int {
    var n = 0
    n += 1
    n++
    ++n
    n--
    --n
    while (n < k) n++
    if (k > 3) n++
    return n
}
";
    match common::byte_diff_against_kotlinc_cp(
        "Increments",
        src,
        "IncrementsKt",
        &[common::stdlib_jar()],
    ) {
        None => eprintln!("skip (Increments: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(why)) => panic!("{why}"),
    }
}
