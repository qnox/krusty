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
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/box_data");
    let work = std::env::temp_dir().join(format!("krusty_vbox_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);

    let mut cases: Vec<_> = fs::read_dir(&data).unwrap().filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |x| x == "kt")).collect();
    cases.sort();
    assert!(!cases.is_empty(), "no vendored box cases found");

    let mut ok = 0usize;
    let mut skipped = 0usize;
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
        let main = format!("public class M {{ public static void main(String[] a) {{ System.out.println({box_class}.box()); }} }}");
        fs::write(out.join("M.java"), main).unwrap();
        let jc = Command::new(&javac).args(["-cp", out.to_str().unwrap(), "-d", out.to_str().unwrap()]).arg(out.join("M.java")).output().unwrap();
        assert!(jc.status.success(), "javac(Main) failed for {}: {}", kt.display(), String::from_utf8_lossy(&jc.stderr));
        let run = Command::new(&java).args(["-Xverify:all", "-cp", out.to_str().unwrap(), "M"]).output().unwrap();
        let returned = String::from_utf8_lossy(&run.stdout).lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("").trim().to_string();
        assert!(run.status.success() && returned == "OK",
            "box() did not return OK for {}: got {:?}, stderr={}", kt.display(), returned, String::from_utf8_lossy(&run.stderr));
        ok += 1;
    }
    let _ = fs::remove_dir_all(&work);
    eprintln!("vendored Kotlin box conformance (IR backend): {ok} OK, {skipped} skipped (unsupported), {} total", cases.len());
}
