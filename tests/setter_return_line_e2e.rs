//! kotlinc completes a setter's `Unit` body with a return that carries a line, as it does for a
//! declared function: a block body falls off its closing `}`
//! (`setExtraLineNumberForVoidReturningFunction`), and an expression body's return sits at the
//! expression's end offset. A body on one line already has that line in effect.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      fun sink(x: Int) {}\n\
                      \n\
                      var stored = 0\n\
                      \n\
                      var blockSetter: Int\n\
                      \x20   get() = stored\n\
                      \x20   set(value) {\n\
                      \x20       stored = value\n\
                      \x20   }\n\
                      \n\
                      var exprSetter: Int\n\
                      \x20   get() = stored\n\
                      \x20   set(value) = sink(\n\
                      \x20       value\n\
                      \x20   )\n\
                      \n\
                      var emptySetter: Int\n\
                      \x20   get() = stored\n\
                      \x20   set(value) {}\n\
                      \n\
                      var oneLine: Int\n\
                      \x20   get() = stored\n\
                      \x20   set(value) = sink(value)\n\
                      \n\
                      class Holder {\n\
                      \x20   var field = 0\n\
                      \x20       set(value) {\n\
                      \x20           field = value + 1\n\
                      \x20       }\n\
                      \x20   var Int.ext: Int\n\
                      \x20       get() = this\n\
                      \x20       set(value) {\n\
                      \x20           sink(value)\n\
                      \x20       }\n\
                      \x20   fun touch() { 1.ext = 2 }\n\
                      }\n";

#[test]
fn setter_returns_are_byte_identical_to_kotlinc() {
    for class in ["store/SetterReturnLineKt", "store/Holder"] {
        common::byte_diff_against_kotlinc_cp(
            "SetterReturnLine",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
