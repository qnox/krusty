//! More of `kotlin.text`, answered by the runtime.
//!
//! Text is UTF-8 here and Kotlin counts UTF-16 units, so each of these is the runtime's rather than
//! something the generator can open-code: a program that asked the same question in Kotlin source
//! would walk the encoding to answer it.
//!
//! Two of them are asked of a NULLABLE receiver. `isNullOrBlank`/`isNullOrEmpty` are the only text
//! members Kotlin declares that way, and the null is the point — it reaches the runtime instead of
//! being checked away at the call site.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// `single()` has two different complaints: none, and more than one.
#[test]
fn text_answers_its_single_char() {
    let source = r#"
fun box(): String {
    if ("O".single() != 'O') return "fail one"
    val empty = try { "".single(); "none" } catch (e: NoSuchElementException) { "raised" }
    if (empty != "raised") return "fail empty " + empty
    val many = try { "OK".single(); "none" } catch (e: IllegalArgumentException) { "raised" }
    if (many != "raised") return "fail many " + many
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_text_single", source);
}

/// `c in s` asks about one UNIT, which is not the question `"a" in s` asks.
#[test]
fn a_char_is_looked_for_inside_text() {
    let source = r#"
fun box(): String {
    val s = "123"
    if ('0' in s) return "fail absent"
    if ('1' !in s) return "fail present"
    if ('3' !in s) return "fail last"
    if ('1' in "") return "fail empty"
    if ("12" !in s) return "fail text still works"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_text_contains_char", source);
}

/// The two questions Kotlin declares on a NULLABLE receiver.
#[test]
fn text_answers_whether_it_is_null_or_blank() {
    let source = r#"
fun box(): String {
    val absent: String? = null
    if (!absent.isNullOrBlank()) return "fail null blank"
    if (!absent.isNullOrEmpty()) return "fail null empty"
    if (!"".isNullOrBlank()) return "fail empty blank"
    if (!"".isNullOrEmpty()) return "fail empty empty"
    if (!"  ".isNullOrBlank()) return "fail spaces blank"
    if ("  ".isNullOrEmpty()) return "fail spaces empty"
    if ("x".isNullOrBlank()) return "fail text blank"
    if ("x".isNullOrEmpty()) return "fail text empty"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_text_is_null_or_blank", source);
}

/// `takeWhile`/`dropWhile` split the text at the first char the predicate refuses.
#[test]
fn text_takes_and_drops_a_prefix_a_predicate_accepts() {
    let source = r#"
fun box(): String {
    val s = "aab1c"
    if (s.takeWhile { it != '1' } != "aab") return "fail take " + s.takeWhile { it != '1' }
    if (s.dropWhile { it != '1' } != "1c") return "fail drop " + s.dropWhile { it != '1' }
    if (s.takeWhile { false } != "") return "fail take none"
    if (s.dropWhile { false } != s) return "fail drop none"
    if (s.takeWhile { true } != s) return "fail take all"
    if (s.dropWhile { true } != "") return "fail drop all"
    if ("".takeWhile { true } != "") return "fail empty"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_text_take_while", source);
}

/// `sb.clear()` empties the builder and answers it, so a call can be chained.
#[test]
fn a_builder_clears_itself_and_answers_itself() {
    let source = r#"
fun box(): String {
    val sb = StringBuilder("junk")
    sb.clear()
    if (sb.length != 0) return "fail length"
    sb.append("O")
    if (sb.toString() != "O") return "fail reuse"
    if (StringBuilder("x").clear().append("K").toString() != "K") return "fail chain"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_text_builder_clear", source);
}
