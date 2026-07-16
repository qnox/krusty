//! Property-signature inference of a primitive numeric/char CONVERSION initializer
//! (`val ten = 10.toLong()`) — the target primitive is fixed by the conversion method name, so the
//! signature phase can type the property without an explicit annotation (mirrors the full checker).
mod common;
use common::assert_box_ok_with_stdlib;

#[test]
fn property_from_numeric_conversion() {
    let src = r#"
val ten = 10.toLong()
val small = 300.toByte()
val frac = 7.toDouble()
val ch = 65.toChar()
fun box(): String {
    if (ten != 10L) return "fail: ten"
    if (small.toInt() != 44) return "fail: small"
    if (frac != 7.0) return "fail: frac"
    if (ch != 'A') return "fail: ch"
    return "OK"
}
"#;
    assert_box_ok_with_stdlib(src, "property_from_numeric_conversion");
}
