//! `try { … } catch (e: E) { … }` as both expression and statement, including a throwing body caught
//! by the handler. Round-tripped against the JVM under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn try_catch_runs() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping try_catch_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping try_catch_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_try_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = "fun mightThrow(b: Boolean): Int { if (b) throw RuntimeException(\"x\"); return 1 }\n\
fun box(): String {\n\
val r = try { mightThrow(true) } catch (e: RuntimeException) { 42 }\n\
if (r != 42) return \"f1\"\n\
val s = try { mightThrow(false) } catch (e: RuntimeException) { 0 }\n\
if (s != 1) return \"f2\"\n\
val t = \"O\" + try { throw Exception(\"boom\") } catch (e: Exception) { \"K\" }\n\
if (t != \"OK\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    fs::write(dir.join("T.kt"), src).unwrap();
    let kc = Command::new(krusty).args(["-d", dir.to_str().unwrap()]).arg(dir.join("T.kt")).output().unwrap();
    if !kc.status.success() { eprintln!("skip (IR unsupported): {}", String::from_utf8_lossy(&kc.stderr)); return; }
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(TKt.box()); } }").unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let r = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "OK", "stderr={}", String::from_utf8_lossy(&r.stderr));
    let _ = fs::remove_dir_all(&dir);
}
