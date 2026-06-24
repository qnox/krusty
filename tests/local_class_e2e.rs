//! A local class/interface declared inside a function body (`class L { … }`) is hoisted to a
//! top-level-equivalent class and lowered like any other class. Slice 1: non-capturing local classes
//! (a capturing one — e.g. a super-constructor argument referencing an outer local — stays unsupported
//! and the file skips, never miscompiles). Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

#[test]
fn local_class_basic() {
    const SRC: &str = "fun box(): String {\n\
    class Pair(val a: Int, val b: Int) {\n\
        fun sum() = a + b\n\
    }\n\
    val p = Pair(1, 2)\n\
    if (p.sum() != 3) return \"fail sum\"\n\
    if (p.a != 1 || p.b != 2) return \"fail fields\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("local class should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn local_data_class_and_interface() {
    // A local `data class` (component/equals via the data-class machinery) and a local `interface`
    // implemented by a local class — all non-capturing (slice 1).
    const SRC: &str = "fun box(): String {\n\
    data class Point(val x: Int, val y: Int)\n\
    val p = Point(3, 4)\n\
    if (p.x + p.y != 7) return \"fail data\"\n\
    if (p != Point(3, 4)) return \"fail eq\"\n\
    interface Greeter { fun hi(): String }\n\
    class En : Greeter { override fun hi() = \"hi\" }\n\
    if (En().hi() != \"hi\") return \"fail iface\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("local data class + local interface should compile + run");
    assert_eq!(out, "OK");
}
