//! Java interop e2e: krust compiles Kotlin that calls a static method of a real javac-compiled
//! Java class, resolving the call by reading that class's `.class` from the classpath.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use krust::codegen::emit::emit_file;
use krust::diag::DiagSink;
use krust::jvm::classpath::Classpath;
use krust::lexer::lex;
use krust::parser::parse;
use krust::resolve::{check_file, collect_signatures};

fn have(t: &str) -> bool {
    Command::new(t).arg("-version").output().is_ok()
}

fn krust_compile(src: &str, internal: &str, cp_dirs: Vec<PathBuf>) -> Vec<u8> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let mut syms = collect_signatures(&files, &mut d);
    syms.classpath = Classpath::new(cp_dirs);
    let info = check_file(&files[0], &syms, &mut d);
    let bytes = emit_file(&files[0], &info, &syms, internal, &mut d);
    assert!(!d.has_errors(), "krust errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    bytes
}

#[test]
fn calls_real_java_static_method() {
    if !have("javac") || !have("java") {
        eprintln!("skipping: javac/java unavailable");
        return;
    }
    let root = std::env::temp_dir().join(format!("krust_javaint_{}", std::process::id()));
    let jdir = root.join("javacp"); // holds util/Calc.class
    let kdir = root.join("kr"); // holds DemoKt.class
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&jdir).unwrap();
    fs::create_dir_all(&kdir).unwrap();

    // A real Java class compiled by javac.
    fs::write(
        root.join("Calc.java"),
        r#"package util;
           public class Calc {
               public static int triple(int x) { return x * 3; }
               public static String tag(String s) { return "[" + s + "]"; }
           }"#,
    )
    .unwrap();
    let jc = Command::new("javac").args(["-d", jdir.to_str().unwrap(), "Calc.java"]).current_dir(&root).output().expect("javac");
    assert!(jc.status.success(), "javac failed: {}", String::from_utf8_lossy(&jc.stderr));
    assert!(jdir.join("util/Calc.class").exists());

    // krust compiles Kotlin that calls the Java class, resolving via the classpath.
    let src = r#"
        import util.Calc
        fun f(n: Int): Int = Calc.triple(n)
        fun g(s: String): String = Calc.tag(s)
        fun combined(n: Int): String = Calc.tag(Calc.triple(n).toString())
    "#;
    let bytes = krust_compile(src, "DemoKt", vec![jdir.clone()]);
    fs::write(kdir.join("DemoKt.class"), &bytes).unwrap();

    // Run: DemoKt (krust) + util/Calc (javac) on the classpath.
    let main = r#"
        public class Main {
            public static void main(String[] a) {
                System.out.println(DemoKt.f(5));
                System.out.println(DemoKt.g("hi"));
                System.out.println(DemoKt.combined(4));
            }
        }"#;
    fs::write(root.join("Main.java"), main).unwrap();
    let cp = format!("{}:{}", kdir.to_str().unwrap(), jdir.to_str().unwrap());
    let mc = Command::new("javac").args(["-cp", &cp, "Main.java"]).current_dir(&root).output().expect("javac main");
    assert!(mc.status.success(), "javac(Main) failed: {}", String::from_utf8_lossy(&mc.stderr));

    let run = Command::new("java")
        .args(["-Xverify:all", "-cp", &format!("{}:{}", root.to_str().unwrap(), cp), "Main"])
        .output()
        .expect("java");
    let out = String::from_utf8_lossy(&run.stdout);
    let err = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "run failed:\nstdout={out}\nstderr={err}");
    assert_eq!(out, "15\n[hi]\n[12]\n", "java-interop mismatch; stderr={err}");

    let _ = fs::remove_dir_all(&root);
}
