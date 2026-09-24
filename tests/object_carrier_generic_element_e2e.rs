//! A value class whose underlying erases to `Object` (`value class Slot(val raw: Any?)`) read out of
//! a type-parameter slot: `Iterator<Slot>.next()`, `List<Slot>.get`, a `for` loop over
//! `Iterable<Slot>`. The declaration returns a bare `T`, so the slot holds the BOX and the read
//! unboxes it. The descriptor `()Ljava/lang/Object;` cannot tell that box from the carrier when the
//! carrier is `Object` too; treating it as the carrier handed the box itself to `raw`, so the loop
//! printed `Slot(raw=a)` where kotlinc prints `a`.
use super::common;

const ELEMENTS: &str = "\
@JvmInline
value class Slot(val raw: Any?)

@JvmInline
value class Tagged<out A>(val raw: Any?)

fun joined(xs: Iterable<Slot>): String {
    var r = \"\"
    for (e in xs) {
        r += e.raw
    }
    return r
}

fun <A> present(xs: Iterable<Tagged<A>>): Int {
    var n = 0
    for (e in xs) {
        if (e.raw != null) n++
    }
    return n
}

fun second(xs: List<Slot>): Any? = xs[1].raw

fun box(): String {
    val slots = listOf(Slot(\"a\"), Slot(null))
    if (joined(slots) != \"anull\") return \"FAIL joined: \" + joined(slots)
    if (present(listOf(Tagged<Int>(1), Tagged<Int>(null))) != 1) return \"FAIL present\"
    if (second(slots) != null) return \"FAIL second\"
    return \"OK\"
}
";

#[test]
fn object_carrier_value_class_elements_are_unboxed() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let out = common::compile_and_run_box(
        ELEMENTS,
        "ObjectCarrierElements",
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(out.as_deref(), Some("OK"));
}

#[test]
fn object_carrier_value_class_element_reads_match_kotlinc() {
    const SOURCE: &str = "\
@JvmInline
value class Slot(val raw: Any?)

@JvmInline
value class Tagged<out A>(val raw: Any?)

fun second(xs: List<Slot>): Any? = xs[1].raw

fun nextRaw(xs: Iterator<Slot>): Any? {
    val e = xs.next()
    return e.raw
}

fun <A> firstRaw(xs: List<Tagged<A>>): Any? = xs.first().raw
";
    match common::byte_diff_against_kotlinc_cp(
        "ObjectCarrierElement",
        SOURCE,
        "ObjectCarrierElementKt",
        &[common::stdlib_jar()],
    ) {
        None => eprintln!("skip (ObjectCarrierElement: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(why)) => panic!("{why}"),
    }
}
