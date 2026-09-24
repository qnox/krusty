//! A type parameter in the ACTUAL extension receiver belongs to the caller and is a fixed type at
//! the call site, not a wildcard. Repository-owned generic receiver families keep this regression
//! independent of stdlib intrinsic recognition.

use super::common;

#[test]
fn a_caller_bounded_type_parameter_receiver_selects_the_generic_extension() {
    common::expect_box_same_as_kotlinc(
        "interface Rank<S>\n\
class Amber : Rank<Amber>\n\
class Blue : Rank<Blue>\n\
class Tray<T>\n\
@JvmName(\"tagAmber\") fun Tray<Amber>.tag(): String = \"amber\"\n\
@JvmName(\"tagBlue\") fun Tray<Blue>.tag(): String = \"blue\"\n\
fun <T : Rank<T>> Tray<T>.tag(): String = \"generic\"\n\
fun <T> throughWhere(value: Tray<T>): String where T : Rank<T> = value.tag()\n\
fun <T : Rank<T>> throughInline(value: Tray<T>): String = value.tag()\n\
fun box(): String {\n\
    if (throughWhere(Tray<Amber>()) != \"generic\") return \"where\"\n\
    if (throughInline(Tray<Blue>()) != \"generic\") return \"inline\"\n\
    if (Tray<Amber>().tag() != \"amber\") return \"concrete\"\n\
    return \"OK\"\n\
}\n",
        "caller_bounded_receiver_generic_extension",
    );
}

/// A source overload pair shaped like the stdlib's: a concrete-element `@JvmName` variant beside the
/// generic one. A caller's `Iterable<T>` reaches only the generic declaration, as in kotlinc.
#[test]
fn a_caller_type_parameter_receiver_does_not_match_a_concrete_element_overload() {
    common::expect_box_same_as_kotlinc(
        "interface Token<T>\n\
class Letter : Token<Letter>\n\
class Digit : Token<Digit>\n\
class Crate<T>\n\
@JvmName(\"routeLetter\") fun Crate<Letter>.route(): String = \"letter\"\n\
fun <T : Token<T>> Crate<T>.route(): String = \"generic\"\n\
fun <T : Token<T>> viaCaller(value: Crate<T>): String = value.route()\n\
fun box(): String {\n\
    if (viaCaller(Crate<Digit>()) != \"generic\") return \"caller\"\n\
    if (Crate<Letter>().route() != \"letter\") return \"concrete\"\n\
    return \"OK\"\n\
}\n",
        "caller_type_param_receiver_overload",
    );
}
