//! Destructuring (`val (a, b) = x`) where `componentN` are user-defined `operator fun` EXTENSIONS
//! (not class members). The checker resolves them via the module's extension functions and the lowerer
//! emits each as a static extension call. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn destructure_via_component_extensions() {
    const SRC: &str = "class C(val i: Int)\n\
operator fun C.component1() = i + 1\n\
operator fun C.component2() = i + 2\n\
fun box(): String {\n\
    val (a, b) = C(10)\n\
    return if (a == 11 && b == 12) \"OK\" else \"fail: $a,$b\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("destructure via component extensions"),
        "OK"
    );
}

#[test]
fn destructure_extension_string_components() {
    // Component extensions returning a reference type.
    const SRC: &str = "class Pair2(val a: String, val b: String)\n\
operator fun Pair2.component1() = a\n\
operator fun Pair2.component2() = b\n\
fun box(): String {\n\
    val (x, y) = Pair2(\"O\", \"K\")\n\
    return x + y\n\
}\n";
    assert_eq!(run(SRC).expect("ref-typed component extensions"), "OK");
}
