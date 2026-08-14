//! Infix operands and `when` subject initializers may begin on the next line.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn infix_to_with_the_operand_on_the_next_line() {
    const SRC: &str = "fun box(): String {\n\
    \x20   val m = mapOf(\n\
    \x20       \"a\" to\n\
    \x20           \"1\",\n\
    \x20   )\n\
    \x20   return m[\"a\"] ?: \"missing\"\n\
    }\n";
    assert_eq!(
        run(SRC).expect("infix `to` continues onto the next line"),
        "1"
    );
}

#[test]
fn infix_step_with_the_operand_on_the_next_line() {
    const SRC: &str = "fun box(): String {\n\
    \x20   var total = 0\n\
    \x20   for (i in 0..10 step\n\
    \x20       5) {\n\
    \x20       total += i\n\
    \x20   }\n\
    \x20   return total.toString()\n\
    }\n";
    assert_eq!(
        run(SRC).expect("infix `step` continues onto the next line"),
        "15"
    );
}

#[test]
fn when_subject_declaration_with_the_initializer_on_the_next_line() {
    const SRC: &str = "fun f(n: Int): Int = n + 1\n\
    fun pick(n: Int): String =\n\
    \x20   when (\n\
    \x20       val r =\n\
    \x20           f(n)\n\
    \x20   ) {\n\
    \x20       1 -> \"one\"\n\
    \x20       else -> r.toString()\n\
    \x20   }\n\
    fun box(): String = pick(0) + \"/\" + pick(5)\n";
    assert_eq!(
        run(SRC).expect("when-subject initializer continues onto the next line"),
        "one/6"
    );
}

#[test]
fn a_newline_before_the_operator_still_ends_the_statement() {
    const SRC: &str = "fun box(): String {\n\
    \x20   val a = \"x\"\n\
    \x20   a\n\
    \x20   to\n\
    \x20   \"y\"\n\
    \x20   return a\n\
    }\n";
    let Some(diags) = common::checker_diags_with_stdlib(SRC) else {
        return;
    };
    assert!(
        diags
            .iter()
            .any(|diagnostic| diagnostic.contains("unresolved reference 'to'")),
        "expected an unresolved line-leading `to`; got {diags:#?}"
    );
    assert!(
        !diags
            .iter()
            .any(|diagnostic| diagnostic.contains("expected")),
        "the line-leading operator must not create a parser diagnostic; got {diags:#?}"
    );
}

#[test]
fn chained_and_user_defined_infix_wrap_at_every_operator() {
    const SRC: &str = "infix fun String.glue(other: String): String = this + other\n\
    fun box(): String =\n\
    \x20   \"a\" glue\n\
    \x20       \"b\" glue\n\
    \x20       \"c\"\n";
    assert_eq!(run(SRC).expect("chained user-defined infix wraps"), "abc");
}

#[test]
fn an_operand_beginning_with_a_lambda_still_continues() {
    const SRC: &str = "infix fun Int.applying(f: (Int) -> Int): Int = f(this)\n\
    fun box(): String =\n\
    \x20   (2 applying\n\
    \x20       { it * 3 }).toString()\n";
    assert_eq!(
        run(SRC).expect("lambda operand continues the infix call"),
        "6"
    );
}

#[test]
fn infix_extension_is_selected_before_a_non_infix_primitive_member() {
    const SRC: &str = "infix fun Int.rem(other: Int): Int = 40 + other\n\
    fun box(): String {\n\
    \x20   val infix = 8 rem 2\n\
    \x20   val member = 8.rem(3)\n\
    \x20   return if (infix == 42 && member == 2) \"OK\" else \"$infix/$member\"\n\
    }\n";
    assert_eq!(run(SRC).expect("infix extension selection"), "OK");
}

#[test]
fn infix_syntax_rejects_a_plain_extension() {
    const SRC: &str = "fun Int.rem(other: Int): Int = other\n\
    fun box(): String = (8 rem 2).toString()\n";
    let diagnostics = common::front_end_diagnostics_with_stdlib(SRC);
    assert!(
        !diagnostics.is_empty(),
        "plain extension must not answer infix syntax"
    );
}

#[test]
fn invalid_infix_declarations_are_rejected() {
    const SRC: &str = "infix fun top(value: Int) = value\n\
    infix fun Int.many(a: Int, b: Int) = a + b\n\
    class C {\n\
    \x20   infix fun defaulted(value: Int = 0) = value\n\
    \x20   infix fun spread(vararg value: Int) = value.size\n\
    }\n";
    let diagnostics = common::front_end_diagnostics_with_stdlib(SRC);
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.contains("'infix' modifier is inapplicable"))
            .count(),
        4,
        "{diagnostics:#?}"
    );
}
