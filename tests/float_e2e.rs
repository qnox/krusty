//! `Float` (`F`): literals `1.5f`/`1f`, arithmetic (`fadd`/…), comparison (`fcmpg`), negation,
//! fields/params/returns, and string conversion. Numeric conversions `n.toInt()`/`toLong()`/
//! `toFloat()`/`toDouble()`. Also: elvis `?:` and `!!` on a non-null primitive are the operand
//! itself (no null check). Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
class V(val x: Float, val y: Float) {
    fun len2(): Float = x * x + y * y
}
fun box(): String {
    val a: Float = 2.5f
    if (a + 1.5f != 4.0f) return "f1"
    if (-a != -2.5f) return "f2"
    if (V(3.0f, 4.0f).len2() != 25.0f) return "f3"
    if (!(a < 4.0f)) return "f4"

    var sum = 0.0f
    for (i in 1..3) sum += i.toFloat()
    if (sum != 6.0f) return "f5"
    if ((3.7).toInt() != 3) return "f6"
    if (5.toDouble() != 5.0) return "f7"
    if (2.9f.toInt() != 2) return "f8"

    if ((42 ?: 239) != 42) return "f9"     // elvis on a non-null primitive
    val n = 7
    if (n!! != 7) return "f10"             // !! on a non-null primitive
    return "OK"
}
"#;

#[test]
fn float_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping float_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_float_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("FloatKt.class"), emit_file(&files[0], &info, &syms, "FloatKt", &mut d).0).unwrap();
    let v = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == "V" => Some(c.clone()),
            _ => None,
        })
        .expect("V decl");
    fs::write(dir.join("V.class"), emit_class(&v, &files[0], &info, "V", "V", &syms, &mut d).0).unwrap();
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(FloatKt.box()); } }",
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
