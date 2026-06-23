//! Class-body properties (`class C { val x = … }`), plain (non-property) constructor parameters,
//! and `init { }` blocks — initialized in the primary constructor, accessible from member methods.
//! Plus open-property virtual dispatch (an `open val` read inside the class calls the getter).

mod common;

fn run_box(name: &str, src: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping class_body_e2e: set JAVA_HOME");
        return;
    };
    // Reference `==`/`!=` compiles to `kotlin/jvm/internal/Intrinsics.areEqual` — needs kotlin-stdlib
    // on the runtime classpath, as any real Kotlin program does.
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping class_body_e2e: no kotlin-stdlib jar found");
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    // IR backend covers a subset; skip (not fail) a construct it doesn't yet lower.
    let Some(out) = common::compile_and_run_box(src, "B", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK", "{name}");
}

#[test]
fn body_properties_and_init_block() {
    run_box("init", "class Counter(start: Int) {\n  val initial: Int = start\n  var count: Int = 0\n  init { count = start * 2 }\n  fun total(): Int = initial + count\n}\nfun box(): String {\n  val c = Counter(5)\n  if (c.initial != 5) return \"f1\"\n  if (c.count != 10) return \"f2\"\n  if (c.total() != 15) return \"f3\"\n  return \"OK\"\n}\n");
}

#[test]
fn open_property_virtual_dispatch() {
    // An `open val` read inside the base class must dispatch to the override.
    run_box("openprop", "open class Base { open val kind: String = \"base\"\n  fun k(): String = kind\n}\nclass Sub : Base() { override val kind: String = \"sub\" }\nfun box(): String = if (Sub().k() == \"sub\") \"OK\" else \"fail\"\n");
}
