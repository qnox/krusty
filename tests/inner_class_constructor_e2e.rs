//! An inner class's constructor takes its outer instance first. kotlinc names that parameter
//! `this$0` in the constructor's LocalVariableTable, like the field it initializes.
use super::common;

#[test]
fn an_inner_constructor_lists_its_outer_instance() {
    let source = "class Holder {\n\
                  \x20   fun area(): Int = 1\n\
                  \x20   inner class Corner(val side: Int) {\n\
                  \x20       fun outer(): Int = area() + side\n\
                  \x20   }\n\
                  }\n";
    let compared =
        common::compile_with_kotlinc("Holder", source, &[], &["Holder", "Holder$Corner"]);
    for (expected, actual) in &compared {
        assert!(actual == expected, "Holder differs from kotlinc");
    }
}

/// The constructed inner instance reads its outer instance's state.
#[test]
fn an_inner_instance_reads_its_outer_state() {
    const SRC: &str = "class Holder(val base: Int) {\n\
        \x20   inner class Corner(val side: Int) {\n\
        \x20       fun total(): Int = base + side\n\
        \x20   }\n\
        }\n\
        fun box(): String = if (Holder(40).Corner(2).total() == 42) \"OK\" else \"Fail\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("inner instance"),
        "OK"
    );
}

/// A local class's leading captured value is not an outer instance: its constructor lists it by
/// the captured name, never as `this$0`.
#[test]
fn a_capturing_local_constructor_lists_no_outer_instance() {
    let source = "fun make(offset: Int): Any {\n\
                  \x20   class Piece(val size: Int) {\n\
                  \x20       fun total(): Int = size + offset\n\
                  \x20   }\n\
                  \x20   return Piece(1)\n\
                  }\n";
    let compared = common::compile_with_kotlinc("Make", source, &[], &["MakeKt$make$Piece"]);
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "MakeKt$make$Piece differs from kotlinc");
}
