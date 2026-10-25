//! JVM signatures use the retained enclosing method's formal identity on anonymous class members.

use super::common;

const SOURCE: &str = r#"
interface Token<T> { fun read(): T }
private fun <T> make(value: T) = object : Token<T> {
    val extra: T = value
    var nullable: T? = value
    override fun read(): T = value
}
private fun <T : Number> bounded(value: T) = object : Token<T> {
    val extra: T = value
    var nullable: T? = value
    override fun read(): T = value
}
class Declared<T>(var item: T)
class Plain(val value: Int, var text: String?)
fun box(): String {
    val first = make("OK")
    val number = bounded(7)
    first.nullable = null
    number.nullable = null
    return if (first.extra == "OK" && first.read() == "OK" && first.nullable == null &&
        number.extra == 7 && number.read() == 7 && number.nullable == null &&
        Declared("OK").item == "OK" && Plain(7, null).value == 7) "OK" else "value"
}
"#;

// PLATFORM: complete JVM class inventory, headers and every field/method signature and access flag.
#[test]
fn anonymous_members_keep_enclosing_method_formal_signatures_like_kotlinc() {
    let classes =
        common::classes_against_kotlinc_module(&[("EnclosingFormalSignatures.kt", SOURCE)]);
    let inventory = |classes: &std::collections::BTreeMap<String, Vec<u8>>| {
        classes
            .iter()
            .map(|(name, bytes)| {
                let info = krusty::jvm::classreader::parse_class(bytes).expect("readable class");
                (
                    name.clone(),
                    (
                        format!(
                            "{:?}",
                            (
                                info.access,
                                info.this_class,
                                info.super_class,
                                info.interfaces(),
                                info.signature,
                                info.inner_classes,
                            )
                        ),
                        common::member_table(bytes),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(inventory(&classes.krusty), inventory(&classes.reference));
}

#[test]
fn enclosing_method_formal_members_run_like_kotlinc() {
    common::expect_box_same_as_kotlinc(SOURCE, "enclosing_method_formal_members");
}
