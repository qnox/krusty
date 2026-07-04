use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn java_runnable_sam_lambda_runs() {
    const SRC: &str = "import java.lang.Runnable\n\
fun box(): String {\n\
    var s = \"\"\n\
    val r = Runnable { s = \"OK\" }\n\
    r.run()\n\
    return s\n\
}\n";
    assert_eq!(run(SRC).expect("Runnable SAM lambda compiles + runs"), "OK");
}

#[test]
fn classpath_sam_bridge_for_comparable_runs() {
    const SRC: &str = "class C : Comparable<C> {\n\
    override fun compareTo(other: C): Int = 7\n\
}\n\
fun box(): String {\n\
    val c: Comparable<C> = C()\n\
    return if (c.compareTo(C()) == 7) \"OK\" else \"no\"\n\
}\n";
    assert_eq!(run(SRC).expect("Comparable bridge compiles + runs"), "OK");
}
