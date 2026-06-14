//! Named arguments on top-level function calls (`f(b = 2, a = 5)`): mapped onto positional
//! parameter slots, combinable with omitted defaults, and evaluated in source order (supplied
//! arguments are spilled to locals left-to-right, then loaded in parameter order). Named arguments
//! on methods/constructors are rejected (covered by a checker unit test, not here). Compiled by
//! krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::codegen::emit::emit_file;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
fun sub(a: Int, b: Int): Int = a - b
fun mix(a: Int, b: Int = 10, c: Int = 100): Int = a * 100 + b * 10 + c
fun rec(log: IntArray, v: Int): Int { log[log[0] + 1] = v; log[0] = log[0] + 1; return v }
fun box(): String {
    if (sub(a = 5, b = 2) != 3) return "f1"   // named, in order
    if (sub(b = 2, a = 5) != 3) return "f2"   // named, reordered
    if (mix(1, c = 3) != 203) return "f3"     // positional + named, omit a middle default
    // reordered named args evaluate in source order (b before a):
    val log = IntArray(3)
    if (sub(b = rec(log, 2), a = rec(log, 5)) != 3) return "f4"
    if (log[1] != 2 || log[2] != 5) return "f5"
    return "OK"
}
"#;

#[test]
fn named_args_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping named_args_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }

    let mut d = DiagSink::new();
    let toks = lex(SRC, &mut d);
    let file = parse(SRC, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    let bytes = emit_file(&files[0], &info, &syms, "NaKt", &mut d);
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    let dir = std::env::temp_dir().join(format!("krusty_na_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("NaKt.class"), bytes).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(NaKt.box()); } }",
    )
    .unwrap();
    let jc = Command::new(&javac)
        .args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}
