//! Malformed `Char` literals — the lexer-level rejection that matches `kotlinc`.
//!
//! A Kotlin `Char` is exactly one UTF-16 code unit, so a literal must hold exactly one *element*:
//! either one BMP character or one valid escape, and a raw newline may not appear at all.
//! `kotlinc` (2.4.10) rejects everything else with four distinct messages, verified against the
//! reference compiler:
//!
//! | source                  | kotlinc                                       |
//! |-------------------------|-----------------------------------------------|
//! | `''`                    | `empty character literal.`                    |
//! | `'ab'` / `'a\n'`        | `too many characters in a character literal.` |
//! | astral character        | `too many characters in a character literal.` |
//! | `'\uD83D\uDE00'`        | `unsupported escape sequence.`                |
//! | `'\n\n'` / `'\q'`       | `unsupported escape sequence.`                |
//! | `'\u12'` / `'\0'`       | `unsupported escape sequence.`                |
//! | a raw CR or LF          | `incorrect character literal.`                |
//!
//! Note the asymmetry, which these tests pin: content that STARTS with a backslash must be exactly
//! one valid escape or it is an escape error, while content that starts with a plain character is a
//! "too many characters" error. A lone surrogate written as an escape (`'\uD83D'`) is ACCEPTED —
//! it is one code unit — so the check must not reason about well-formed UTF-16. A raw TAB is
//! accepted too: only CR and LF are excluded from a literal's body.
//!
//! Without these, `const val E = '😀'` compiled silently to `'\uF600'`: `unquote_char` truncates
//! a code POINT to its low 16 bits, so U+1F600 became U+F600 — not even a surrogate half.
//! A diagnostic-parity gap against `kotlinc`.

use super::common;

fn parse_diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

/// Wrap a char literal in a program that needs no library symbols, so the front end never touches
/// the classpath and the assertion can never be satisfied by an unrelated "unresolved" error.
fn diags_for(lit: &str) -> Vec<String> {
    parse_diags(&format!("fun box(): Int {{ val c = {lit}; return 0 }}"))
}

fn assert_reports(lit: &str, expected: &str) {
    let d = diags_for(lit);
    assert!(
        d.iter().any(|m| m.contains(expected)),
        "expected {expected:?} for the literal {lit}, got {d:?}"
    );
}

/// The companion of [`assert_reports`]: kotlinc picks ONE message per literal, so a test that only
/// checked for the expected message would still pass if krusty also emitted a second, wrong one.
fn assert_not_reported(lit: &str, unexpected: &str) {
    let d = diags_for(lit);
    assert!(
        !d.iter().any(|m| m.contains(unexpected)),
        "did not expect {unexpected:?} for the literal {lit}, got {d:?}"
    );
}

fn assert_accepted(lit: &str) {
    let d = diags_for(lit);
    assert!(
        d.is_empty(),
        "expected the literal {lit} to be accepted, got {d:?}"
    );
}

// ---------------------------------------------------------------------------
// Rejected: not exactly one UTF-16 code unit
// ---------------------------------------------------------------------------

#[test]
fn astral_character_literal_is_rejected() {
    // U+1F600 is two UTF-16 code units; truncating its CODE POINT to 16 bits silently
    // produced `'\uF600'`.
    assert_reports("'\u{1F600}'", "too many characters in a character literal");
}

#[test]
fn multi_character_literal_is_rejected() {
    assert_reports("'ab'", "too many characters in a character literal");
}

#[test]
fn character_followed_by_escape_is_rejected() {
    // Starts with a plain character, so kotlinc counts elements rather than blaming the escape.
    assert_reports("'a\\n'", "too many characters in a character literal");
}

#[test]
fn empty_character_literal_is_rejected() {
    assert_reports("''", "empty character literal");
}

#[test]
fn surrogate_pair_written_as_escapes_is_rejected() {
    // The two-escape spelling of U+1F600: still two code units, but kotlinc blames the escape.
    assert_reports("'\\uD83D\\uDE00'", "unsupported escape sequence");
}

#[test]
fn two_escapes_are_rejected() {
    assert_reports("'\\n\\n'", "unsupported escape sequence");
}

#[test]
fn two_unicode_escapes_are_rejected() {
    assert_reports("'\\u0041\\u0042'", "unsupported escape sequence");
}

#[test]
fn unknown_escape_is_rejected() {
    // `\q` silently decoded to `'q'`.
    assert_reports("'\\q'", "unsupported escape sequence");
}

#[test]
fn truncated_unicode_escape_is_rejected() {
    // Fewer than four hex digits; `unquote_char` silently yielded `'\u{0000}'`. Spell the
    // sentinel as source text: embedding its NUL byte here makes this Rust file binary to Git
    // and hides otherwise reviewable test diffs from ordinary text tooling.
    assert_reports("'\\u12'", "unsupported escape sequence");
}

#[test]
fn nul_escape_is_rejected() {
    // Kotlin has no `\0` escape (C does): kotlinc rejects `'\0'`, even though `unquote_char`
    // decodes it.
    assert_reports("'\\0'", "unsupported escape sequence");
}

#[test]
fn raw_newline_in_literal_is_rejected() {
    // Kotlin's grammar excludes CR and LF from a literal's body outright, independently of the
    // code-unit count — a bare LF IS one code unit, so counting alone would let it through.
    assert_reports("'\n'", "incorrect character literal");
}

#[test]
fn raw_carriage_return_in_literal_is_rejected() {
    assert_reports("'\r'", "incorrect character literal");
}

// ---------------------------------------------------------------------------
// Each failure picks exactly ONE message, the way kotlinc does
// ---------------------------------------------------------------------------

#[test]
fn a_counting_failure_does_not_also_blame_the_escape() {
    // `'a\\n'` holds an escape, but the content starts with a plain character, so kotlinc counts.
    assert_not_reported("'a\\n'", "unsupported escape sequence");
}

#[test]
fn an_escape_failure_does_not_also_count_characters() {
    assert_not_reported("'\\q'", "too many characters in a character literal");
    assert_not_reported(
        "'\\uD83D\\uDE00'",
        "too many characters in a character literal",
    );
}

// ---------------------------------------------------------------------------
// The same check inside a string template
// ---------------------------------------------------------------------------

#[test]
fn char_literal_inside_a_string_template_is_checked() {
    // `char_lit` is also reached through `string_template_inner`, the path behind the `${'$'}`
    // idiom — a malformed literal must not slip through just because it sits in a template.
    let d = parse_diags("fun box(): Int { val s = \"a${'ab'}b\"; return 0 }");
    assert!(
        d.iter()
            .any(|m| m.contains("too many characters in a character literal")),
        "expected the template's char literal to be checked, got {d:?}"
    );
}

#[test]
fn the_dollar_template_idiom_still_compiles() {
    let d = parse_diags("fun box(): Int { val s = \"a${'$'}b\"; return 0 }");
    assert!(
        d.is_empty(),
        "expected `${{'$'}}` to be accepted, got {d:?}"
    );
}

// ---------------------------------------------------------------------------
// Accepted: exactly one UTF-16 code unit (kotlinc compiles all of these)
// ---------------------------------------------------------------------------

#[test]
fn plain_character_is_accepted() {
    assert_accepted("'a'");
}

#[test]
fn simple_escapes_are_accepted() {
    // Kotlin's whole escape set for a `Char`, verified against kotlinc — note the absence of `\0`.
    for lit in [
        "'\\n'", "'\\t'", "'\\r'", "'\\b'", "'\\\\'", "'\\''", "'\\\"'", "'\\$'",
    ] {
        assert_accepted(lit);
    }
}

#[test]
fn unicode_escape_is_accepted() {
    assert_accepted("'\\u0041'");
}

#[test]
fn lone_surrogate_escape_is_accepted() {
    // One code unit, even though it is not a valid scalar value — kotlinc accepts both halves.
    assert_accepted("'\\uD83D'");
    assert_accepted("'\\uDE00'");
}

#[test]
fn raw_tab_space_and_dollar_are_accepted() {
    // The contents nearest the boundary: TAB is not excluded the way CR/LF are, and a raw `$` is
    // legal even though `$` is one of the escapable characters.
    assert_accepted("'\t'");
    assert_accepted("' '");
    assert_accepted("'$'");
}

#[test]
fn non_ascii_bmp_character_is_accepted() {
    assert_accepted("'\u{00E9}'");
    assert_accepted("'\u{4E2D}'");
}
