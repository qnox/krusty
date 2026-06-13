//! Generics via type erasure: type parameters (`class Box<T>`, `fun <T> id(x: T): T`) are parsed and
//! every type-parameter reference is erased to `java/lang/Object`, matching the JVM bytecode kotlinc
//! emits. Generic *declarations* and *inferred* generic calls compile and run; usages that would need
//! a synthetic `checkcast`/box (an erased value flowing into a more specific type) are rejected, not
//! miscompiled. We also reject overloads that collide after erasure (kotlinc erases to the bound).

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
class Holder<T>(val value: T)

fun <T> first(a: T, b: T): T = a

fun box(): String {
    val h = Holder("OK")
    val v = h.value
    if (v != "OK") return "f1"
    val w = first("OK", "no")
    if (w != "OK") return "f2"
    return "OK"
}
"#;

#[test]
fn generics_via_erasure_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping generics_e2e: set JAVA_HOME");
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

    let facade = emit_file(&files[0], &info, &syms, "GKt", &mut d);
    let holder = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == "Holder" => Some(c.clone()),
            _ => None,
        })
        .expect("Holder decl");
    let holder_bytes = emit_class(&holder, &files[0], &info, "Holder", &syms, &mut d);
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    let dir = std::env::temp_dir().join(format!("krusty_gen_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("GKt.class"), facade).unwrap();
    fs::write(dir.join("Holder.class"), holder_bytes).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(GKt.box()); } }",
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

    // The erased generic parameter becomes Object in the ABI.
    let jp = Command::new("javap").args(["-p", "-cp", dir.to_str().unwrap(), "Holder"]).output().unwrap();
    let abi = String::from_utf8_lossy(&jp.stdout);
    assert!(abi.contains("java.lang.Object getValue()"), "expected erased getter, got:\n{abi}");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn overloads_clashing_after_erasure_are_rejected() {
    // `<T> f(T)` and `<T> f(T)` both erase to `f(Ljava/lang/Object;)` — kotlinc keeps them distinct by
    // erasing to each bound, which krusty does not model, so it rejects rather than emit a duplicate.
    let src = "fun <T> f(x: T): String = \"a\"\nfun <U> f(x: U): String = \"b\"\n";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let _ = check_file(&files[0], &syms, &mut d);
    assert!(d.has_errors(), "expected an erased-overload-clash error");
    assert!(
        d.diags.iter().any(|x| x.msg.contains("erasure")),
        "expected erasure clash message, got: {:?}",
        d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
    );
}
