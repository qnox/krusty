//! An instance member the call site cannot see does not shadow an applicable extension: kotlinc
//! ranks the invisible member below every visible candidate. A real-world SQL DSL declares
//! `Col.clone()` beside the protected `Object.clone`.

use super::common;

#[test]
fn an_extension_wins_over_the_protected_object_clone() {
    const SRC: &str = "class Col<T>(val name: String)\n\
class Alias {\n\
    private fun <T> Col<T>.clone() = Col<T>(name + \"!\")\n\
    fun one(c: Col<*>): String = c.clone().name\n\
    fun two(c: Any): String = ((c as? Col<*>)?.clone())?.name ?: \"none\"\n\
}\n\
fun <T> Col<T>.clone() = Col<T>(name + \"?\")\n\
fun top(c: Col<String>): String = c.clone().name\n\
fun ref(c: Col<String>): String = c.let(Col<String>::clone).name\n\
fun bound(c: Col<String>): String = c.let { (it::clone)() }.name\n\
fun box(): String {\n\
    val result = Alias().one(Col<Int>(\"a\")) + Alias().two(Col<Int>(\"b\")) + top(Col(\"c\")) + ref(Col(\"d\")) + bound(Col(\"e\"))\n\
    return if (result == \"a!b!c?d?e?\") \"OK\" else result\n\
}\n";
    assert_eq!(
        common::reference_error_messages("Main", SRC),
        Vec::<String>::new(),
        "kotlinc must select the clone extensions"
    );
    common::expect_box_ok_with_stdlib(SRC, "hidden_member_clone");
}

#[test]
fn an_invisible_member_without_an_extension_stays_an_access_error() {
    const SRC: &str = "open class B { protected fun foo(): Int = 1; private fun bar(): Int = 1 }\n\
fun t(b: B): Int = b.foo() + b.bar()\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        [
            "Main.kt:2:22: cannot access 'fun foo(): Int': it is protected in 'B'.",
            "Main.kt:2:32: cannot access 'fun bar(): Int': it is private in 'B'.",
        ]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn an_inapplicable_extension_leaves_the_hidden_operator_member_an_access_error() {
    const SRC: &str = "class B { private operator fun plus(x: Int): String = \"m\" }\n\
fun use(b: B): String = b + 1\n\
operator fun B.plus(x: String): String = x\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        ["Main.kt:2:27: cannot access 'fun plus(x: Int): String': it is private in 'B'."]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn an_ambiguous_visible_extension_family_hides_the_member() {
    const SRC: &str = "interface X\n\
interface Y\n\
class Both : X, Y\n\
open class B { protected operator fun plus(b: Both): String = \"m\" }\n\
operator fun B.plus(x: X): String = \"x\"\n\
operator fun B.plus(y: Y): String = \"y\"\n\
fun use(b: B) = b + Both()\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        [
            "Main.kt:7:19: overload resolution ambiguity between candidates:",
            "| fun B.plus(x: X): String",
            "| fun B.plus(y: Y): String",
        ]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn an_inapplicable_index_extension_leaves_the_hidden_get_an_access_error() {
    const SRC: &str = "open class B { protected operator fun get(i: Int): String = \"g\" }\n\
operator fun B.get(i: String): String = i\n\
fun bad(b: B) = b[0]\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        ["Main.kt:3:17: cannot access 'fun get(i: Int): String': it is protected in 'B'."]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn a_wrong_arity_extension_leaves_the_hidden_member_an_access_error() {
    const SRC: &str = "open class B { protected fun f(i: Int): String = \"f\" }\n\
fun B.f(i: Int, j: Int): String = \"e\"\n\
fun arity(b: B) = b.f(1)\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        ["Main.kt:3:21: cannot access 'fun f(i: Int): String': it is protected in 'B'."]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn a_reference_names_the_visible_extension_over_the_hidden_member() {
    // kotlinc selects the visible extension for `B::f`, not the protected member, so the
    // reference has the extension's shape and calls it.
    const MAIN: &str = "open class B { protected fun f(i: Int): String = \"m\" }\n\
fun B.f(i: Int, j: Int): String = \"O\" + (i + j)\n\
val ref: (B, Int, Int) -> String = B::f\n\
fun box(): String = if (ref(B(), 1, 1) == \"O2\") \"OK\" else ref(B(), 1, 1)\n";
    common::expect_box_same_as_kotlinc(MAIN, "HiddenMemberReference");
}
