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

/// A class's own primary-constructor header sees the class's body scope, so the classifiers the
/// class inherits rank ahead of a same-named top-level classifier and an explicit import, in a
/// parameter type and in a default value alike. An expression root commits to that classifier too:
/// `Token.Child()` is `Base.Token.Child`, although the top-level `Token` has no `Child`.
const OWN_INHERITED_HEADER_CLASSIFIERS: &str = r#"import kotlin.text.Regex

open class Base {
    class Token {
        class Child
    }
    class Regex
}

class Token

class Holder(
    val token: Token = Token(),
    val child: Any = Token.Child(),
    val regex: Regex = Regex(),
) : Base()

fun box(): String {
    val holder = Holder(token = Base.Token(), regex = Base.Regex())
    val defaults = Holder()
    val text = "${defaults.token is Base.Token}/${defaults.child is Base.Token.Child}/" +
        "${defaults.regex is Base.Regex}/${holder.token is Base.Token}"
    return if (text == "true/true/true/true") "OK" else text
}
"#;

#[test]
fn class_header_binds_the_classifiers_its_class_inherits() {
    common::expect_box_same_as_kotlinc(
        OWN_INHERITED_HEADER_CLASSIFIERS,
        "OwnInheritedHeaderClassifiers",
    );
}
