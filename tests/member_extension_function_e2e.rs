use super::common;

#[test]
fn member_extension_function_resolution() {
    const CASES: &[(&str, &str, Option<&str>)] = &[
        (
            "explicit receiver with implicit dispatch receiver",
            r#"
                class Item(val text: String)
                object Mapper {
                    fun convert(item: Item): String = item.render()
                    private fun Item.render(): String = text
                }
            "#,
            None,
        ),
        (
            "missing dispatch receiver",
            r#"
                class Item
                class Mapper {
                    fun Item.render(): String = "OK"
                }
                fun convert(item: Item): String = item.render()
            "#,
            Some("unresolved"),
        ),
        (
            "ordinary member takes precedence",
            r#"
                class Item {
                    fun render(): Int = 1
                }
                class Mapper {
                    fun Item.render(): String = "extension"
                    fun convert(item: Item): Int = item.render()
                }
            "#,
            None,
        ),
        (
            "inapplicable ordinary member falls through to member extension",
            r#"
                class Item {
                    fun render(value: String, block: (String) -> String): String = block(value)
                }
                class Mapper {
                    fun Item.render(value: Int, block: (Int) -> Int): Int = block(value)
                    fun convert(item: Item): Int = item.render(1) { it * 2 }
                }
            "#,
            None,
        ),
        (
            "applicable ordinary vararg member keeps precedence",
            r#"
                class Item {
                    fun render(vararg values: Int): Int = values[0]
                }
                class Mapper {
                    fun Item.render(first: Int, second: Int): String = "extension"
                    fun convert(item: Item): Int = item.render(1, 2)
                }
            "#,
            None,
        ),
        (
            "ordinary vararg spread keeps precedence",
            r#"
                class Item {
                    fun render(vararg values: Int): Int = values[0]
                }
                class Mapper {
                    fun Item.render(values: IntArray): String = "extension"
                    fun convert(item: Item, values: IntArray): Int = item.render(*values)
                }
            "#,
            None,
        ),
        (
            "supertype extension receiver",
            r#"
                open class Item
                class SpecialItem : Item()
                class Mapper {
                    fun Item.render(): String = "OK"
                    fun convert(item: SpecialItem): String = item.render()
                }
            "#,
            None,
        ),
        (
            "generic receiver and argument",
            r#"
                class Mapper {
                    fun <T> T.combine(other: T): T = other
                    fun convert(item: String): Int = item.combine("OK").length
                }
            "#,
            None,
        ),
        (
            "explicit function type argument",
            r#"
                class Item
                abstract class Mapper {
                    abstract fun <T> Item.convert(): T
                    fun read(item: Item): Int = item.convert<String>().length
                }
            "#,
            None,
        ),
        (
            "named and default arguments",
            r#"
                class Item
                class Mapper {
                    fun Item.render(prefix: String = "", value: String): String = prefix + value
                    fun read(item: Item): String = item.render(value = "OK")
                }
            "#,
            None,
        ),
        (
            "vararg arguments",
            r#"
                class Item
                class Mapper {
                    fun Item.render(vararg values: String): String = values[0]
                    fun read(item: Item): String = item.render("O", "K")
                }
            "#,
            None,
        ),
        (
            "member extension accepts a vararg spread",
            r#"
                class Item
                class Mapper {
                    fun Item.render(vararg values: Int): Int = values[0]
                    fun read(item: Item, values: IntArray): Int = item.render(*values)
                }
            "#,
            None,
        ),
        (
            "generic member extension infers a vararg spread element",
            r#"
                class Item
                class Mapper {
                    fun <T> Item.render(vararg values: T): T = values[0]
                    fun read(item: Item, values: IntArray): Int = item.render(*values)
                }
            "#,
            None,
        ),
        (
            "lambda parameter is contextually typed",
            r#"
                class Item(val text: String)
                class Mapper {
                    fun Item.render(block: (Item) -> String): String = block(this)
                    fun read(item: Item): String = item.render { it.text }
                }
            "#,
            None,
        ),
        (
            "lambda overload uses typed non-lambda arguments",
            r#"
                class Item
                class Mapper {
                    fun Item.render(value: String, block: (String) -> String): String = block(value)
                    fun Item.render(value: Int, block: (Int) -> Int): Int = block(value)
                    fun read(item: Item): Int = item.render(1) { it * 2 }
                }
            "#,
            None,
        ),
        (
            "receiver lambda is contextually typed",
            r#"
                class Item
                class Block(val text: String)
                class Mapper {
                    fun Item.render(block: Block.() -> String): String = Block("OK").block()
                    fun read(item: Item): String = item.render { text }
                }
            "#,
            None,
        ),
        (
            "named overload selects the exact argument type",
            r#"
                open class General
                class Item : General()
                class Target
                class Mapper {
                    fun Target.pick(value: General): String = "general"
                    fun Target.pick(value: Item): Int = 1
                    fun read(target: Target, item: Item): Int = target.pick(value = item)
                }
            "#,
            None,
        ),
        (
            "inferred return reaches a later member extension",
            r#"
                class Item
                class Mapper {
                    fun Item.first() = second()
                    fun Item.second() = "OK"
                    fun convert(item: Item): Int = item.first().length
                }
            "#,
            None,
        ),
        (
            "inherited member extension override",
            r#"
                class Item
                abstract class BaseMapper {
                    protected abstract fun Item.render(prefix: String): String
                }
                class Mapper : BaseMapper() {
                    override fun Item.render(prefix: String): String = prefix
                    fun convert(item: Item): String = item.render("OK")
                }
            "#,
            None,
        ),
        (
            "inherited generic dispatch type is substituted for override",
            r#"
                abstract class BaseMapper<T> {
                    protected abstract fun T.render(value: T): String
                }
                class Mapper : BaseMapper<String>() {
                    override fun String.render(value: String): String = value
                    fun read(value: String): String = value.render("OK")
                }
            "#,
            None,
        ),
        (
            "private extension remains inaccessible in an external dispatch scope",
            r#"
                class Item
                class Mapper {
                    private fun Item.render(): String = "OK"
                }
                fun convert(mapper: Mapper, item: Item): String =
                    mapper.run { item.render() }
            "#,
            Some("cannot access 'render'"),
        ),
        (
            "override requires the same extension receiver",
            r#"
                class Item
                class OtherItem
                abstract class BaseMapper {
                    abstract fun Item.render(value: String): String
                }
                class Mapper : BaseMapper() {
                    override fun OtherItem.render(value: String): String = value
                }
            "#,
            Some("'render' overrides nothing"),
        ),
        (
            "override requires the same value parameter types",
            r#"
                class Item
                abstract class BaseMapper {
                    abstract fun Item.render(value: String): String
                }
                class Mapper : BaseMapper() {
                    override fun Item.render(value: Int): String = value.toString()
                }
            "#,
            Some("'render' overrides nothing"),
        ),
    ];

    for (name, source, expected) in CASES {
        let diagnostics = common::front_end_diagnostics_files(&[*source], &[], None);
        match expected {
            None => assert!(
                diagnostics.is_empty(),
                "{name}: unexpected diagnostics: {diagnostics:?}"
            ),
            Some(expected) => assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.contains(expected)),
                "{name}: expected diagnostic containing {expected:?}, got {diagnostics:?}"
            ),
        }
    }
}

#[test]
fn generic_member_extension_boxes_erased_argument() {
    const SOURCE: &str = r#"
        class Collector {
            fun <T> ArrayList<T>.append(value: T): Boolean = add(value)

            fun fill(values: ArrayList<Int>) {
                values.append(1)
            }
        }

        fun box(): String {
            val values = ArrayList<Int>()
            Collector().fill(values)
            return if (values[0] == 1) "OK" else "FAIL"
        }
    "#;

    common::expect_box_ok_with_stdlib(SOURCE, "GenericMemberExtensionArgument");
}
