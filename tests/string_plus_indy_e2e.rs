//! `String.plus` on a JVM 9+ target: kotlinc lowers `a + b` on strings into ONE
//! `invokedynamic makeConcatWithConstants` (the same `StringConcatFactory` shape a template gets),
//! folding literal operands into the recipe. krusty answered every `+` with a `StringBuilder`
//! chain regardless of the target — the class then differs on every string concatenation in a
//! Gradle-built (JDK 17/21/25 toolchain) module. Measured byte-for-byte against kotlinc 2.4.10.

use super::common;

fn assert_byte_identical_at(name: &str, src: &str, class: &str, target: &str) {
    match common::byte_diff_against_kotlinc_cp_target(
        name,
        src,
        class,
        &[common::stdlib_jar()],
        Some(target),
    ) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

#[test]
fn string_plus_string_is_indy_on_jvm9_plus() {
    assert_byte_identical_at(
        "spiPlusString",
        "fun f(a: String, b: String): String = a + b\n",
        "SpiPlusStringKt",
        "25",
    );
}

#[test]
fn string_plus_any_and_primitive_operands_are_indy_on_jvm9_plus() {
    assert_byte_identical_at(
        "spiPlusAny",
        "fun g(a: String, b: Any?): String = a + b\n\
         fun h(a: String, i: Int): String = a + i\n\
         fun l(a: String, x: Long, d: Double): String = a + x + d\n",
        "SpiPlusAnyKt",
        "25",
    );
}

#[test]
fn string_plus_chain_with_literals_folds_into_one_recipe() {
    assert_byte_identical_at(
        "spiPlusChain",
        "fun k(a: String, b: String): String = \"x\" + a + \"-\" + b + \"y\"\n",
        "SpiPlusChainKt",
        "25",
    );
}

#[test]
fn string_plus_stays_string_builder_on_jvm8() {
    assert_byte_identical_at(
        "spiPlusLegacy",
        "fun f(a: String, b: String): String = a + b\n",
        "SpiPlusLegacyKt",
        "1.8",
    );
}
