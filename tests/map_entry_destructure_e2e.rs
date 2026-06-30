//! Destructuring a `Map.Entry` (`for ((k, v) in map.entries) { … }`) resolves `component1`/`component2`,
//! which are `@InlineOnly` stdlib extensions (they inline to `getKey()`/`getValue()`) — reachable only
//! through the inline-callable resolution path, in both the checker and the lowerer. The keys/values
//! are typed `Any` (the entry's type arguments are erased), so they are used here through `Any`-valid
//! operations (string templates, which call `toString`). Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn for_destructure_over_map_entries() {
    const SRC: &str = "fun box(): String {\n\
        val m = linkedMapOf(\"O\" to \"K\")\n\
        var s = \"\"\n\
        for ((k, v) in m.entries) { s += \"$k$v\" }\n\
        return s\n\
    }\n";
    assert_eq!(
        run(SRC).expect("destructuring Map.Entry in a for-loop"),
        "OK"
    );
}

#[test]
fn destructure_single_map_entry() {
    const SRC: &str = "fun box(): String {\n\
        val m = linkedMapOf(\"O\" to \"K\")\n\
        val (k, v) = m.entries.first()\n\
        return \"$k$v\"\n\
    }\n";
    assert_eq!(run(SRC).expect("destructuring a single Map.Entry"), "OK");
}
