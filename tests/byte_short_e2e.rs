//! `Byte` (`B`) and `Short` (`S`): int on the JVM stack, with `i2b`/`i2s` truncation on narrowing.
//! Integer literals assign to Byte/Short; arithmetic promotes to Int; `.toByte()`/`.toShort()`
//! truncate; they flow through fields, params, comparison, string conversion, and data-class
//! equals/hashCode (also covering a `Char` data-class field). Compiled by krusty, run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
class Holder(val b: Byte, val s: Short)
data class DC(val b: Byte, val s: Short, val c: Char)

fun add(x: Byte, y: Byte): Int = x + y

fun box(): String {
    val b: Byte = 1
    val s: Short = 300
    if (add(b, 4) != 5) return "f1"
    val h = Holder(7, 1000)
    if (h.b.toInt() != 7 || h.s.toInt() != 1000) return "f2"
    if (130.toByte().toInt() != -126) return "f3"
    if (40000.toShort().toInt() != -25536) return "f4"
    if (!(b < s)) return "f5"
    if ("$b $s" != "1 300") return "f6"
    if (DC(1, 2, 'x') != DC(1, 2, 'x')) return "f7"
    if (DC(1, 2, 'x') == DC(1, 2, 'y')) return "f8"
    return "OK"
}
"#;

#[test]
fn byte_short_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping byte_short_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_bs_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("BsKt.class"), emit_file(&files[0], &info, &syms, "BsKt", &mut d).0).unwrap();
    for name in ["Holder", "DC"] {
        let cd = files[0]
            .decls
            .iter()
            .find_map(|&id| match files[0].decl(id) {
                Decl::Class(c) if c.name == name => Some(c.clone()),
                _ => None,
            })
            .expect("class decl");
        fs::write(dir.join(format!("{name}.class")), emit_class(&cd, &files[0], &info, name, name, &syms, &mut d).0).unwrap();
    }
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(BsKt.box()); } }",
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
