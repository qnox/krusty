//! JVM bridges for Kotlin collection properties.
use super::common;

fn run(src: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    common::compile_and_run_box(src, "Main", &[sl, jdk.clone()], Some(&jdk))
}

#[test]
fn collection_size_reachable_through_interface() {
    const SRC: &str = "class C : Collection<String> {\n\
        \x20   override val size: Int get() = 3\n\
        \x20   override fun isEmpty(): Boolean = false\n\
        \x20   override fun iterator(): Iterator<String> = throw UnsupportedOperationException()\n\
        \x20   override fun containsAll(elements: Collection<String>): Boolean = false\n\
        \x20   override fun contains(element: String): Boolean = false\n\
        }\n\
        fun box(): String {\n\
        \x20   val c: Collection<String> = C()\n\
        \x20   return if (c.size == 3) \"OK\" else \"F:${c.size}\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("Collection.size bridge compiles + runs"),
        "OK"
    );
}

#[test]
fn map_keys_bridge_uses_interface_return_type() {
    const SRC: &str = "class M(private val data: Map<String, Int>) : Map<String, Int> {\n\
    override val entries: Set<Map.Entry<String, Int>> get() = data.entries\n\
    override val keys: HashSet<String> get() = HashSet(data.keys)\n\
    override val size: Int get() = data.size\n\
    override val values: Collection<Int> get() = data.values\n\
    override fun containsKey(key: String): Boolean = data.containsKey(key)\n\
    override fun containsValue(value: Int): Boolean = data.containsValue(value)\n\
    override fun get(key: String): Int? = data[key]\n\
    override fun isEmpty(): Boolean = data.isEmpty()\n\
}\n\
fun box(): String {\n\
    val value: Map<String, Int> = M(mapOf(\"a\" to 1))\n\
    return if (value.keys.single() == \"a\") \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("Map.keys bridge"), "OK");
}
