//! A value class whose underlying erases to `Object` (`value class Slot(val raw: Any?)`) read out of
//! a type-parameter slot: a repository-owned `keep<T>`/`Container<T>` path as well as
//! `Iterator<Slot>.next()`, `List<Slot>.get`, and a `for` loop over `Iterable<Slot>`. The declaration
//! returns a bare `T`, so the slot holds the BOX and the read unboxes it. The descriptor
//! `()Ljava/lang/Object;` cannot tell that box from the carrier when the carrier is `Object` too;
//! treating it as the carrier handed the box itself to `raw`, so the loop printed `Slot(raw=a)`
//! where kotlinc prints `a`. The custom path also pins the opposite edge: `Slot` must be boxed when
//! entering `keep`'s bare-`T` parameter before that same box returns through its result slot, and
//! when entering `Container<T>`'s constructor before its generic property getter returns it.
use super::common;

const ELEMENTS: &str = "\
@JvmInline
value class Slot(val raw: Any?)

@JvmInline
value class Tagged<out A>(val raw: Any?)

class Container<T>(val value: T)

fun <T> keep(value: T): T = value

fun keptRaw(value: Slot): Any? = keep(value).raw

fun containedRaw(value: Slot): Any? = Container(value).value.raw

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
    if (keptRaw(Slot(\"kept\")) != \"kept\") return \"FAIL kept\"
    if (containedRaw(Slot(\"contained\")) != \"contained\") return \"FAIL contained\"
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

class Container<T>(val value: T)

fun <T> keep(value: T): T = value

fun keptRaw(value: Slot): Any? = keep(value).raw

fun containedRaw(value: Slot): Any? = Container(value).value.raw

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
