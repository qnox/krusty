//! kotlinc treats a fixed set of Java collection classes as pure implementations of a Kotlin
//! mutable collection interface, so their members take that interface's nullability: null is not a
//! `String` for `ArrayList<String>.add`, and a map's `put` rejects a null value. The interfaces and
//! every other Java class (`Vector`, `ConcurrentMap`) keep their flexible parameters.

use super::common;

const SOURCE: &str = "fun lists() {
    java.util.ArrayList<String>().add(null)
    java.util.LinkedList<String>().add(null)
    java.util.ArrayList<String?>().add(null)
}

fun sets() {
    java.util.HashSet<String>().add(null)
    java.util.LinkedHashSet<String>().add(null)
    java.util.TreeSet<String>().add(null)
}

fun maps() {
    java.util.HashMap<String, String>().put(\"a\", null)
    java.util.LinkedHashMap<String, String>().put(\"a\", null)
    java.util.TreeMap<String, String>().put(\"a\", null)
    java.util.concurrent.ConcurrentHashMap<String, String>().put(\"a\", null)
    java.util.concurrent.ConcurrentSkipListMap<String, String>().put(\"a\", null)
    java.util.HashMap<String, String?>().put(\"a\", null)
}

fun collections(map: Map<String, String>, list: List<String>) {
    java.util.HashMap<String, String>().putAll(map)
    java.util.ArrayList<String>().addAll(list)
    java.util.HashMap<String, String>(map)
    java.util.ArrayList<String>(list)
}

fun impure() {
    java.util.Vector<String>().add(null)
    java.util.Hashtable<String, String>().put(\"a\", null)
    java.util.ArrayDeque<String>().add(null)
}
";

fn render(diagnostics: Vec<common::CompilerError>) -> Vec<String> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "{}:{}:{}: {}",
                diagnostic.file, diagnostic.line, diagnostic.column, diagnostic.message
            )
        })
        .collect()
}

#[test]
fn a_pure_collection_implementation_takes_its_kotlin_interfaces_parameters() {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Use.kt", SOURCE)],
        &[],
        &["-no-reflect".to_string()],
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty = render(common::compiler_errors(&krusty_output));
    let kotlinc = render(common::compiler_errors(&result.reference_stderr));
    assert_eq!(krusty, kotlinc);
    assert_eq!(
        kotlinc,
        [
            "Use.kt:2:39: null cannot be a value of a non-null type 'String'.",
            "Use.kt:3:40: null cannot be a value of a non-null type 'String'.",
            "Use.kt:8:37: null cannot be a value of a non-null type 'String'.",
            "Use.kt:9:43: null cannot be a value of a non-null type 'String'.",
            "Use.kt:10:37: null cannot be a value of a non-null type 'String'.",
            "Use.kt:14:50: null cannot be a value of a non-null type 'String'.",
            "Use.kt:15:56: null cannot be a value of a non-null type 'String'.",
            "Use.kt:16:50: null cannot be a value of a non-null type 'String'.",
            "Use.kt:17:71: null cannot be a value of a non-null type 'String'.",
            "Use.kt:18:75: null cannot be a value of a non-null type 'String'."
        ]
    );
}
