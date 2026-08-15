//! A member property initialized by a FULLY-QUALIFIED generic Java constructor.
//!
//! Shape (in-memory repository mocks): `private val items =
//! java.util.concurrent.CopyOnWriteArrayList<Item>()` — no import, explicit type argument, Java
//! class. Signature inference must type the property `CopyOnWriteArrayList<Item>` so collection
//! lambdas keep the element type; failing surfaces as "cannot infer the type of property" and every
//! `it.<member>` in a lambda over it cascades to "unresolved reference". The simple-name spelling
//! (`CopyOnWriteArrayList<Item>()` under an import) already worked — only the qualified spelling
//! fell through the pre-pass.
use super::common;

fn run(tag: &str, lib: &str, main: &str) -> Option<String> {
    common::expect_box_run_against(tag, lib, main)
}

const LIB: &str = "package lib\n\
    sealed interface Item { val id: String }\n\
    data class RealItem(override val id: String) : Item\n";

#[test]
fn member_property_from_fq_generic_java_ctor_keeps_element_type() {
    const MAIN: &str = "import lib.Item\n\
        import lib.RealItem\n\
        class Store {\n\
            private val items = java.util.concurrent.CopyOnWriteArrayList<Item>()\n\
            fun add(item: Item) { items.add(item) }\n\
            fun find(want: String): Int = items.indexOfFirst { it.id == want }\n\
        }\n\
        fun box(): String {\n\
            val s = Store()\n\
            s.add(RealItem(\"a\"))\n\
            s.add(RealItem(\"b\"))\n\
            return if (s.find(\"b\") == 1) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run("fq1", LIB, MAIN).expect("fq generic java ctor property"),
        "OK"
    );
}

#[test]
fn top_level_property_from_fq_generic_java_ctor() {
    // The top-level pre-pass shares the channel; pin it too.
    const MAIN: &str = "import lib.Item\n\
        import lib.RealItem\n\
        private val registry = java.util.concurrent.ConcurrentHashMap<String, Item>()\n\
        fun box(): String {\n\
            registry[\"k\"] = RealItem(\"v\")\n\
            return if (registry[\"k\"]?.id == \"v\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run("fq2", LIB, MAIN).expect("fq generic java ctor top-level property"),
        "OK"
    );
}
