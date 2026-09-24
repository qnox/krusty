//! A `when`/`if` whose value is discarded is a statement.
//!
//! krusty joined the branches of `if (b) { …; if (c) f() } else { g() }` at a `kotlin/Unit` value
//! even as the last statement of a `Unit` function: the `else` branch pushed `Unit.INSTANCE`, the
//! `then` branch — ending in an `if` without `else` — pushed nothing, and the join after them failed
//! verification ("Inconsistent stackmap frames"). kotlinc emits a discarded `when` as a statement,
//! each branch discarding its own value, and never materializes a discarded `Unit`.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "fun choose(b: Boolean, c: Boolean) {\n\
    \x20   if (b) {\n\
    \x20       println(\"b\")\n\
    \x20       if (c) {\n\
    \x20           println(\"c\")\n\
    \x20       }\n\
    \x20   } else {\n\
    \x20       println(\"x\")\n\
    \x20   }\n\
    }\n\
    fun nested(a: String?, b: Boolean) {\n\
    \x20   if (b) {\n\
    \x20       println(\"b\")\n\
    \x20       if (a != null) {\n\
    \x20           println(a)\n\
    \x20       }\n\
    \x20   } else {\n\
    \x20       println(\"x\")\n\
    \x20   }\n\
    }\n";

#[test]
fn a_discarded_if_with_an_if_statement_branch_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   choose(true, true)\n\
             \x20   choose(true, false)\n\
             \x20   choose(false, true)\n\
             \x20   nested(\"a\", true)\n\
             \x20   nested(null, true)\n\
             \x20   nested(null, false)\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "discarded if with an if-statement branch",
    );
}

#[test]
fn a_discarded_if_materializes_no_unit_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "DiscardedWhen",
        SOURCE,
        "DiscardedWhenKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in ["void choose(", "void nested("] {
        for (compiler, listing) in [("kotlinc", &built.reference), ("krusty", &built.krusty)] {
            let instructions = method_instructions(listing, member);
            assert!(!instructions.is_empty(), "{compiler}: {member} not found");
            assert!(
                !instructions
                    .iter()
                    .any(|instruction| instruction.contains("kotlin/Unit.INSTANCE")),
                "{compiler}: {member} materializes Unit: {instructions:#?}"
            );
        }
    }
}
