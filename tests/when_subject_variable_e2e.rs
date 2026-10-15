//! `when (val subject = value)` uses the declared local as its subject instead of copying that
//! local into a second compiler temporary.
use super::common;

const SOURCE: &str = "package store\n\
    class Token(val text: String)\n\
    fun select(value: Token, first: Token, second: Token): String =\n\
    \x20   when (val subject = value) {\n\
    \x20       first, second -> \"hit ${subject.text}\"\n\
    \x20       else -> \"miss ${subject.text}\"\n\
    \x20   }\n\
    fun classify(value: Any?): String = when (val subject = value) {\n\
    \x20   is String? -> if (subject == null) \"null\" else \"text $subject\"\n\
    \x20   else -> \"other\"\n\
    }\n\
    fun box(): String {\n\
    \x20   val first = Token(\"first\")\n\
    \x20   val second = Token(\"second\")\n\
    \x20   val hit = select(second, first, second)\n\
    \x20   val miss = select(Token(\"other\"), first, second)\n\
    \x20   val nullable = classify(null) + \" / \" + classify(\"value\") + \" / \" + classify(7)\n\
    \x20   return if (hit == \"hit second\" && miss == \"miss other\" &&\n\
    \x20       nullable == \"null / text value / other\") \"OK\" else \"$hit / $miss / $nullable\"\n\
    }\n";

#[test]
fn the_declared_local_is_the_bytecode_subject_and_owns_its_scope() {
    common::byte_diff_against_kotlinc("WhenSubjectVariable", SOURCE, "store/WhenSubjectVariableKt")
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn the_declared_subject_executes_without_a_copy() {
    let output = common::compile_and_run_with_stdlib(SOURCE, "WhenSubjectVariableRun");
    assert_eq!(output.as_deref(), Some("OK"));
}
