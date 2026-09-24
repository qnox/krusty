//! kotlinc's stack peephole pass, applied to krusty's finished methods.
//!
//! kotlinc turns a value pushed only to be popped into `nop`s and drops them
//! (`StackPeepholeOptimizationsTransformer`), so a `Unit` result that nothing reads never reaches
//! its bytecode. krusty left `getstatic kotlin/Unit.INSTANCE; pop` after a `Unit` call used as the
//! value of an `if` arm, after an inlined lambda's body, and after an inlined `forEach`/`repeat`.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};
use super::temporary_elimination_e2e::stack_map;

const SOURCE: &str = "class Node { var hits = 0\n\
    \x20   fun touch() { hits++ }\n\
    }\n\
    fun guarded(n: Node, b: Boolean) { if (b) n.touch() }\n\
    fun each(ns: List<Node>) { ns.forEach { it.touch() } }\n\
    fun twice(n: Node) { repeat(2) { n.touch() } }\n\
    fun maybe(n: Node?) { n?.let { it.touch() } }\n";

const MEMBERS: [&str; 4] = ["void guarded(", "void each(", "void twice(", "void maybe("];

#[test]
fn a_unit_nothing_reads_is_never_materialized_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "StackPeephole",
        SOURCE,
        "StackPeepholeKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in MEMBERS {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        let krusty = method_instructions(&built.krusty, member);
        assert!(!krusty.is_empty(), "{member} not emitted");
        for (compiler, body) in [("kotlinc", &reference), ("krusty", &krusty)] {
            assert!(
                !body
                    .iter()
                    .any(|insn| insn.contains("kotlin/Unit.INSTANCE")),
                "{compiler} materializes an unread Unit in {member}: {body:#?}"
            );
        }
    }
    // Nothing else separates the two here: the `if` arm is kotlinc's instruction for instruction.
    let member = "void guarded(";
    assert_eq!(
        method_instructions(&built.krusty, member),
        method_instructions(&built.reference, member),
        "{member}"
    );
    assert_eq!(
        stack_map(&built.krusty, member),
        stack_map(&built.reference, member),
        "{member} frames"
    );
}

#[test]
fn code_without_its_unread_units_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   val n = Node()\n\
             \x20   guarded(n, false)\n\
             \x20   guarded(n, true)\n\
             \x20   each(listOf(n, n))\n\
             \x20   twice(n)\n\
             \x20   maybe(null)\n\
             \x20   maybe(n)\n\
             \x20   return if (n.hits == 6) \"OK\" else \"hits ${{n.hits}}\"\n\
             }}\n"
        ),
        "stack peephole",
    );
}
