//! kotlinc's Java scope shows a Java method that overrides a builtin property realized under
//! another JVM name as that property: `java.util.HashMap.keySet()` is `keys` and `entrySet()` is
//! `entries`, read through `<get-keys>` and `<get-entries>`. The method's result is a Java value
//! nothing checked, so kotlinc's implicit not-null cast (`insertSpecialCast`) guards it wherever the
//! expected type rejects `null`: a declared result, an extension receiver, and an interface
//! delegation forwarder, naming the property's getter. A dispatch receiver stays unchecked.

use super::common;

const SOURCE: &str = r#"
import java.util.HashMap
import java.util.LinkedHashMap

class Table : Map<String, String> by HashMap<String, String>()

class MutableTable : MutableMap<String, String> by HashMap<String, String>()

open class Registry : HashMap<String, String>()

fun Iterable<*>.countOf(): Int {
    var count = 0
    for (element in this) count++
    return count
}

fun keysOf(map: HashMap<String, String>): Set<String> = map.keys

fun entriesOf(map: LinkedHashMap<String, String>): Set<Map.Entry<String, String>> = map.entries

fun registryKeys(registry: Registry): Collection<String> = registry.keys

fun entryCount(map: HashMap<String, String>): Int = map.entries.countOf()

fun box(): String {
    val map = HashMap<String, String>()
    map.put("O", "K")
    if (keysOf(map).size != 1) return "keys"
    val linked = LinkedHashMap<String, String>()
    linked.put("O", "K")
    if (entriesOf(linked).size != 1) return "entries"
    if (registryKeys(Registry()).size != 0) return "registry"
    if (Table().keys.size + Table().entries.size != 0) return "Table"
    val table = MutableTable()
    table.put("O", "K")
    if (table.keys.size != 1 || table.entries.size != 1) return "MutableTable"
    if (entryCount(map) != 1) return "entryCount"
    return map.get("O") ?: "missing"
}
"#;

#[test]
fn renamed_builtin_property_reads_are_checked_like_kotlinc() {
    let cases: &[(&str, &[&str])] = &[
        (
            "RenamedBuiltinPropertyKt",
            &["keysOf", "entriesOf", "registryKeys", "entryCount", "box"],
        ),
        ("Table", &["getKeys", "getEntries", "getValues"]),
        ("MutableTable", &["getKeys", "getEntries", "getValues"]),
    ];
    for (class, methods) in cases {
        let pair =
            common::ModuleClassPair::compile(&[("RenamedBuiltinProperty.kt", SOURCE)], class);
        for method in *methods {
            let (kotlinc, krusty) = pair.method_code(class, method);
            assert_eq!(krusty, kotlinc, "{class}.{method}");
        }
    }
}

#[test]
fn renamed_builtin_property_reads_run_like_kotlinc() {
    let jdk = common::jdk_modules();
    let output = common::compile_and_run_box(
        SOURCE,
        "RenamedBuiltinProperty",
        &[common::stdlib_jar()],
        Some(jdk.as_path()),
    )
    .expect("krusty compiles and runs the box");
    assert_eq!(output, "OK");
}
