//! Typed function variables: `val f: (Int) -> Int = { it * 2 }; f(3)`. `Ty::Fun` carries the real
//! parameter/return types, so the lambda body types `it`/`x` correctly and the call recovers the
//! return type (unboxed from the `FunctionN.invoke` `Object` result). Run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::jvm::emit::{emit_file, file_class_name};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn compile(src: &str, internal: &str) -> Vec<u8> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let (bytes, _) = emit_file(&files[0], &info, &syms, internal, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    bytes
}

const SRC: &str = r#"
fun apply(f: (Int) -> Int, x: Int): Int = f(x)
fun box(): String {
    val f: (Int) -> Int = { it * 2 }       // `it` typed Int from the annotation
    val g: (Int) -> Int = { x -> x + 1 }   // explicit param typed Int
    val s: (Int) -> String = { "v=$it" }   // reference-typed return
    if (f(3) != 6) return "f"              // call recovers Int (unboxed)
    if (g(4) != 5) return "g"
    if (apply(f, 10) != 20) return "hof"   // HOF arg
    if (s(7) != "v=7") return "s"
    return "OK"
}
"#;

#[test]
fn typed_function_variables_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping fun_type_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("krusty_funty_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // Compile via the binary so the synthesized lambda classes are emitted too.
    let src_path = dir.join("Funty.kt");
    fs::write(&src_path, SRC).unwrap();
    let _ = compile(SRC, &file_class_name("Funty", None)); // also exercises the in-process path
    let bin = env!("CARGO_BIN_EXE_krusty");
    let out = Command::new(bin).args(["-d", dir.to_str().unwrap()]).arg(&src_path).output().unwrap();
    assert!(out.status.success(), "krusty: {}", String::from_utf8_lossy(&out.stderr));
    let main = "public class M { public static void main(String[] a) { System.out.println(FuntyKt.box()); } }";
    fs::write(dir.join("M.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}
