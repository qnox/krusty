//! A member-extension `operator fun <T> Key<in List<T>>.invoke(vararg values: T)` on an implicit
//! receiver applies to a `Key<List<String>>`: its own type parameter is inferred from that receiver
//! (`T = String`), not replaced by its bound first. This is the shape of the Kotlin scripting API's
//! configuration builders (`defaultImports("kotlin.math.*")`). A generic vararg is packed into an
//! array of the inferred element type, whichever call form selects it.

use super::common;

const KEYS: &str = "class Key<T>\n\
interface Keys\n\
open class Builder {\n\
\x20   operator fun <T> Key<T>.invoke(value: T) { println(\"one\") }\n\
\x20   operator fun <T> Key<in List<T>>.invoke(vararg values: T) { println(values.size) }\n\
}\n\
class Configuration : Builder(), Keys\n\
val Keys.items: Key<List<String>> get() = Key()\n\
fun build(block: Configuration.() -> Unit) = Configuration().block()\n";

fn assert_matches_kotlinc(source: &str) {
    let classes = common::classes_against_kotlinc_module(&[("Keys.kt", source)]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn a_projected_member_extension_invoke_infers_its_type_parameter_from_the_receiver_like_kotlinc() {
    assert_matches_kotlinc(&format!(
        "{KEYS}fun main() = build {{ items(\"a\", \"b\") }}\n"
    ));
}

#[test]
fn an_inapplicable_member_extension_invoke_is_reported_like_kotlinc() {
    let source = KEYS.replace(
        "\x20   operator fun <T> Key<in List<T>>.invoke(vararg values: T) { println(values.size) }\n",
        "",
    ) + "fun main() = build { items(\"a\") }\n";
    let result = common::compiler_diagnostics(
        &[("Keys.kt", &source)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&result, "inapplicable member-extension invoke");
}

#[test]
fn an_extension_invoke_packs_its_inferred_vararg_element_like_kotlinc() {
    assert_matches_kotlinc(
        "class Key<T>\n\
         operator fun <T> Key<in List<T>>.invoke(vararg values: T) = values.size\n\
         val items: Key<List<String>> = Key()\n\
         fun main() { println(items(\"a\", \"b\")) }\n",
    );
}

#[test]
fn a_member_extension_packs_its_inferred_vararg_element_like_kotlinc() {
    assert_matches_kotlinc(
        "class Key<T>\n\
         class Lists { fun <T> Key<List<T>>.put(vararg values: T) = values.size }\n\
         class Strings { fun <T> String.put(vararg values: T) = values.size }\n\
         val items: Key<List<String>> = Key()\n\
         fun main() {\n\
         \x20   println(with(Lists()) { items.put(\"a\") })\n\
         \x20   println(with(Strings()) { \"x\".put(1, 2) })\n\
         }\n",
    );
}
