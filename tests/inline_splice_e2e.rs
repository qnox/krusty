//! Cross-module bytecode inliner (inliner #2): a *branchless* `inline fun` compiled by the real
//! `kotlinc` into a separate library is **spliced** into the caller by krusty — no `invokestatic` to
//! the library function survives, and the result is correct under the JVM verifier. Proves the
//! `Emitter::try_inline_static` → `inline::splice_branchless` path end-to-end.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn branchless_inline_fn_is_spliced_not_called() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping inline_splice_e2e: set KRUSTY_KOTLINC");
        return;
    };
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping inline_splice_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping inline_splice_e2e: no kotlin-stdlib jar");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    let jdk_modules = format!("{java_home}/lib/modules");
    let krusty = env!("CARGO_BIN_EXE_krusty");

    let work = std::env::temp_dir().join(format!("krusty_inline_splice_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("libout");
    let mainout = work.join("mainout");
    let runner = work.join("runner");
    for d in [&libout, &mainout, &runner] {
        fs::create_dir_all(d).unwrap();
    }

    // 1. A library with a branchless `inline fun`, compiled by the *real* kotlinc.
    let lib_kt = work.join("Lib.kt");
    fs::write(&lib_kt, "package lib\ninline fun triple(x: Int): Int = x * 3\n").unwrap();
    let kc = Command::new(&kotlinc)
        .args(["-d", libout.to_str().unwrap(), "-cp", &stdlib])
        .arg(&lib_kt)
        .output()
        .unwrap();
    assert!(kc.status.success(), "kotlinc(lib): {}", String::from_utf8_lossy(&kc.stderr));

    // 2. A caller that uses the inline fn, compiled by krusty with the lib on its classpath.
    let main_kt = work.join("Main.kt");
    fs::write(&main_kt, "import lib.triple\nfun box(): String = if (triple(7) == 21) \"OK\" else \"fail:${triple(7)}\"\n").unwrap();
    let compile_cp = format!("{libout}:{stdlib}:{jdk_modules}", libout = libout.to_str().unwrap());
    let kr = Command::new(krusty)
        .args(["-cp", &compile_cp, "-d", mainout.to_str().unwrap()])
        .arg(&main_kt)
        .output()
        .unwrap();
    assert!(kr.status.success(), "krusty(main): {}", String::from_utf8_lossy(&kr.stderr));

    // 3. The inline fn was *spliced*, not called: no reference to `triple` survives in MainKt.
    let main_class = fs::read(mainout.join("MainKt.class")).unwrap();
    assert!(
        !contains(&main_class, b"triple"),
        "MainKt still references `triple` — the inline fn was called, not spliced"
    );

    // 4. The spliced bytecode verifies and computes the right result.
    let runner_src = r#"import java.io.File; import java.net.URL; import java.net.URLClassLoader;
public class BoxRun {
  public static void main(String[] a) throws Exception {
    URLClassLoader cl = new URLClassLoader(new URL[]{ new File(a[0]).toURI().toURL() }, BoxRun.class.getClassLoader());
    System.out.println(Class.forName("MainKt", true, cl).getMethod("box").invoke(null));
  }
}"#;
    fs::write(runner.join("BoxRun.java"), runner_src).unwrap();
    let jc = Command::new(&javac).args(["-d", runner.to_str().unwrap()]).arg(runner.join("BoxRun.java")).output().unwrap();
    assert!(jc.status.success(), "javac(BoxRun): {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java)
        .args(["-Xverify:all", "-cp"])
        .arg(format!("{}:{stdlib}", runner.to_str().unwrap()))
        .args(["BoxRun", mainout.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(run.status.success(), "BoxRun: {}", String::from_utf8_lossy(&run.stderr));
    let out = String::from_utf8_lossy(&run.stdout);
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");

    let _ = fs::remove_dir_all(&work);
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}
