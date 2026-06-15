//! (v0) `interface` declarations + classes implementing them: the interface becomes a JVM
//! `public interface` with abstract methods; an implementing class lists it in `implements` and
//! provides the methods. Concrete-type dispatch (`Square(3).area()`) runs on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::emit_class;
use krusty::diag::DiagSink;
use krusty::jvm::classreader::parse_class;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn interface_and_impl_shapes() {
    let src = "interface Shape { fun area(): Int }\nclass Square(val side: Int) : Shape { fun area(): Int = side * side }";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let get = |name: &str, internal: &str| {
        let cd = files[0].decls.iter().find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == name => Some(c.clone()),
            _ => None,
        }).unwrap();
        parse_class(&emit_class(&cd, &files[0], &info, internal, internal, &syms, &mut DiagSink::new()).0).unwrap()
    };
    let iface = get("Shape", "Shape");
    assert!(iface.major != 0);
    assert!(iface.method("area", "()I").is_some(), "interface abstract method");
    let sq = get("Square", "Square");
    assert!(sq.method("area", "()I").is_some(), "impl method");
    assert!(!d.has_errors(), "errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
}

#[test]
fn interface_polymorphism_runs() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping interface polymorphism: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_poly_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // An interface-typed value + a function taking the interface → invokeinterface + subtyping.
    fs::write(dir.join("P.kt"),
        "interface Shape { fun area(): Int }\nclass Square(val side: Int) : Shape { fun area(): Int = side * side }\nclass Rect(val w: Int, val h: Int) : Shape { fun area(): Int = w * h }\nfun describe(s: Shape): Int = s.area()\nfun box(): String {\n  val s: Shape = Square(3)\n  if (s.area() != 9) return \"f1\"\n  if (describe(Rect(2, 5)) != 10) return \"f2\"\n  return \"OK\"\n}\n").unwrap();
    let kc = Command::new(krusty).args(["-d", dir.to_str().unwrap()]).arg(dir.join("P.kt")).output().unwrap();
    assert!(kc.status.success(), "krusty: {}", String::from_utf8_lossy(&kc.stderr));
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(PKt.box()); } }").unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK", "stderr={}", String::from_utf8_lossy(&run.stderr));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn interface_impl_runs() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping interface_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_if_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("S.kt"),
        "interface Shape { fun area(): Int }\nclass Square(val side: Int) : Shape { fun area(): Int = side * side }\nfun box(): String = if (Square(4).area() == 16) \"OK\" else \"fail\"\n").unwrap();
    let kc = Command::new(krusty).args(["-d", dir.to_str().unwrap()]).arg(dir.join("S.kt")).output().unwrap();
    assert!(kc.status.success(), "krusty: {}", String::from_utf8_lossy(&kc.stderr));
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(SKt.box()); } }").unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK", "stderr={}", String::from_utf8_lossy(&run.stderr));
    let _ = fs::remove_dir_all(&dir);
}
