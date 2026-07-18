//! Front-end PARSE/LEX error-path coverage.
//!
//! Each test feeds a DISTINCT malformed Kotlin snippet through `lex → parse` (via
//! `common::front_end_diagnostics` with no classpath) and asserts the compiler produced at least
//! one diagnostic — i.e. it rejected the snippet at the lexer or parser level. These exercise the
//! many error branches in `src/lexer.rs` and `src/parser.rs` that the box corpus (only *valid*
//! programs) never reaches.
//!
//! Every snippet needs no library symbols, so it never touches the classpath and never skips: a
//! genuine lex/parse diagnostic is required for each assertion to hold.

use super::common;

/// Run the front end with NO classpath — these are lexer/parser-level errors that need no library
/// symbols, so `collect`/`check` never run (they are gated on `!diags.has_errors()`).
fn parse_diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_rejected(d: &[String], what: &str) {
    assert!(
        d.iter().any(|m| !m.is_empty()),
        "expected a diagnostic for {what}, got none: {d:?}"
    );
}

// ---------------------------------------------------------------------------
// Lexer: unterminated literals & stray characters
// ---------------------------------------------------------------------------

#[test]
fn unterminated_string_literal() {
    // string opened but never closed before EOF
    let d = parse_diags("fun box(): Int { val s = \"abc; return 0 }");
    assert_rejected(&d, "unterminated string literal");
}

#[test]
fn unterminated_char_literal() {
    // char opened but never closed
    let d = parse_diags("fun box(): Int { val c = 'a; return 0 }");
    assert_rejected(&d, "unterminated character literal");
}

#[test]
fn unterminated_triple_quoted_string() {
    // raw string opened with `"""` and never closed
    let d = parse_diags("fun box(): Int { val s = \"\"\"raw text; return 0 }");
    assert_rejected(&d, "unterminated triple-quoted string");
}

#[test]
fn unterminated_backtick_identifier() {
    // backtick identifier never closed before newline/EOF
    let d = parse_diags("fun `weird box(): Int { return 0 }");
    assert_rejected(&d, "unterminated backtick identifier");
}

#[test]
fn unterminated_template_string() {
    // interpolated string opened, contains a `$x`, never closed
    let d = parse_diags("fun box(): Int { val x = 1; val s = \"hi $x ; return 0 }");
    assert_rejected(&d, "unterminated template string");
}

#[test]
fn stray_hash_character() {
    // `#` is not a valid Kotlin token
    let d = parse_diags("fun box(): Int { # ; return 0 }");
    assert_rejected(&d, "stray '#' character");
}

#[test]
fn stray_backslash_character() {
    // a lone backslash outside a string/char is not a valid token
    let d = parse_diags("fun box(): Int { val x = \\ ; return 0 }");
    assert_rejected(&d, "stray backslash character");
}

// ---------------------------------------------------------------------------
// Declarations: missing names / structure
// ---------------------------------------------------------------------------

#[test]
fn fun_without_name() {
    // `fun` with an immediate parameter list, no name
    let d = parse_diags("fun (): Int { return 0 }");
    assert_rejected(&d, "function with no name");
}

#[test]
fn class_without_name() {
    // `class` followed immediately by a body
    let d = parse_diags("class { }\nfun box(): Int = 0");
    assert_rejected(&d, "class with no name");
}

#[test]
fn extension_fun_without_name() {
    // `fun Int.` then `(` — no extension function name after the dot
    let d = parse_diags("fun Int.(): Int = 0");
    assert_rejected(&d, "extension function with no name");
}

#[test]
fn param_without_name() {
    // parameter list starts with a `:` — no parameter name
    let d = parse_diags("fun f(: Int): Int = 0");
    assert_rejected(&d, "parameter with no name");
}

#[test]
fn val_without_name() {
    // `val =` — no identifier for the binding
    let d = parse_diags("fun box(): Int { val = 5; return 0 }");
    assert_rejected(&d, "val with no name");
}

#[test]
fn val_without_initializer_at_eof() {
    // `val x =` then end of block — no initializer expression
    let d = parse_diags("fun box(): Int { val x = }");
    assert_rejected(&d, "val with no initializer expression");
}

#[test]
fn property_getter_missing_body() {
    // property getter with neither `=` nor `{`
    let d = parse_diags("class C { val x: Int get }\nfun box(): Int = 0");
    assert_rejected(&d, "getter without '=' or '{'");
}

#[test]
fn constructor_delegation_not_this_or_super() {
    // secondary constructor delegates to something other than `this`/`super`
    let d = parse_diags("class C { constructor() : foo() {} }\nfun box(): Int = 0");
    assert_rejected(&d, "constructor delegation not this/super");
}

// ---------------------------------------------------------------------------
// Types: malformed generics / function types
// ---------------------------------------------------------------------------

#[test]
fn missing_type_after_colon() {
    // `val x:` then `=` — no type between the colon and the initializer
    let d = parse_diags("fun box(): Int { val x: = 5; return 0 }");
    assert_rejected(&d, "missing type after colon");
}

#[test]
fn unclosed_generic_type_argument() {
    // `List<Int` never closed with `>`
    let d = parse_diags("fun box(): Int { val x: List<Int = 5; return 0 }");
    assert_rejected(&d, "unclosed generic type argument");
}

#[test]
fn generic_type_missing_second_argument() {
    // `Map<Int,` then `=` — no second type argument
    let d = parse_diags("fun box(): Int { val x: Map<Int, = 5; return 0 }");
    assert_rejected(&d, "generic type missing second argument");
}

#[test]
fn function_type_missing_result() {
    // `(Int) ->` with no result type after the arrow
    let d = parse_diags("fun box(): Int { val f: (Int) -> = 5; return 0 }");
    assert_rejected(&d, "function type missing result type");
}

#[test]
fn type_position_empty() {
    // return type is missing entirely (`:` then `{`)
    let d = parse_diags("fun box(): { return 0 }");
    assert_rejected(&d, "empty type position");
}

// ---------------------------------------------------------------------------
// Expressions: dangling operators / malformed constructs
// ---------------------------------------------------------------------------

#[test]
fn dangling_binary_operator() {
    // `1 +` with no right operand
    let d = parse_diags("fun box(): Int { return 1 + }");
    assert_rejected(&d, "dangling binary operator");
}

#[test]
fn dangling_operator_at_eof() {
    // expression-body function ending on an operator
    let d = parse_diags("fun box(): Int = 1 *");
    assert_rejected(&d, "dangling operator at EOF");
}

#[test]
fn if_without_condition_parens() {
    // `if` with no `(` condition
    let d = parse_diags("fun box(): Int { if { return 1 }; return 0 }");
    assert_rejected(&d, "if without condition parens");
}

#[test]
fn if_empty_condition() {
    // `if ()` — no condition expression
    let d = parse_diags("fun box(): Int { if () { return 1 }; return 0 }");
    assert_rejected(&d, "if with empty condition");
}

#[test]
fn for_without_in() {
    // `for (x 0..3)` — missing `in`
    let d = parse_diags("fun box(): Int { for (x 0..3) {}; return 0 }");
    assert_rejected(&d, "for without in");
}

#[test]
fn while_without_condition_parens() {
    // `while` with no `(`
    let d = parse_diags("fun box(): Int { while { }; return 0 }");
    assert_rejected(&d, "while without condition parens");
}

#[test]
fn empty_template_interpolation() {
    // `${}` — empty interpolation expression
    let d = parse_diags("fun box(): Int { val s = \"${}\"; return 0 }");
    assert_rejected(&d, "empty template interpolation");
}

#[test]
fn try_without_catch_or_finally() {
    // `try { }` with no catch/finally
    let d = parse_diags("fun box(): Int { try { return 1 }; return 0 }");
    assert_rejected(&d, "try without catch or finally");
}

#[test]
fn when_branch_missing_arrow() {
    // `when` branch with no `->`
    let d = parse_diags("fun box(): Int { val x = 1; return when (x) { 1 10 else -> 0 } }");
    assert_rejected(&d, "when branch missing arrow");
}

// ---------------------------------------------------------------------------
// Delimiter balance / stray tokens
// ---------------------------------------------------------------------------

#[test]
fn unmatched_open_brace() {
    // function body brace never closed
    let d = parse_diags("fun box(): Int { return 0 ");
    assert_rejected(&d, "unmatched open brace");
}

#[test]
fn unmatched_open_paren() {
    // parenthesised expression never closed
    let d = parse_diags("fun box(): Int { return (1 + 2 }");
    assert_rejected(&d, "unmatched open paren");
}

#[test]
fn unmatched_open_bracket() {
    // index expression bracket never closed
    let d = parse_diags("fun box(): Int { val a = intArrayOf(1); return a[0 }");
    assert_rejected(&d, "unmatched open bracket");
}

#[test]
fn stray_closing_paren_top_level() {
    // stray `)` where a top-level declaration is expected
    let d = parse_diags(")\nfun box(): Int = 0");
    assert_rejected(&d, "stray closing paren at top level");
}

#[test]
fn stray_closing_brace_top_level() {
    // stray `}` after a complete function
    let d = parse_diags("fun box(): Int { return 0 } }");
    assert_rejected(&d, "stray closing brace at top level");
}

#[test]
fn missing_paren_after_fun_name() {
    // `fun box` then `:` — no parameter list
    let d = parse_diags("fun box: Int { return 0 }");
    assert_rejected(&d, "missing paren after fun name");
}

// ---------------------------------------------------------------------------
// Annotations / lambdas / destructuring
// ---------------------------------------------------------------------------

#[test]
fn annotation_arguments_unclosed() {
    // `@Ann(` never closed
    let d = parse_diags("@Ann( fun box(): Int = 0");
    assert_rejected(&d, "annotation arguments unclosed");
}

#[test]
fn lambda_arrow_without_body_close() {
    // lambda `{ x ->` with no closing brace
    let d = parse_diags("fun box(): Int { val f = { x: Int -> x ; return 0 }");
    assert_rejected(&d, "lambda not closed");
}

#[test]
fn destructuring_missing_close_paren() {
    // `val (a` never closed
    let d = parse_diags("fun box(): Int { val (a = pair; return 0 }");
    assert_rejected(&d, "destructuring not closed");
}

#[test]
fn catch_missing_paren() {
    // `catch` with no `(` binding
    let d = parse_diags("fun box(): Int { try { return 1 } catch { return 2 } }");
    assert_rejected(&d, "catch without binding parens");
}

#[test]
fn interface_property_with_initializer() {
    // interface properties cannot carry an initializer
    let d = parse_diags("interface I { val x: Int = 5 }\nfun box(): Int = 0");
    assert_rejected(&d, "interface property with initializer");
}

#[test]
fn nested_unclosed_paren_in_call() {
    // call argument list never closed
    let d = parse_diags("fun box(): Int { return maxOf(1, 2 ; }");
    assert_rejected(&d, "call argument list unclosed");
}
