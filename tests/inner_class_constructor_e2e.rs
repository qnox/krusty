//! An inner class's constructor takes its outer instance first. kotlinc names that parameter
//! `this$0` in the constructor's LocalVariableTable, like the field it initializes.
use super::common;

#[test]
fn an_inner_constructor_lists_its_outer_instance() {
    let source = "open class Root\n\
                  class Token : Root()\n\
                  class Holder {\n\
                  \x20   fun item(): Root = Token()\n\
                  \x20   inner class Corner(val side: Root) {\n\
                  \x20       fun outer(): Root = item()\n\
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
    const SRC: &str = "class Token(val text: String)\n\
        class Holder(val base: Token) {\n\
        \x20   inner class Corner(val side: Token) {\n\
        \x20       fun total(): Token = base\n\
        \x20   }\n\
        }\n\
        fun box(): String = Holder(Token(\"OK\")).Corner(Token(\"unused\")).total().text\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("inner instance"),
        "OK"
    );
}

/// A local class's leading captured value is not an outer instance: its constructor lists it by
/// the captured name, never as `this$0`.
#[test]
fn a_capturing_local_constructor_lists_no_outer_instance() {
    let source = "open class Root\n\
                  class Token : Root()\n\
                  fun make(offset: Root): Root {\n\
                  \x20   class Piece(val item: Root) : Root() {\n\
                  \x20       fun captured(): Root = offset\n\
                  \x20   }\n\
                  \x20   return Piece(Token())\n\
                  }\n";
    let compared = common::compile_with_kotlinc("Make", source, &[], &["MakeKt$make$Piece"]);
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "MakeKt$make$Piece differs from kotlinc");
}
