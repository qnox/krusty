//! A numeric primitive is assignable to `Number` (it boxes to its wrapper, which is a `Number`):
//! `fun f(n: Number)` accepts an `Int`, `val n: Number = 5`. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

#[test]
fn primitive_assignable_to_number() {
    const SRC: &str = "fun describe(n: Number): String = \"n=\" + n\n\
fun box(): String {\n\
    if (describe(5) != \"n=5\") return \"fail int: \" + describe(5)\n\
    if (describe(2.5) != \"n=2.5\") return \"fail double\"\n\
    val x: Number = 7L\n\
    if (describe(x) != \"n=7\") return \"fail val\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("primitive→Number assignability should compile + run");
    assert_eq!(out, "OK");
}
