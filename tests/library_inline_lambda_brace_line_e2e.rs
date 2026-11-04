//! A multi-line `Unit` lambda passed to a dependency's inline iteration (`forEach`) falls off its
//! closing brace. kotlinc marks that `}` on a `nop` while the lambda's locals are still open, then
//! closes the inlined frame on the dependency's invocation line with its own `nop`. A one-line
//! lambda's brace shares its body's line, so the cleanup drops that first `nop`.

use super::common;

const SOURCE: &str = "class Sink {\n\
    \x20   fun add(s: String) {}\n\
    }\n\
    \n\
    fun Sink.statement(items: List<String>) {\n\
    \x20   items.forEach {\n\
    \x20       add(it)\n\
    \x20   }\n\
    }\n\
    \n\
    fun Sink.returned(items: List<String>) {\n\
    \x20   return items.forEach {\n\
    \x20       add(it)\n\
    \x20       add(it.trim())\n\
    \x20   }\n\
    }\n\
    \n\
    fun Sink.sequence(text: String) {\n\
    \x20   text.lineSequence().forEach { line ->\n\
    \x20       add(line)\n\
    \x20   }\n\
    \x20   add(text)\n\
    }\n\
    \n\
    fun Sink.oneLine(items: List<String>) {\n\
    \x20   items.forEach { add(it) }\n\
    }\n";

#[test]
fn a_multi_line_lambda_brace_in_a_library_inline_call_is_marked_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "LibraryInlineLambdaBrace",
        SOURCE,
        "LibraryInlineLambdaBraceKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("the library inline lambda braces differ: {diff}"));
}
