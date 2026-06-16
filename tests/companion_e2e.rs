//! `companion object` members are emitted as `static`/`static final` members of the enclosing class:
//! `ClassName.fn(...)` → `invokestatic`, `ClassName.PROP` → `getstatic`, and the members are also
//! reachable unqualified inside other companion members. Compiled by krusty and run on a real JVM.
//! (Companion members colliding with an instance member, or touching a top-level property, are
//! rejected — krusty puts statics on the same class rather than a nested `Companion`.)

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::jvm::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
class Registry(val name: String) {
    companion object {
        const val MAX = 100
        val DEFAULT = "none"
        fun create(n: String): Registry = Registry(n)
        fun limit(): Int = MAX
        fun fallback(): String = DEFAULT
    }
}
fun box(): String {
    if (Registry.MAX != 100) return "f1"
    if (Registry.DEFAULT != "none") return "f2"
    if (Registry.create("db").name != "db") return "f3"
    if (Registry.limit() != 100) return "f4"
    if (Registry.fallback() != "none") return "f5"
    return "OK"
}
"#;

#[test]
fn companion_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping companion_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_comp_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("CompKt.class"), emit_file(&files[0], &info, &syms, "CompKt", &mut d).0).unwrap();
    let reg = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == "Registry" => Some(c.clone()),
            _ => None,
        })
        .expect("Registry decl");
    fs::write(dir.join("Registry.class"), emit_class(&reg, &files[0], &info, "Registry", "Registry", &syms, &mut d).0).unwrap();
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(CompKt.box()); } }",
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
fn companion_instance_collision_is_rejected() {
    let src = "class C { val x = \"a\"\n companion object { val x = \"b\" } }";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let _ = check_file(&files[0], &syms, &mut d);
    assert!(d.diags.iter().any(|x| x.msg.contains("collides")), "expected collision rejection");
}
