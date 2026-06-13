//! Class inheritance: `open class Base(args)` + `class Sub(...) : Base(args)` — the subclass
//! `extends` the base, its constructor calls `super(args)`, and it inherits the base's methods and
//! properties (open members are non-`final`). Compiled by krusty, run on a real JVM.

use std::fs;
use std::process::Command;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn subclass_inherits_and_overrides() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping inheritance_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_inh_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("H.kt"),
        "open class Animal(val name: String) {\n  fun describe(): String = \"animal:\" + name\n}\nclass Dog(val tag: Int) : Animal(\"rex\") {\n  fun bark(): String = \"woof\"\n}\nfun box(): String {\n  val d = Dog(7)\n  if (d.bark() != \"woof\") return \"f1\"\n  if (d.describe() != \"animal:rex\") return \"f2\"\n  if (d.name != \"rex\") return \"f3\"\n  if (d.tag != 7) return \"f4\"\n  return \"OK\"\n}\n").unwrap();
    let kc = Command::new(krusty).args(["-d", dir.to_str().unwrap()]).arg(dir.join("H.kt")).output().unwrap();
    assert!(kc.status.success(), "krusty: {}", String::from_utf8_lossy(&kc.stderr));
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(HKt.box()); } }").unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK", "stderr={}", String::from_utf8_lossy(&run.stderr));
    let _ = fs::remove_dir_all(&dir);
}
