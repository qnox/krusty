//! A backtick-quoted name is always an IDENTIFIER, never a keyword.
//!
//! The lexer already emits `` `continue` `` as an `Ident`, but the parser decides several
//! constructs by the token's TEXT, so an escaped hard keyword was re-read as the keyword and the
//! surrounding expression failed to parse:
//!
//! ```text
//! error: expected an expression
//! ```
//!
//! Generated API clients use exactly this spelling — Kubernetes' pagination parameter is named
//! `continue` — which made it the corpus's largest parser cluster.
use super::common;

/// Every case here is plain Kotlin plus the stdlib — the contract is purely syntactic.
fn run(main: &str) -> String {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::expect_box_run(main, "Main", &[stdlib], Some(jdk.as_path()))
}

#[test]
fn escaped_keyword_reads_as_a_value() {
    const MAIN: &str = "fun probe(`continue`: String): String = `continue`\n\
        fun box(): String = probe(\"OK\")\n";
    assert_eq!(run(MAIN), "OK");
}

#[test]
fn escaped_keyword_takes_a_safe_call() {
    // The failing corpus shape: `` `continue`?.let { … } `` inside a builder lambda.
    const MAIN: &str = "fun probe(`continue`: String?, `break`: String?): String {\n\
        \x20   val parts = buildList {\n\
        \x20       `continue`?.let { add(\"c=\" + it) }\n\
        \x20       `break`?.let { add(\"b=\" + it) }\n\
        \x20   }\n\
        \x20   return parts.joinToString(\",\")\n\
        }\n\
        fun box(): String = probe(\"1\", \"2\")\n";
    assert_eq!(run(MAIN), "c=1,b=2");
}

#[test]
fn escaped_keyword_is_assignable_and_callable() {
    const MAIN: &str = "fun box(): String {\n\
        \x20   var `continue` = \"a\"\n\
        \x20   `continue` = `continue` + \"b\"\n\
        \x20   val `object` = `continue`.uppercase()\n\
        \x20   return `object`\n\
        }\n";
    assert_eq!(run(MAIN), "AB");
}

#[test]
fn escaped_modifier_stays_a_callable_value() {
    const MAIN: &str = "fun box(): String {\n\
        \x20   val `suspend`: (() -> String) -> String = { block -> block() }\n\
        \x20   val `inline` = \"O\"\n\
        \x20   return `inline` + `suspend` { \"K\" }\n\
        }\n";
    assert_eq!(run(MAIN), "OK");
}

#[test]
fn a_real_keyword_still_parses_as_one() {
    // The escape must not disarm the keyword itself: an unescaped `continue` is still a jump, and
    // `break` still leaves the loop.
    const MAIN: &str = "fun box(): String {\n\
        \x20   var seen = \"\"\n\
        \x20   for (s in listOf(\"0\", \"1\", \"2\", \"3\")) {\n\
        \x20       if (s == \"1\") continue\n\
        \x20       if (s == \"3\") break\n\
        \x20       seen = seen + s\n\
        \x20   }\n\
        \x20   return seen\n\
        }\n";
    assert_eq!(run(MAIN), "02");
}

#[test]
fn escaped_keyword_starts_an_expression() {
    // These reach the dispatches that read a token's TEXT to choose a construct: `` `object` `` and
    // `` `try` `` would have started an object literal / try-expression, and `` `throw` `` compiled
    // CLEAN pre-fix but died at runtime with a VerifyError — a silent miscompile, not a parse error.
    const MAIN: &str = "fun `object`(): String = \"o\"\n\
        fun `try`(): String = \"t\"\n\
        fun `throw`(v: String): String = v\n\
        fun box(): String = `object`() + `try`() + `throw`(\"!\")\n";
    assert_eq!(run(MAIN), "ot!");
}

#[test]
fn escaped_keyword_names_a_declaration() {
    // Declaration and access positions: a property named `` `object` ``, and a type parameter named
    // `` `out` `` — the latter reaches the modifier-list dispatch, which would otherwise consume the
    // name as a variance modifier and drop the parameter.
    const MAIN: &str = "class Holder<`out`>(val `object`: `out`, val `typealias`: String)\n\
        fun box(): String {\n\
        \x20   val h = Holder(\"o\", \"t\")\n\
        \x20   return h.`object` + h.`typealias`\n\
        }\n";
    assert_eq!(run(MAIN), "ot");
}

#[test]
fn until_stays_an_infix_function_when_escaped() {
    // `until` is `kotlin.ranges.until`, an ordinary infix function — NOT a keyword. Escaping it
    // must keep it callable in every position a range is read, including `in` and `when`.
    const MAIN: &str = "fun box(): String {\n\
        \x20   var seen = \"\"\n\
        \x20   for (i in 0 `until` 3) seen = seen + i.toString()\n\
        \x20   val inside = 5 in 0 `until` 10\n\
        \x20   val matched = when (5) {\n\
        \x20       in 0 `until` 10 -> \"y\"\n\
        \x20       else -> \"n\"\n\
        \x20   }\n\
        \x20   return seen + inside.toString() + matched\n\
        }\n";
    assert_eq!(run(MAIN), "012truey");
}

#[test]
fn an_adjacent_closing_backtick_does_not_escape_the_next_name() {
    // In `` `a`is ``-shaped source the bare token is byte-preceded by the PREVIOUS name's closing
    // backtick; treating it as escaped would stop a real keyword from being read as one.
    const MAIN: &str = "fun box(): String {\n\
        \x20   val `a` = \"x\"\n\
        \x20   val hit = `a` is String\n\
        \x20   return if (hit) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(MAIN), "OK");
}
