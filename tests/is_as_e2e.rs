//! Type tests and casts: `e is T` / `e !is T` lower to `instanceof` (Boolean), `e as T` to
//! `checkcast`, and `e as? T` to an `instanceof`-guarded cast (null on mismatch). Compiled by krusty
//! and run on a real JVM. Targets and operands must be *known reference types* — primitive or
//! unresolved targets, and nullable `is T?`, are rejected (cleanly skipped) rather than miscompiled.

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
class Dog : Animal()
class Cat : Animal()

fun describe(a: Animal): String {
    if (a is Dog) return "dog"
    if (a !is Cat) return "other"
    val c = a as Cat
    return "cat"
}

fun box(): String {
    val d: Animal = Dog()
    if (describe(d) != "dog") return "f1"
    if (describe(Cat()) != "cat") return "f2"
    if (describe(Animal()) != "other") return "f3"

    val x: Any = "hello"
    val s = x as String
    if (s != "hello") return "f4"

    val maybe = x as? String
    if (maybe == null) return "f5"

    val notStr: Any = Dog()
    if ((notStr as? String) != null) return "f6"

    return "OK"
}
"#;

#[test]
fn is_as_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping is_as_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_isas_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("IsAsKt.class"), emit_file(&files[0], &info, &syms, "IsAsKt", &mut d).0).unwrap();
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
        "public class M { public static void main(String[] a) { System.out.println(IsAsKt.box()); } }",
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

fn rejects(src: &str) -> bool {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let _ = check_file(&files[0], &syms, &mut d);
    d.has_errors()
}

#[test]
fn unsafe_is_as_are_rejected() {
    // Unresolved target → would erase to Object (instanceof always true): reject.
    assert!(rejects("fun box(): String { val x: Any = \"\"\n if (x is Number) return \"a\"\n return \"b\" }"));
    // Nullable `is T?` → `null is T?` is true but instanceof is false: reject.
    assert!(rejects("fun box(): String { val x: Any = \"\"\n if (x is String?) return \"a\"\n return \"b\" }"));
    // Primitive `is`/`as` would need boxing: reject.
    assert!(rejects("fun box(): String { val x: Any = \"\"\n val n = x as Int\n return \"b\" }"));
}
