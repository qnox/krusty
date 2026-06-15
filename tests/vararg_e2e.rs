//! `vararg` parameters: a `vararg xs: T` parameter has runtime type `Array<T>`; callers pack the
//! trailing arguments into a fresh array (the right element type), and the body sees `xs` as the
//! array. Works with a leading fixed parameter and with zero trailing args. Compiled by krusty and
//! run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::codegen::emit::emit_file;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn compile(src: &str, internal: &str) -> (Vec<u8>, Vec<String>) {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let (bytes, _) = emit_file(&files[0], &info, &syms, internal, &mut d);
    (bytes, d.diags.iter().map(|x| x.msg.clone()).collect())
}

const SRC: &str = r#"
fun sum(vararg xs: Int): Int {
    var s = 0
    for (x in xs) s += x
    return s
}
fun join(sep: String, vararg parts: String): String {
    var r = ""
    var i = 0
    for (p in parts) {
        if (i > 0) r += sep
        r += p
        i += 1
    }
    return r
}
fun box(): String {
    if (sum(1, 2, 3, 4) != 10) return "f1"
    if (sum() != 0) return "f2"
    if (join("-", "a", "b", "c") != "a-b-c") return "f3"
    if (join(",", "x") != "x") return "f4"
    if (join(",") != "") return "f5"
    return "OK"
}
"#;

#[test]
fn vararg_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping vararg_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let (bytes, errs) = compile(SRC, "VaKt");
    assert!(errs.is_empty(), "krusty errors: {errs:?}");
    let dir = std::env::temp_dir().join(format!("krusty_va_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("VaKt.class"), bytes).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(VaKt.box()); } }",
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
