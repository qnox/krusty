//! kotlinc's temporary-variable elimination, applied to krusty's finished methods.
//!
//! kotlinc writes a temporary as a local and then folds the ones whose value can stay on the
//! operand stack (`TemporaryVariablesEliminationTransformer`). krusty emitted the locals and never
//! folded them, so `println(f())` kept `astore; …; getstatic System.out; aload` where kotlinc has
//! `getstatic System.out; swap`. The class writer now runs the same rules over each method once its
//! tables are final.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

const SOURCE: &str = "fun label(n: Int): String = \"n=\" + n\n\
    fun printed(n: Int) {\n\
    \x20   println(label(n))\n\
    }\n\
    fun counted(n: Int) {\n\
    \x20   println(n + 1)\n\
    }\n";

#[test]
fn a_printed_value_stays_on_the_stack_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "TemporaryElimination",
        SOURCE,
        "TemporaryEliminationKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // `counted` folds too, but krusty spills an inline argument twice where kotlinc spills it once;
    // the folded copy's slot stays taken (kotlinc does not renumber), so its locals differ.
    let member = "void printed(int)";
    let reference = method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "{member} not found");
    assert_eq!(
        method_instructions(&built.krusty, member),
        reference,
        "{member}"
    );
}

#[test]
fn folded_temporaries_still_run() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   printed(3)\n\
             \x20   counted(4)\n\
             \x20   return if (label(5) == \"n=5\") \"OK\" else \"FAIL\"\n\
             }}\n"
        ),
        "folded temporaries",
    );
}
