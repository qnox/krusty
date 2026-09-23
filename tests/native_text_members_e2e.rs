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

use super::common::{
    expect_box_ok_with_stdlib, expect_native_box, expect_native_decline, kotlinc_box_result,
};

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

/// The parse Kotlin makes: an optional sign, ASCII digits and nothing else.
#[test]
fn text_parses_itself_as_an_integer() {
    let source = r#"
fun refused(run: () -> Unit): Boolean =
    try { run(); false } catch (e: NumberFormatException) { true }

fun box(): String {
    if ("123".toInt() != 123) return "fail plain"
    if ("-123".toInt() != -123) return "fail negative"
    if ("+7".toInt() != 7) return "fail signed"
    if ("0".toInt() != 0) return "fail zero"
    if ("2147483647".toInt() != Int.MAX_VALUE) return "fail max"
    if ("-2147483648".toInt() != Int.MIN_VALUE) return "fail min"
    if ("9223372036854775807".toLong() != Long.MAX_VALUE) return "fail long max"
    if (!refused { "".toInt() }) return "fail empty"
    if (!refused { "-".toInt() }) return "fail lone sign"
    if (!refused { "12x".toInt() }) return "fail trailing"
    if (!refused { " 12".toInt() }) return "fail space"
    if (!refused { "2147483648".toInt() }) return "fail over max"
    if (!refused { "-2147483649".toInt() }) return "fail under min"
    if (!refused { "99999999999999999999999".toInt() }) return "fail far over"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_text_to_int", source);
}

/// The `OrNull` forms answer nothing where the others raise.
#[test]
fn text_parses_itself_as_an_integer_or_nothing() {
    let source = r#"
fun box(): String {
    if ("123".toIntOrNull() != 123) return "fail plain"
    if ("12x".toIntOrNull() != null) return "fail bad"
    if ("".toIntOrNull() != null) return "fail empty"
    if ("2147483648".toIntOrNull() != null) return "fail over max"
    if ("2147483648".toLongOrNull() != 2147483648L) return "fail long"
    if ("9223372036854775808".toLongOrNull() != null) return "fail long over max"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_text_to_int_or_null", source);
}

/// `sb[i] = c` replaces one UTF-16 unit; `sb[i]++` is a read and one of these.
#[test]
fn a_builder_replaces_one_of_its_units() {
    let source = r#"
fun box(): String {
    val sb = StringBuilder("NK")
    sb[0]++
    if (sb.toString() != "OK") return "fail step " + sb.toString()
    val other = StringBuilder("abc")
    other[1] = 'X'
    if (other.toString() != "aXc") return "fail middle " + other.toString()
    other[2] = 'Z'
    if (other.toString() != "aXZ") return "fail last " + other.toString()
    other[0] = 'q'
    if (other.toString() != "qXZ") return "fail first " + other.toString()
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_text_builder_set", source);
}

/// `sb.substring(…)` is the builder's OWN member under a jar provider, and the runtime has none.
///
/// Worth pinning because the shape is a trap rather than a gap. Every text member here is declared
/// over `CharSequence`, which a builder is, and `kt_string_substring` shares a STRING's storage by
/// reading its fields directly — so a builder reaching it would read whatever a builder holds at
/// those offsets. A jar provider resolves the call to `java.lang.StringBuilder.substring` and it
/// declines by name; the runtime guards the receiver anyway, because a klib provider has no such
/// class to resolve to and the same call lands in the `kotlin.text` facade.
#[test]
fn a_builders_own_substring_is_declined_by_name() {
    let source = r#"
fun box(): String {
    val sb = StringBuilder("abcd")
    return if (sb.substring(1, 3) == "bc") "OK" else "fail"
}
"#;
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "unexpected kotlinc result"
    );
    expect_native_decline(
        source,
        "native_text_builder_substring",
        "java.lang.StringBuilder.substring",
    );
}

/// `s.toCharArray()`: the text's UTF-16 units, which is what a `CharArray` holds.
#[test]
fn text_hands_over_its_units_as_an_array() {
    let source = r#"
fun box(): String {
    val units = "OK".toCharArray()
    if (units.size != 2) return "fail size " + units.size
    if (units[0] != 'O' || units[1] != 'K') return "fail units"
    if ("".toCharArray().isNotEmpty()) return "fail empty"
    // The array is the caller's own: writing through it does not reach the text.
    units[0] = 'X'
    if ("OK"[0] != 'O') return "fail the text changed"
    var joined = ""
    for (unit in "abc".toCharArray()) joined += unit
    if (joined != "abc") return "fail walked " + joined
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_text_to_char_array", source);
}

/// `s.indexOfAny(chars)`: the first position holding any of the units.
///
/// Its other two parameters — a start index and `ignoreCase` — are not values the runtime is
/// given, so only a call that leaves both at their declared defaults reaches it. Those two arrive
/// differently by provider: a jar call leaves them out, a klib call materializes each as a
/// constant argument, and reading the ARGUMENTS rather than the signature makes them one answer.
#[test]
fn text_finds_the_first_position_holding_any_unit() {
    let source = r#"
fun box(): String {
    if ("123a".indexOfAny("a".toCharArray()) != 3) return "fail hit"
    if ("123".indexOfAny(charArrayOf('2', '3')) != 1) return "fail earliest"
    if ("123".indexOfAny(charArrayOf('9')) != -1) return "fail miss"
    if ("123".indexOfAny(charArrayOf()) != -1) return "fail none wanted"
    if ("".indexOfAny(charArrayOf('1')) != -1) return "fail empty text"
    // The FIRST position in the text, not the first wanted char's earliest position.
    if ("ba".indexOfAny(charArrayOf('a', 'b')) != 0) return "fail order"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_text_index_of_any", source);
}

/// A start index the program DID choose asks to skip a prefix, which the runtime entry point does
/// not do — so the call declines with its own argument still in sight rather than ignoring it.
#[test]
fn a_chosen_start_index_declines_rather_than_being_dropped() {
    let source = r#"
fun box(): String = if ("aba".indexOfAny(charArrayOf('a'), 1) == 2) "OK" else "fail"
"#;
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "unexpected kotlinc result"
    );
    expect_native_decline(source, "native_text_index_of_any_start", "indexOfAny");
}
