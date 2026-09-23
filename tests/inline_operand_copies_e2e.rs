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
        // Both push the two parameters straight from their own slots, in source order.
        let pushes = |body: &[String]| {
            body.iter()
                .take(2)
                .map(|insn| opcode(insn).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(pushes(&krusty), pushes(&reference), "{member}: {krusty:#?}");
        // The expansion's parameter store is the only copy left; kotlinc reads these in place.
        let stores = krusty
            .iter()
            .filter(|insn| opcode(insn).contains("store"))
            .count();
        assert!(stores <= 1, "{member} copies an operand twice: {krusty:#?}");
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
