//! Exact builtin realization: compiler operations attach to normalized stdlib declarations, while
//! dependency members with the same operator spellings remain ordinary calls.

use super::common;

#[test]
fn same_named_dependency_operators_keep_their_declared_implementations() {
    const LIBRARY: &str = r#"
package dep

class Token(val value: Int) {
    operator fun plus(other: Token): Token = Token(value * 10 + other.value)
    operator fun rangeTo(other: Token): Span = Span(value * 10 + other.value)
}

class Span(val marker: Int)
"#;
    const CALLER: &str = r#"
import dep.Token

fun box(): String {
    if ((Token(2) + Token(3)).value != 23) return "plus"
    if ((Token(4)..Token(5)).marker != 45) return "rangeTo"
    return "OK"
}
"#;
    let Some(output) =
        common::expect_box_run_against("same_named_dependency_builtin_operators", LIBRARY, CALLER)
    else {
        return;
    };
    assert_eq!(output, "OK");
}

#[test]
fn exact_stdlib_scalar_declarations_keep_compiler_realizations() {
    const LIBRARY: &str = r#"
package dep

fun exactBuiltinMembers(): String {
    val byte: Byte = 2
    val short: Short = 3
    if (byte + short != 5) return "narrow-plus"
    val promoted: Long = 2 + 3L
    if (promoted != 5L) return "plus"
    if (-byte != -2) return "unary"
    if (3.75.toInt() != 3) return "conversion"
    if (2.compareTo(2.5) >= 0) return "compare"
    if ('d' - 'a' != 3) return "char-minus"
    if ('d' - 1 != 'c') return "char-offset"
    if (1.inv() != -2) return "inv"
    if ((6 and 3) != 2) return "and"
    if ((1 shl 4) != 16) return "shift"
    if ((16L ushr 2) != 4L) return "long-shift"
    if (!(true xor false)) return "boolean-xor"
    if (!(!false)) return "boolean-not"
    val any: Any? = 7
    if ("value=" + any != "value=7") return "string-plus"
    if ((1..3).sum() != 6) return "range-to"
    if ((1..<4).sum() != 6) return "range-until"
    return "OK"
}
"#;
    const CALLER: &str = "fun box(): String = dep.exactBuiltinMembers()\n";
    let Some(output) =
        common::expect_box_run_against("exact_stdlib_scalar_declarations", LIBRARY, CALLER)
    else {
        return;
    };
    assert_eq!(output, "OK");
}
