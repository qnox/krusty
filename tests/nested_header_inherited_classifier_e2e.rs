//! A nested class's primary-constructor header sits inside its enclosing classes' bodies, so a
//! classifier those enclosing classes inherit is visible there by its simple name, ahead of a
//! same-named classifier of the file or an import. moshi's `JsonValueReader.JsonIterator(val endToken:
//! Token, …)` relies on `JsonReader.Token` inherited this way.

use super::common;

const ENCLOSING_INHERITED_CLASSIFIERS: &str = r#"class Token(val label: String) { override fun toString() = "top:$label" }

sealed class Reader {
    enum class Token { BEGIN, END }
    interface Kind
}

class ValueReader : Reader() {
    class Item(val token: Token, val kind: Kind?)
    class Span(val first: Token, val last: Token = Token.END)

    fun item(token: Token) = Item(token, null)
}

fun box(): String {
    val reader = ValueReader()
    val item = reader.item(Reader.Token.BEGIN)
    val span = ValueReader.Span(Reader.Token.BEGIN)
    val top = Token("t")
    val text = "${item.token}/${span.first}-${span.last}/$top"
    return if (text == "BEGIN/BEGIN-END/top:t") "OK" else text
}
"#;

#[test]
fn nested_class_header_sees_classifiers_its_enclosing_class_inherits() {
    common::expect_box_same_as_kotlinc(
        ENCLOSING_INHERITED_CLASSIFIERS,
        "NestedHeaderInheritedClassifier",
    );
}
