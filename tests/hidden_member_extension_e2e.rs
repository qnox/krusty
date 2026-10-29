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
