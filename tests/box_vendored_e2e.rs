//! In-repo conformance: real Kotlin `codegen/box` cases (vendored under `tests/box_data/`) that fall
//! within krusty's supported subset. Each is compiled by the `krusty` binary and its `box(): String`
//! is run on a real JVM; it must return `"OK"`. These run in normal `cargo test` (given a JDK),
//! unlike the full external sweep in `kotlin_box_conformance.rs`.
//!
//! Provenance: copied verbatim from JetBrains/kotlin `compiler/testData/codegen/box/` (Apache-2.0).

use std::fs;
use std::path::Path;
use std::process::Command;

use krusty::jvm::classreader::parse_class;

mod common;

fn find_box_class(dir: &Path) -> Option<String> {
    let mut found = None;
    fn walk(dir: &Path, found: &mut Option<String>) {
        if found.is_some() {
            return;
        }
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, found);
                } else if p.extension().map_or(false, |x| x == "class") {
                    if let Ok(ci) = parse_class(&fs::read(&p).unwrap_or_default()) {
                        if ci.method("box", "()Ljava/lang/String;").map_or(false, |m| m.is_static()) {
                            *found = Some(ci.this_class.replace('/', "."));
                            return;
                        }
                    }
                }
            }
        }
    }
    walk(dir, &mut found);
    found
}

#[test]
fn vendored_kotlin_box_cases_return_ok() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping vendored box cases: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !Path::new(&javac).exists() {
        return;
    }
    // Kotlin-conforming output references `kotlin/jvm/internal/Intrinsics` (areEqual,
    // checkNotNullParameter, …), so kotlin-stdlib must be on the runtime classpath.
    let stdlib = common::stdlib_jar().map(|p| p.to_string_lossy().into_owned());
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/box_data");
    let work = std::env::temp_dir().join(format!("krusty_vbox_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);

    let mut cases: Vec<_> = fs::read_dir(&data).unwrap().filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |x| x == "kt")).collect();
    cases.sort();
    assert!(!cases.is_empty(), "no vendored box cases found");

    fs::create_dir_all(&work).unwrap();
    // Compile a single reflective runner ONCE (instead of a `javac`+`java` per case): it loads each
    // accepted case's classes through a per-case `URLClassLoader` and invokes its static `box()`,
    // so all cases run in ONE JVM under `-Xverify:all` (every emitted class is still verified on
    // load). This collapses dozens of subprocess spawns — the dominant cost — into one.
    let runner = work.join("runner");
    fs::create_dir_all(&runner).unwrap();
    let runner_src = r#"import java.io.File; import java.net.URL; import java.net.URLClassLoader;
public class BoxRun {
  public static void main(String[] args) throws Exception {
    for (int i = 0; i + 1 < args.length; i += 2) {
      String result;
      try {
        URLClassLoader cl = new URLClassLoader(new URL[]{ new File(args[i]).toURI().toURL() }, BoxRun.class.getClassLoader());
        Object r = Class.forName(args[i+1], true, cl).getMethod("box").invoke(null);
        result = String.valueOf(r);
      } catch (Throwable t) { result = "EXC:" + t; }
      System.out.println(args[i+1] + "\t" + result);
    }
  }
}
"#;
    fs::write(runner.join("BoxRun.java"), runner_src).unwrap();
    let jc = Command::new(&javac).args(["-d", runner.to_str().unwrap()]).arg(runner.join("BoxRun.java")).output().unwrap();
    assert!(jc.status.success(), "javac(BoxRun) failed: {}", String::from_utf8_lossy(&jc.stderr));

    // Compile every case with krusty; collect (output dir, box class) for the accepted ones.
    let mut skipped = 0usize;
    let mut accepted: Vec<(String, String, &Path)> = Vec::new();
    for (i, kt) in cases.iter().enumerate() {
        let out = work.join(format!("o{i}"));
        fs::create_dir_all(&out).unwrap();
        let kc = Command::new(krusty).args(["-d", out.to_str().unwrap()]).arg(kt).output().expect("krusty");
        // The IR backend covers a subset; a case it rejects is *skipped*, never a failure. The gate
        // is: every case krusty *accepts* must run and return "OK" (never miscompile an accepted file).
        if !kc.status.success() {
            skipped += 1;
            continue;
        }
        let box_class = find_box_class(&out).unwrap_or_else(|| panic!("no box() class for {}", kt.display()));
        accepted.push((out.to_str().unwrap().to_string(), box_class, kt.as_path()));
    }

    // Run all accepted cases in a single JVM: classpath is the runner + stdlib (the parent loader,
    // so `Intrinsics` resolves); each case's own classes load via its `URLClassLoader` argument.
    let mut cp = runner.to_str().unwrap().to_string();
    if let Some(s) = &stdlib { cp.push(':'); cp.push_str(s); }
    let mut args: Vec<String> = vec!["-Xverify:all".into(), "-cp".into(), cp, "BoxRun".into()];
    for (dir, class, _) in &accepted {
        args.push(dir.clone());
        args.push(class.clone());
    }
    let run = Command::new(&java).args(&args).output().unwrap();
    assert!(run.status.success(), "BoxRun failed: {}", String::from_utf8_lossy(&run.stderr));
    let stdout = String::from_utf8_lossy(&run.stdout);
    let results: std::collections::HashMap<&str, &str> = stdout.lines()
        .filter_map(|l| l.split_once('\t')).collect();
    for (_, class, kt) in &accepted {
        let got = results.get(class.as_str()).copied().unwrap_or("<missing>");
        assert!(got == "OK", "box() did not return OK for {}: got {:?}", kt.display(), got);
    }
    let _ = fs::remove_dir_all(&work);
    eprintln!("vendored Kotlin box conformance (IR backend): {} OK, {skipped} skipped (unsupported), {} total", accepted.len(), cases.len());
}
