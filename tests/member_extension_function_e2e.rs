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
            "inapplicable ordinary member falls through to top-level extension",
            r#"
                class Item {
                    fun render(value: String): String = value
                }
                fun Item.render(value: Int): Int = value
                fun convert(item: Item): Int = item.render(1)
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
            "vararg overload selection ignores declaration order",
            r#"
                class Item
                class Mapper {
                    fun Item.render(prefix: String, vararg values: Int): String = prefix
                    fun Item.render(prefix: Int, vararg values: Int): Int = prefix + values[0]
                    fun read(item: Item): Int = item.render(1, 2)
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
            "specific receiver beats generic receiver",
            r#"
                class Item
                class Mapper {
                    fun <T> T.pick(): String = "generic"
                    fun Item.pick(): Int = 1
                    fun read(item: Item): Int = item.pick()
                }
            "#,
            None,
        ),
        (
            "specific receiver beats inherited nested generic receiver",
            r#"
                class Box<T>
                open class BaseMapper {
                    fun <T> Box<T>.pick(): String = "generic"
                }
                class Mapper : BaseMapper() {
                    fun Box<String>.pick(): Int = 1
                    fun read(box: Box<String>): Int = box.pick()
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
fn safe_call_ordinary_member_keeps_precedence_over_member_extension() {
    const SOURCE: &str = r#"
        class Entry {
            fun resolve(predicate: (String) -> Boolean): String? = "ok".takeIf(predicate)
        }
        class Marker

        class Registry {
            private fun Entry.resolve(predicate: (Marker) -> Boolean): Marker? =
                Marker().takeIf(predicate)

            fun check(entry: Entry?): String? = entry?.resolve { it.length > 0 }
        }
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn generic_extension_chain_preserves_non_null_element_type() {
    const SOURCE: &str = r#"
        // LANGUAGE: +NameBasedDestructuring
        class Entry(val enabled: Boolean)
        class Scope
        class Marker(val accepted: Boolean)

        class Registry {
            private fun Entry.resolve(
                scope: Scope? = null,
                predicate: (Marker) -> Boolean = { true },
            ): Marker? = Marker(enabled).takeIf(predicate)

            private fun accepts(marker: Marker): Boolean = marker.accepted

            private fun Collection<Entry>.grouped(scope: Scope): Map<Boolean, List<Pair<Entry, Marker>>> =
                mapNotNull { entry ->
                    entry.takeIf { it.enabled }?.resolve(scope) { accepts(it) }?.let { entry to it }
                }.groupBy { [entry, marker] ->
                    entry.enabled && marker.accepted
                }
        }
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn member_extension_property_root_types_collection_lambda() {
    const SOURCE: &str = r#"
        interface Record {
            val payload: String
            val category: Int
        }

        interface RecordSet {
            val records: List<Record>
        }

        abstract class Processor {
            fun RecordSet.payloads(): List<String> =
                records.filter { it.category > 0 }.map { it.payload }
        }
    "#;

    common::expect_front_end_ok_files_with_stdlib(
        &[SOURCE],
        "member extension property root collection lambda",
    );
}

#[test]
fn safe_call_member_extensions_run() {
    const SOURCE: &str = r##"
        class Entry(val text: String)

        class Node(val text: String) {
            private fun Node.render(): String = text + "%"

            fun read(other: Node?): String = other?.render() ?: "none"
        }

        class Registry {
            private fun suffix(value: String): String = value

            private fun Entry.value(): String = text

            private fun Entry.resolve(transform: (String) -> String): String =
                transform(value()) + suffix("")

            private fun String.decorate(transform: (String) -> String): String =
                transform(this)

            private fun <T> T.adapt(transform: (T) -> T): T =
                transform(this)

            fun read(entry: Entry?): String = entry?.resolve { it + "!" } ?: "none"
            fun direct(entry: Entry): String = entry.resolve { it + "." }
            fun decorate(value: String?): String = value?.decorate { it + "?" } ?: "none"
            fun adapt(value: String?): String = value?.adapt { it + "*" } ?: "none"
        }

        object Formatter {
            private fun String.mark(): String = this + "#"

            fun render(value: String?): String = value?.mark() ?: "none"
        }

        fun box(): String {
            val registry = Registry()
            if (registry.read(Entry("OK")) != "OK!") return "object"
            if (registry.read(null) != "none") return "object-null"
            if (registry.direct(Entry("OK")) != "OK.") return "object-direct"
            if (registry.decorate("OK") != "OK?") return "string"
            if (registry.decorate(null) != "none") return "string-null"
            if (registry.adapt("OK") != "OK*") return "generic"
            if (registry.adapt(null) != "none") return "generic-null"
            if (Formatter.render("OK") != "OK#") return "object-dispatch"
            if (Formatter.render(null) != "none") return "object-dispatch-null"
            val node = Node("OK")
            if (node.read(Node("OK")) != "OK%") return "same-owner"
            if (node.read(null) != "none") return "same-owner-null"
            return "OK"
        }
    "##;

    common::expect_box_ok_with_stdlib(SOURCE, "S");
}

#[test]
fn named_member_extension_trailing_lambda_selects_before_lowering() {
    const SOURCE: &str = r#"
        class Item

        class Registry {
            private fun Item.render(value: Int, block: (Int) -> String): String =
                block(value)

            private fun Item.render(value: String, block: (String) -> String): String =
                block(value)

            fun direct(item: Item): String =
                item.render(value = 3) { "i$it" }

            fun safe(item: Item?): String =
                item?.render(value = "x") { "s$it" } ?: "none"
        }

        fun box(): String {
            val registry = Registry()
            return registry.direct(Item()) + "/" + registry.safe(Item())
        }
    "#;

    let result = common::compile_and_run_with_stdlib(SOURCE, "S");
    assert_eq!(result.as_deref(), Some("i3/sx"));
}

#[test]
fn member_extension_dispatch_and_erasure_run() {
    const SOURCE: &str = r#"
        open class Base {
            open val value: String = "OK"
        }

        class Derived : Base() {
            override val value: String = "wrong"
            fun selected(): String = "wrong"
        }

        class Host {
            inner class Inner

            infix fun <T> ArrayList<T>.append(value: T) {
                add(value)
            }

            fun Inner.copy(): Inner = Inner()

            fun Base?.read(): String {
                if (this is Derived) return selected()
                if (this != null) return value
                return "null"
            }

            fun check(): String {
                val values = ArrayList<Int>()
                values append 7
                if (values[0] != 7) return "erasure"
                if (Inner().copy() !is Inner) return "inner"
                return Base().read()
            }
        }

        class Item(val value: String)

        fun withItem(body: Item.() -> String): String = Item("OK").body()

        object Holder {
            private fun Item.read(): String = value
            val value: String = withItem { read() }
        }

        fun box(): String {
            if (Host().check() != "OK") return "dispatch"
            return Holder.value
        }
    "#;

    common::expect_box_ok_with_stdlib(SOURCE, "S");
}
