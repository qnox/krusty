//! A labelled return to a lambda inside an `init` block or a property initializer.
//!
//! Such a return leaves only the lambda, so kotlinc accepts it where a return from the declaration
//! itself is prohibited. krusty reported `'return' is prohibited here.` for both, because it checked
//! that the enclosing declaration allows a return before resolving which frame the return leaves.

use super::common;

const SOURCE: &str = "val log = StringBuilder()\n\
                      inline fun step(block: () -> Unit) = block()\n\
                      class Box(val id: Int) {\n\
                      \x20   init { step { if (id < 0) return@step; log.append(id) } }\n\
                      \x20   val sign: Int = run { if (id < 0) return@run -1; 1 }\n\
                      }\n\
                      fun box(): String {\n\
                      \x20   val b = Box(5)\n\
                      \x20   Box(-1)\n\
                      \x20   return if (log.toString() == \"5\" && b.sign == 1) \"OK\" else log.toString()\n\
                      }\n";

#[test]
fn a_labelled_return_leaves_a_lambda_in_a_class_initializer() {
    common::expect_box_ok_with_stdlib(SOURCE, "InitializerLambdaReturn");
}

#[test]
fn an_init_block_with_a_labelled_return_matches_kotlinc_bytes() {
    const INIT: &str = "val log = StringBuilder()\n\
                        inline fun step(block: () -> Unit) = block()\n\
                        class Box(val id: Int) {\n\
                        \x20   init { step { if (id < 0) return@step; log.append(id) } }\n\
                        }\n";
    match common::byte_diff_against_kotlinc_cp(
        "InitializerLambdaReturnBytes",
        INIT,
        "Box",
        &[common::stdlib_jar()],
    ) {
        None => eprintln!("skip (reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

/// A return from the declaration itself is still prohibited there, inside a lambda or not.
#[test]
fn an_unlabelled_return_in_a_class_initializer_is_still_prohibited() {
    const MAIN: &str = "inline fun step(block: () -> Unit) = block()\n\
                        class Box(val id: Int) {\n\
                        \x20   init { step { if (id < 0) return } }\n\
                        }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    common::expect_identical_rejection(&result, "an unlabelled return in an init block");
}
