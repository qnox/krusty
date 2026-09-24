//! An inlined library call reads its operands where they already are, as kotlinc does.
//!
//! A library inline function is expanded from its retained bytecode, and that expansion stores each
//! operand into the callee's own parameter slot. Lowering also copied every plain local or constant
//! operand into a caller local first, whenever no inline lambda was among the operands, so the value
//! was stored twice. kotlinc stores it once at most: `maxOf(a, b)` is `iload_0; iload_1;
//! invokestatic Math.max`.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

const SOURCE: &str = "fun larger(first: Int, second: Int): Int = maxOf(first, second)\n\
    fun smaller(first: Long, second: Long): Long = minOf(first, second)\n";

fn opcode(instruction: &str) -> &str {
    instruction
        .split_once(": ")
        .map_or(instruction, |(_, rest)| rest)
        .split_whitespace()
        .next()
        .unwrap_or("")
}

#[test]
fn an_inlined_call_pushes_its_operands_from_where_they_are_like_kotlinc() {
    let Some(built) = compare_with_kotlinc_plugin(
        "InlineOperandCopies",
        SOURCE,
        "InlineOperandCopiesKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in ["int larger(", "long smaller("] {
        let reference = method_instructions(&built.reference, member);
        let krusty = method_instructions(&built.krusty, member);
        assert!(
            !reference.is_empty() && !krusty.is_empty(),
            "{member} not found"
        );
        // The common-lowering copies and the expansion's own parameter store are both gone: the
        // complete instruction stream now matches kotlinc's in-place `@InlineOnly` expansion.
        let pushes = |body: &[String]| {
            body.iter()
                .take(2)
                .map(|insn| opcode(insn).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(pushes(&krusty), pushes(&reference), "{member}: {krusty:#?}");
        assert_eq!(krusty, reference, "{member}: complete instructions");
        let stores = krusty
            .iter()
            .filter(|insn| opcode(insn).contains("store"))
            .count();
        assert_eq!(stores, 0, "{member}: no operand store remains");
    }
}

#[test]
fn an_inlined_call_without_operand_copies_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (larger(3, 7) != 7 || larger(9, 2) != 9) return \"larger\"\n\
             \x20   return if (smaller(5L, -1L) == -1L) \"OK\" else \"smaller\"\n\
             }}\n"
        ),
        "inline operand copies",
    );
}

#[test]
fn an_external_inline_vararg_keeps_nested_element_locals_declared() {
    const LIBRARY: &str = "@file:Suppress(\"INVISIBLE_MEMBER\", \"INVISIBLE_REFERENCE\")\n\
        package dependency\n\
        @kotlin.internal.InlineOnly\n\
        inline fun first(vararg values: String): String = values[0]\n";
    const CONSUMER: &str = "package consumer\n\
        import dependency.first\n\
        fun box(): String {\n\
        \x20   val value = \"OK\"\n\
        \x20   return first(value)\n\
        }\n";

    let Some(output) = common::expect_box_run_against_kotlinc(LIBRARY, CONSUMER) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    // `value` is nested below the Vararg node. Removing its operand temp without rewriting that
    // nested read leaves an uninitialized local slot, which the JVM verifier rejects before box().
    assert_eq!(output, "OK");
}
