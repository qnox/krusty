//! A non-null cast of an immutable value reads it again, as kotlinc does.
//!
//! kotlinc's cast lowering checks `x as T` for `null` before casting. When `x` reads an immutable
//! binding, it reads `x` once for the check and again for the cast (`aload; ldc; checkNotNull;
//! aload; checkcast`); anything else is duplicated (`dup; ldc; checkNotNull; checkcast`). Common IR
//! records binding stability on the read; the JVM backend chooses the stack shape.
use std::fmt::Write;

use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

const SOURCE: &str = "class CastParts(val first: Any?, val second: Int) {\n\
    \x20   operator fun component1(): Any? = first\n\
    \x20   operator fun component2(): Int = second\n\
    }\n\
    fun supplied(parts: CastParts): CastParts = parts\n\
    fun consume(value: Int) {}\n\
    fun make(): Any? = \"c\"\n\
    fun parameter(a: Any?): String = a as String\n\
    fun valLocal(a: Any?): String {\n\
    \x20   val v = a\n\
    \x20   return v as String\n\
    }\n\
    fun varLocal(a: Any?, b: Any?): String {\n\
    \x20   var v = a\n\
    \x20   v = b\n\
    \x20   return v as String\n\
    }\n\
    fun destructuredVal(parts: CastParts): String {\n\
    \x20   val (v, marker) = supplied(parts)\n\
    \x20   consume(marker)\n\
    \x20   return v as String\n\
    }\n\
    fun destructuredVar(parts: CastParts, b: Any?): String {\n\
    \x20   var (v, marker) = supplied(parts)\n\
    \x20   consume(marker)\n\
    \x20   v = b\n\
    \x20   return v as String\n\
    }\n\
    fun loopValue(values: Array<Any?>): String {\n\
    \x20   for (v in values) return v as String\n\
    \x20   return \"empty\"\n\
    }\n\
    fun catchValue(failure: Throwable): IllegalStateException {\n\
    \x20   try { throw failure }\n\
    \x20   catch (caught: Throwable) { return caught as IllegalStateException }\n\
    }\n\
    inline fun inlineCast(value: Any?): String = value as String\n\
    fun inlined(a: Any?): String = inlineCast(a)\n\
    fun call(): String = make() as String\n";

#[test]
fn an_immutable_cast_operand_is_read_again_like_kotlincs() {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    let Some(built) = compare_with_kotlinc_plugin(
        "CastOperandReread",
        SOURCE,
        "CastOperandRereadKt",
        &classpath,
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // Ordinary/destructured vars and calls keep the `dup`; every immutable binding form reloads.
    for member in [
        "String parameter(",
        "String valLocal(",
        "String varLocal(",
        "String destructuredVal(",
        "String destructuredVar(",
        "String loopValue(",
        "IllegalStateException catchValue(",
        "String call(",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

#[test]
fn a_high_slot_immutable_cast_uses_kotlincs_wide_reload() {
    let mut source = String::from("fun highSlot(a: Any?): String {\n");
    for index in 0..260 {
        writeln!(source, "    val value{index}: Any? = a").expect("write source");
    }
    source.push_str("    return value259 as String\n}\n");

    let Some(built) = compare_with_kotlinc_plugin(
        "HighSlotCastOperandReread",
        &source,
        "HighSlotCastOperandRereadKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let reference = method_instructions(&built.reference, "String highSlot(");
    assert!(
        reference.iter().any(|row| row.contains("aload_w")),
        "fixture did not allocate the cast operand above slot 255: {reference:?}"
    );
    assert_eq!(
        method_instructions(&built.krusty, "String highSlot("),
        reference
    );
}

#[test]
fn an_immutable_cast_operand_read_again_still_casts() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   val parts = CastParts(\"d\", 0)\n\
             \x20   if (parameter(\"a\") != \"a\" || valLocal(\"b\") != \"b\") return \"read\"\n\
             \x20   if (destructuredVal(parts) != \"d\") return \"destructure val\"\n\
             \x20   if (destructuredVar(parts, \"m\") != \"m\") return \"destructure var\"\n\
             \x20   if (loopValue(arrayOf(\"l\")) != \"l\") return \"loop\"\n\
             \x20   val failure = IllegalStateException(\"caught\")\n\
             \x20   if (catchValue(failure) !== failure) return \"catch\"\n\
             \x20   if (inlined(\"i\") != \"i\") return \"inline\"\n\
             \x20   if (varLocal(null, \"x\") != \"x\" || call() != \"c\") return \"dup\"\n\
             \x20   try {{\n\
             \x20       parameter(null)\n\
             \x20       return \"null\"\n\
             \x20   }} catch (e: NullPointerException) {{\n\
             \x20   }}\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "cast operand read again",
    );
}
