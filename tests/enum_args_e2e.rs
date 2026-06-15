//! Enum classes with a primary constructor + per-entry arguments + member methods, and hex/binary/
//! underscore integer literals. Each entry is `new C("NAME", ordinal, args)` in `<clinit>`; property
//! params become fields + getters. Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::{Decl, Expr};
use krusty::codegen::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
enum class Planet(val mass: Int, val label: String) {
    EARTH(597, "earth"),
    MARS(64, "mars"),
    JUPITER(1898, "jupiter");
    fun describe(): String = label + "=" + mass
    fun heavy(): Boolean = mass > 100
}
fun box(): String {
    if (Planet.EARTH.mass != 597) return "f1"
    if (Planet.MARS.label != "mars") return "f2"
    if (Planet.JUPITER.describe() != "jupiter=1898") return "f3"
    if (Planet.EARTH.name != "EARTH") return "f4"
    if (Planet.MARS.ordinal != 1) return "f5"
    if (!Planet.JUPITER.heavy() || Planet.MARS.heavy()) return "f6"
    if (0xFF != 255 || 0b1010 != 10 || 1_000 != 1000 || 0x10L != 16L) return "f7"
    return "OK"
}
"#;

#[test]
fn enum_args_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping enum_args_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_en_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("EnKt.class"), emit_file(&files[0], &info, &syms, "EnKt", &mut d).0).unwrap();
    let pl = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == "Planet" => Some(c.clone()),
            _ => None,
        })
        .expect("Planet decl");
    fs::write(dir.join("Planet.class"), emit_class(&pl, &files[0], &info, "Planet", "Planet", &syms, &mut d).0).unwrap();
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(EnKt.box()); } }",
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

#[test]
fn hex_binary_underscore_literals_parse() {
    let src = "fun f(): Int = 0xFF\nfun g(): Int = 0b1010\nfun h(): Int = 1_000";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    assert!(!d.has_errors(), "parse errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    let ints: Vec<i64> = file.expr_arena.iter().filter_map(|e| if let Expr::IntLit(v) = e { Some(*v) } else { None }).collect();
    assert_eq!(ints, vec![255, 10, 1000]);
}
