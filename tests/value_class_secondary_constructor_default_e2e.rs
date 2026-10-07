//! Default arguments of a value class's own secondary constructor.
//!
//! A value class's secondary constructor becomes a static `constructor-impl` overload over its own
//! parameters, and one with defaulted parameters also gets `constructor-impl$default(<params>,
//! <masks>, DefaultConstructorMarker)`, which fills each omitted parameter and calls that overload.
//! A call omitting an argument goes through the secondary's own stub, never the primary's.
//!
//! A DEFAULTED value-class parameter whose carrier can hold null stays boxed in that stub (null is
//! its placeholder), so a call supplying it boxes the argument at the stub boundary; any other
//! value-class parameter takes its carrier there, as in the overload itself.

use super::common;

const SRC: &str = "@JvmInline value class Z(val x: Int) {\n\
    \x20   constructor(x: Long = 42L) : this(x.toInt())\n\
    \x20   constructor(a: String, b: Int = a.length) : this(b + 1)\n\
    }\n\
    fun omitted(): Z = Z()\n\
    fun earlier(): Z = Z(\"ab\")\n\
    fun supplied(): Z = Z(\"ab\", 3)\n\
    fun box(): String {\n\
    \x20   if (omitted().x != 42) return \"fail: omitted\"\n\
    \x20   if (earlier().x != 3) return \"fail: earlier parameter\"\n\
    \x20   if (supplied().x != 4) return \"fail: supplied\"\n\
    \x20   return \"OK\"\n\
    }\n";

const NULLABLE_CARRIER_SRC: &str = "@JvmInline value class N(val s: String?)\n\
    @JvmInline value class W(val x: Int) {\n\
    \x20   constructor(n: N, k: Int = 1) : this(if (n.s == null) k else k + 10)\n\
    \x20   constructor(b: Boolean, n: N = N(null), m: Int = 5) : this(if (n.s == null) m else m + 20)\n\
    }\n\
    fun carrierAtStub(): W = W(N(\"a\"))\n\
    fun boxedAtStub(): W = W(true, N(\"a\"))\n\
    fun boxedPlaceholder(): W = W(true)\n\
    fun direct(): W = W(N(\"a\"), 2)\n\
    fun box(): String {\n\
    \x20   if (carrierAtStub().x != 11) return \"fail: carrier at stub\"\n\
    \x20   if (boxedAtStub().x != 25) return \"fail: boxed at stub\"\n\
    \x20   if (boxedPlaceholder().x != 5) return \"fail: boxed placeholder\"\n\
    \x20   if (direct().x != 12) return \"fail: direct\"\n\
    \x20   return \"OK\"\n\
    }\n";

fn assert_same_method(class: &str, method: &str) {
    assert_same_method_in(SRC, class, method);
}

fn assert_same_method_in(source: &str, class: &str, method: &str) {
    match common::class_bytes_diff_against_kotlinc("Main", &[], source, class, method) {
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{diff}"),
        None => panic!("{class}.{method}: reference toolchain unavailable"),
    }
}

#[test]
fn a_secondary_constructor_default_stub_fills_the_omitted_parameter() {
    assert_same_method(
        "Z",
        "public static int constructor-impl$default(long, int, kotlin.jvm.internal.DefaultConstructorMarker)",
    );
}

#[test]
fn a_secondary_constructor_default_reads_an_earlier_parameter() {
    assert_same_method(
        "Z",
        "public static int constructor-impl$default(java.lang.String, int, int, kotlin.jvm.internal.DefaultConstructorMarker)",
    );
}

#[test]
fn an_omitted_argument_calls_the_secondary_default_stub() {
    assert_same_method("MainKt", "public static final int omitted(");
}

#[test]
fn a_partly_omitted_call_passes_the_supplied_argument_and_a_placeholder() {
    assert_same_method("MainKt", "public static final int earlier(");
}

#[test]
fn secondary_constructor_defaults_run() {
    let output = common::compile_and_run_box(SRC, "Main", &[common::stdlib_jar()], None)
        .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}

#[test]
fn an_undefaulted_nullable_carrier_parameter_takes_its_carrier_in_the_default_stub() {
    assert_same_method_in(
        NULLABLE_CARRIER_SRC,
        "W",
        "public static int constructor-impl$default(java.lang.String, int, int, kotlin.jvm.internal.DefaultConstructorMarker)",
    );
}

#[test]
fn a_defaulted_nullable_carrier_parameter_stays_boxed_in_the_default_stub() {
    assert_same_method_in(
        NULLABLE_CARRIER_SRC,
        "W",
        "public static int constructor-impl$default(boolean, N, int, int, kotlin.jvm.internal.DefaultConstructorMarker)",
    );
}

#[test]
fn an_undefaulted_nullable_carrier_argument_reaches_the_default_stub_as_its_carrier() {
    assert_same_method_in(
        NULLABLE_CARRIER_SRC,
        "MainKt",
        "public static final int carrierAtStub(",
    );
}

#[test]
fn a_supplied_defaulted_nullable_carrier_argument_is_boxed_for_the_default_stub() {
    assert_same_method_in(
        NULLABLE_CARRIER_SRC,
        "MainKt",
        "public static final int boxedAtStub(",
    );
}

#[test]
fn an_omitted_nullable_carrier_argument_passes_a_null_box_placeholder() {
    assert_same_method_in(
        NULLABLE_CARRIER_SRC,
        "MainKt",
        "public static final int boxedPlaceholder(",
    );
}

#[test]
fn a_nullable_carrier_argument_calls_the_overload_unboxed_when_nothing_is_omitted() {
    assert_same_method_in(
        NULLABLE_CARRIER_SRC,
        "MainKt",
        "public static final int direct(",
    );
}

#[test]
fn nullable_carrier_secondary_constructor_defaults_run() {
    let output =
        common::compile_and_run_box(NULLABLE_CARRIER_SRC, "Main", &[common::stdlib_jar()], None)
            .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}

/// A class declared in another source file has no `constructor-impl` in the constructing file's
/// IR; its default call is realized over the selected constructor's declared parameters.
#[test]
fn another_files_secondary_constructor_defaults_run() {
    const DECLARATION: &str = "@JvmInline value class Z(val x: Int) {\n\
        \x20   constructor(a: String, b: Int = 2, c: Long = 3L) : this(if (a == \"a\" && c == 3L) b else 0)\n\
        }\n";
    const USE: &str = "fun box(): String {\n\
        \x20   if (Z(\"a\").x != 2) return \"fail: all omitted\"\n\
        \x20   if (Z(\"a\", 5).x != 5) return \"fail: supplied\"\n\
        \x20   return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Z.kt", DECLARATION), ("Main.kt", USE)], "Main");
}
