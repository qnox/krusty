//! `do … while` (post-test loop): the body always runs once, the condition is tested at the bottom.
//! `continue` jumps to that bottom test; `break` exits. Round-tripped under `-Xverify:all`.

mod common;

#[test]
fn do_while_runs() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping do_while_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping do_while_e2e: no kotlin-stdlib jar found");
        return;
    };
    const SRC: &str = "fun box(): String {\n\
var i = 0; var s = 0\n\
do { i += 1; s += i } while (i < 5)\n\
if (s != 15) return \"f1\"\n\
var n = 0\n\
do { n += 1; if (n == 3) continue; if (n > 6) break } while (n < 100)\n\
if (n != 7) return \"f2\"\n\
var only = 0\n\
do { only += 1 } while (false)\n\
if (only != 1) return \"f3\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "D", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
