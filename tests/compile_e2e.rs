//! End-to-end through the full krusty pipeline: source → lex → parse → typecheck → emit class,
//! then JVM-verify + run via a Java `Main`, comparing results to the expected Kotlin semantics.

use std::fs;
use std::process::Command;

use krusty::jvm::emit::emit_file;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

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
    let (bytes, _) = emit_file(&files[0], &info, &syms, internal_name, &mut diags);
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
        Err(errs) => panic!("krusty compile errors: {errs:?}"),
    };

    let dir = std::env::temp_dir().join(format!("krusty_pipe_{}", std::process::id()));
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
    assert!(javac.status.success(), "javac rejected krusty output:\n{}", String::from_utf8_lossy(&javac.stderr));

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

#[test]
fn control_flow_pipeline() {
    if !have("javac") || !have("java") {
        eprintln!("skipping: javac/java unavailable");
        return;
    }

    let src = r#"
        fun max(a: Int, b: Int): Int = if (a > b) a else b
        fun absdiff(a: Int, b: Int): Int = if (a > b) a - b else b - a
        fun both(a: Int, b: Int): Boolean = a > 0 && b > 0
        fun either(a: Int, b: Int): Boolean = a > 0 || b > 0
        fun classify(n: Int): String = if (n > 0) "pos" else "nonpos"
        fun fib(n: Int): Int {
            var a = 0
            var b = 1
            var i = 0
            while (i < n) {
                val t = a + b
                a = b
                b = t
                i = i + 1
            }
            return a
        }
    "#;

    let bytes = match compile(src, "CtrlKt") {
        Ok(b) => b,
        Err(errs) => panic!("krusty compile errors: {errs:?}"),
    };

    let dir = std::env::temp_dir().join(format!("krusty_ctrl_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("CtrlKt.class"), &bytes).unwrap();

    let main = r#"
        public class Main {
            public static void main(String[] x) {
                System.out.println(CtrlKt.max(3, 7));
                System.out.println(CtrlKt.max(9, 2));
                System.out.println(CtrlKt.absdiff(3, 7));
                System.out.println(CtrlKt.both(1, 1));
                System.out.println(CtrlKt.both(1, -1));
                System.out.println(CtrlKt.either(-1, 2));
                System.out.println(CtrlKt.either(-1, -1));
                System.out.println(CtrlKt.classify(5));
                System.out.println(CtrlKt.classify(-1));
                System.out.println(CtrlKt.fib(10));
            }
        }
    "#;
    fs::write(dir.join("Main.java"), main).unwrap();

    let javac = Command::new("javac")
        .args(["-cp", dir.to_str().unwrap(), "Main.java"])
        .current_dir(&dir)
        .output()
        .expect("javac");
    assert!(javac.status.success(), "javac rejected krusty output:\n{}", String::from_utf8_lossy(&javac.stderr));

    let run = Command::new("java")
        .args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "Main"])
        .output()
        .expect("java");
    let out = String::from_utf8_lossy(&run.stdout);
    let err = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "java verify/run failed:\nstdout={out}\nstderr={err}");

    let expected = "7\n9\n4\ntrue\nfalse\ntrue\nfalse\npos\nnonpos\n55\n";
    assert_eq!(out, expected, "control-flow semantic mismatch; stderr={err}");

    let _ = fs::remove_dir_all(&dir);
}
