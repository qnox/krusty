//! A `for`-loop variable may carry an explicit type — `for (i: Int in xs)`, including a widened
//! nullable type `for (c: Char? in str)`. The variable's value is the iterable's (non-null) element;
//! the annotation only widens, so it is accepted and discarded. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn typed_int_loop_var_over_array() {
    const SRC: &str = "fun box(): String {\n\
    var s = 0\n\
    for (i: Int in intArrayOf(1, 2, 3)) { s += i }\n\
    return if (s == 6) \"OK\" else \"fail $s\"\n\
}\n";
    assert_eq!(run(SRC).expect("typed loop var"), "OK");
}

#[test]
fn typed_loop_var_over_range() {
    const SRC: &str = "fun box(): String {\n\
    var s = 0\n\
    for (i: Int in 1..3) { s += i }\n\
    return if (s == 6) \"OK\" else \"fail $s\"\n\
}\n";
    assert_eq!(run(SRC).expect("typed loop var over range"), "OK");
}

#[test]
fn nullable_typed_loop_var_downto() {
    const SRC: &str = "fun box(): String {\n\
    var s = 0\n\
    for (i: Int? in 4 downTo 1) { s += i!! }\n\
    return if (s == 10) \"OK\" else \"fail $s\"\n\
}\n";
    assert_eq!(run(SRC).expect("nullable typed loop var"), "OK");
}

#[test]
fn source_interface_inheriting_list_is_iterable() {
    let declarations = "\
package sample

class Token
interface TokenList : List<Token>
";
    let usage = "\
package sample

fun visit(tokens: TokenList) {
    for (token in tokens) {
        token.toString()
    }
}
";
    common::expect_front_end_ok_files_with_stdlib(
        &[declarations, usage],
        "SourceInterfaceInheritingListIsIterable",
    );
}

#[test]
fn source_class_inheriting_array_list_is_iterable() {
    const SRC: &str = "\
class TokenList : ArrayList<String>()

fun box(): String {
    val tokens = TokenList()
    tokens.add(\"O\")
    tokens.add(\"K\")
    var result = \"\"
    for (token in tokens) {
        result += token
    }
    return result
}
";
    common::expect_box_ok_with_stdlib(SRC, "SourceClassInheritingArrayListIsIterable");
}
