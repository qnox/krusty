//! Smart-casting: after `if (x is T)` (or an early-return guard `if (x !is T) return …`), a stable
//! `x` (a `val` or parameter) is treated as `T` — member accesses on it resolve against `T` and the
//! codegen inserts a `checkcast`. Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
open class Animal
class Dog(val name: String) : Animal()
class Cat(val lives: Int) : Animal()

fun describe(a: Animal): String {
    if (a is Dog) return a.name
    if (a is Cat) return "cat" + a.lives
    return "other"
}

fun guard(a: Animal): String {
    if (a !is Dog) return "notdog"
    return a.name
}

fun box(): String {
    if (describe(Dog("rex")) != "rex") return "f1"
    if (describe(Cat(9)) != "cat9") return "f2"
    if (describe(Animal()) != "other") return "f3"
    if (guard(Dog("fido")) != "fido") return "f4"
    if (guard(Cat(1)) != "notdog") return "f5"
    return "OK"
}
"#;

#[test]
fn smartcast_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping smartcast_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_sc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ScKt.class"), emit_file(&files[0], &info, &syms, "ScKt", &mut d).0).unwrap();
    for name in ["Animal", "Dog", "Cat"] {
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
        "public class M { public static void main(String[] a) { System.out.println(ScKt.box()); } }",
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
fn var_is_not_smartcast() {
    // A `var` is not a stable value — accessing a `Dog`-only member on it after `is` must NOT resolve.
    let src = r#"
open class Animal
class Dog(val name: String) : Animal()
fun f(): String {
    var a: Animal = Dog("x")
    if (a is Dog) return a.name
    return "other"
}
"#;
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let _ = check_file(&files[0], &syms, &mut d);
    assert!(d.has_errors(), "expected unresolved-member error for var smart-cast");
}
