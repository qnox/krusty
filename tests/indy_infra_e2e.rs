//! Validates the `invokedynamic` + `BootstrapMethods` constant-pool/attribute infrastructure in the
//! class writer by hand-building a class whose `run()` creates a lambda via `LambdaMetafactory`
//! (bound to the JDK functional interface `java.util.function.IntUnaryOperator`, so no kotlin-stdlib
//! is needed) and running it on the JVM under `-Xverify:all`. This is the foundation the IR lambda
//! lowering builds on; here we exercise only the bytecode emission.

use krusty::jvm::classfile::{ClassWriter, CodeBuilder, ACC_PRIVATE, ACC_PUBLIC, ACC_STATIC};
use std::fs;

use super::common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn invokedynamic_lambda_runs() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping indy_infra_e2e: set JAVA_HOME");
        return;
    };
    if !std::path::Path::new(&format!("{java_home}/bin/javac")).exists() {
        return;
    }

    let mut cw = ClassWriter::new("Indy", "java/lang/Object");

    // private static int lam(int n) { return n + 1; }
    let mut lam = CodeBuilder::new(1);
    lam.iload(0);
    lam.push_int(1, &mut cw);
    lam.iadd();
    lam.ireturn();
    lam.link();
    cw.add_method(ACC_PRIVATE | ACC_STATIC, "lam", "(I)I", &lam);

    // The bootstrap: LambdaMetafactory.metafactory(...), with static args
    // (samMethodType=(I)I, implMethod=Indy.lam, instantiatedMethodType=(I)I).
    let meta = cw.method_handle_static(
        "java/lang/invoke/LambdaMetafactory",
        "metafactory",
        "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;\
Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodHandle;Ljava/lang/invoke/MethodType;)\
Ljava/lang/invoke/CallSite;",
    );
    let sam = cw.method_type("(I)I");
    let impl_mh = cw.method_handle_static("Indy", "lam", "(I)I");
    let inst = cw.method_type("(I)I");
    let bsm = cw.add_bootstrap(meta, vec![sam, impl_mh, inst]);
    let indy = cw.invoke_dynamic(bsm, "applyAsInt", "()Ljava/util/function/IntUnaryOperator;");
    let app = cw.interface_methodref("java/util/function/IntUnaryOperator", "applyAsInt", "(I)I");

    // public static int run() { IntUnaryOperator f = <indy>; return f.applyAsInt(41); }
    let mut run = CodeBuilder::new(0);
    run.ensure_locals(1);
    run.invokedynamic(indy, 0, 1);
    run.astore(0);
    run.aload(0);
    run.push_int(41, &mut cw);
    run.invokeinterface(app, 1, 1);
    run.ireturn();
    run.link();
    cw.add_method(ACC_PUBLIC | ACC_STATIC, "run", "()I", &run);

    let dir = std::env::temp_dir().join(format!("krusty_indy_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Indy.class"), cw.finish()).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(Indy.run()); } }",
    )
    .unwrap();
    // Persistent `javac_run` (its JavaRunner JVM uses `-Xverify:all`, verifying the krusty-emitted
    // invokedynamic class on load) — no cold `javac`/`java` per case.
    let out = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        dir.to_str().unwrap(),
        dir.to_str().unwrap(),
        "M",
    );
    assert_eq!(
        out.as_deref().map(str::trim),
        Some("42"),
        "indy run: {out:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}
