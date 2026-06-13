//! End-to-end through the full krust pipeline: source → lex → parse → typecheck → emit class,
//! then JVM-verify + run via a Java `Main`, comparing results to the expected Kotlin semantics.

use std::fs;
use std::process::Command;

use krust::codegen::emit::emit_file;
use krust::diag::DiagSink;
use krust::lexer::lex;
use krust::parser::parse;
use krust::resolve::{check_file, collect_signatures};

fn have(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok()
}

/// Compile one source string into class bytes named `internal_name` (e.g. "DemoKt").
fn compile(src: &str, internal_name: &str) -> Result<Vec<u8>, Vec<String>> {
    let mut diags = DiagSink::new();
    let toks = lex(src, &mut diags);
    let file = parse(src, &toks, &mut diags);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut diags);
    let info = check_file(&files[0], &syms, &mut diags);
    let bytes = emit_file(&files[0], &info, &syms, internal_name, &mut diags);
    if diags.has_errors() {
        return Err(diags.diags.iter().map(|d| d.msg.clone()).collect());
    }
    Ok(bytes)
}

#[test]
fn numeric_and_concat_pipeline() {
    if !have("javac") || !have("java") {
        eprintln!("skipping: javac/java unavailable");
        return;
    }

    let src = r#"
        fun add(a: Int, b: Int): Int = a + b
        fun precedence(a: Int, b: Int, c: Int): Int = a + b * c
        fun div(a: Int, b: Int): Int = a / b
        fun neg(a: Int): Int = -a
        fun promote(a: Long, b: Int): Long = a + b
        fun mixed(a: Double, b: Int): Double = a * b + 1
        fun concat(a: Int, b: String): String = a.toString() + b
        fun greet(name: String): String = "hi " + name
    "#;

    let bytes = match compile(src, "DemoKt") {
        Ok(b) => b,
        Err(errs) => panic!("krust compile errors: {errs:?}"),
    };

    let dir = std::env::temp_dir().join(format!("krust_pipe_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("DemoKt.class"), &bytes).unwrap();

    let main = r#"
        public class Main {
            public static void main(String[] x) {
                System.out.println(DemoKt.add(3, 4));
                System.out.println(DemoKt.precedence(2, 3, 4));
                System.out.println(DemoKt.div(7, 2));
                System.out.println(DemoKt.neg(5));
                System.out.println(DemoKt.promote(5L, 3));
                System.out.println(DemoKt.mixed(2.5, 4));
                System.out.println(DemoKt.concat(42, "!"));
                System.out.println(DemoKt.greet("bob"));
            }
        }
    "#;
    fs::write(dir.join("Main.java"), main).unwrap();

    let javac = Command::new("javac")
        .args(["-cp", dir.to_str().unwrap(), "Main.java"])
        .current_dir(&dir)
        .output()
        .expect("javac");
    assert!(javac.status.success(), "javac rejected krust output:\n{}", String::from_utf8_lossy(&javac.stderr));

    let run = Command::new("java")
        .args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "Main"])
        .output()
        .expect("java");
    let out = String::from_utf8_lossy(&run.stdout);
    let err = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "java verify/run failed:\nstdout={out}\nstderr={err}");

    let expected = "7\n14\n3\n-5\n8\n11.0\n42!\nhi bob\n";
    assert_eq!(out, expected, "semantic mismatch; stderr={err}");

    let _ = fs::remove_dir_all(&dir);
}
