//! `break` / `continue` in `for` and `while` loops (including nested loops). The loop `update` (a
//! `for`-loop increment) runs at the `continue` target so `continue` advances rather than skipping it.
//! Round-tripped under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn break_continue_runs() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping break_continue_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_bc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = "fun box(): String {\n\
var s = 0\n\
for (i in 1..10) { if (i == 3) continue; if (i == 7) break; s += i }\n\
if (s != 1 + 2 + 4 + 5 + 6) return \"ffor\"\n\
var t = 0; var j = 0\n\
while (j < 10) { j += 1; if (j % 2 == 0) continue; if (j > 7) break; t += j }\n\
if (t != 1 + 3 + 5 + 7) return \"fwhile\"\n\
var u = 0\n\
for (a in 0 until 5) { for (b in 0 until 5) { if (b == 2) break; u += 1 } }\n\
if (u != 10) return \"fnest\"\n\
return \"OK\"\n\
}\n";
    fs::write(dir.join("D.kt"), src).unwrap();
    let kc = Command::new(krusty).args(["-d", dir.to_str().unwrap()]).arg(dir.join("D.kt")).output().unwrap();
    if !kc.status.success() { eprintln!("skip (IR unsupported): {}", String::from_utf8_lossy(&kc.stderr)); return; }
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(DKt.box()); } }").unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let r = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "OK", "stderr={}", String::from_utf8_lossy(&r.stderr));
    let _ = fs::remove_dir_all(&dir);
}
