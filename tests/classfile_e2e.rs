//! End-to-end: emit a real class with krusty's class-file writer, then have the JVM load, VERIFY,
//! and run it (via a Java `Main` that calls the method). This is the Phase 3 exit gate.

use std::fs;

use krusty::jvm::classfile::*;

use super::common;

#[test]
fn emitted_add_class_verifies_and_runs() {
    if common::java_home().is_none() {
        eprintln!("skipping: javac/java not available");
        return;
    }

    // FooKt.add(int,int):int = a + b
    let mut cw = ClassWriter::new("FooKt", "java/lang/Object");
    let mut code = CodeBuilder::new(2);
    code.iload(0);
    code.iload(1);
    code.iadd();
    code.ireturn();
    cw.add_method(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, "add", "(II)I", &code);
    let bytes = cw.finish();

    let dir = std::env::temp_dir().join(format!("krusty_e2e_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("FooKt.class"), &bytes).unwrap();
    fs::write(
        dir.join("Main.java"),
        "public class Main { public static void main(String[] a){ System.out.println(FooKt.add(3,4)); } }",
    )
    .unwrap();

    // Compile the Java driver + run it through the persistent `javac_run` server (its `JavaRunner`
    // JVM runs with `-Xverify:all`, so the krusty-emitted class is still fully bytecode-verified on
    // load). No cold `javac`/`java` spawn per case.
    let out = common::javac_run(
        dir.join("Main.java").to_str().unwrap(),
        dir.to_str().unwrap(),
        dir.to_str().unwrap(),
        "Main",
    );
    assert_eq!(
        out.as_deref().map(str::trim),
        Some("7"),
        "javac/run of krusty-emitted class (verify/result): {out:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}
