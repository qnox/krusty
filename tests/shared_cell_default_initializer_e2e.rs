//! A captured `var` whose initializer is the value a fresh `Ref` holder's `element` already holds
//! (`null`, `false`, or a zero of the element's kind) sets no element, as kotlinc's does. `-0.0`
//! and any other value, and a later assignment of the default, are still stored.

use super::common;

const SOURCE: &str = r#"
fun sink(vararg values: Any?): Int = values.size

fun defaults(): Int {
    var aText: String? = null
    var bInt: Int = 0
    var cLong: Long = 0L
    var dFlag: Boolean = false
    var eFloat: Float = 0.0f
    var fDouble: Double = 0.0
    var gChar: Char = '\u0000'
    var hByte: Byte = 0
    var iShort: Short = 0
    var jBoxed: Int? = null
    fun assign() {
        aText = "b"; bInt = 1; cLong = 2L; dFlag = true; eFloat = 1f; fDouble = 1.0
        gChar = 'a'; hByte = 1; iShort = 1; jBoxed = 1
    }
    assign()
    return sink(aText, bInt, cLong, dFlag, eFloat, fDouble, gChar, hByte, iShort, jBoxed)
}

fun stored(k: Int): Int {
    var aNegativeZero: Double = -0.0
    var bOne: Int = 1
    var cCopied = k
    var dLater: Int
    dLater = 0
    var eText: String = ""
    fun assign() { aNegativeZero = 1.0; bOne = 2; cCopied = 3; dLater = 4; eText = "x" }
    assign()
    return sink(aNegativeZero, bOne, cCopied, dLater, eText)
}
"#;

#[test]
fn a_captured_var_initialized_to_its_default_sets_no_element_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SharedCellDefaults",
        SOURCE,
        &["SharedCellDefaultsKt"],
    );
}
