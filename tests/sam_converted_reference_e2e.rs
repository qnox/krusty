//! A function reference converted to a Java functional interface is the interface's own
//! implementation, as kotlinc's `FunctionReferenceLowering` builds it with the interface as its
//! super type: an `invokedynamic` of `LambdaMetafactory` by default, and under
//! `-Xsam-conversions=class` one class implementing the interface. No `FunctionReferenceImpl` value
//! exists for a wrapper to hold. krusty built the reference's `FunctionN` class and then wrapped it,
//! in the file's `$sam$` wrapper class or an `invokedynamic` over it.

use std::path::Path;

use super::common;

const SOURCE: &str = r#"
import java.util.function.IntUnaryOperator
import java.util.function.Supplier

class Counter(var count: Int = 0) {
    fun bump() { count++ }
    fun plus(n: Int): Int = count + n
}

var calls = 0
fun top() { calls++ }
fun twice(n: Int): Int = n * 2
fun scaled(n: Int, by: Int = 3): Int = n * by
fun made(): Counter = Counter(5)
class Fresh { val count = 0 }

fun runIt(r: Runnable) = r.run()
fun applyIt(f: IntUnaryOperator, n: Int): Int = f.applyAsInt(n)
fun <T> get(s: Supplier<T>): T = s.get()

fun box(): String {
    runIt(::top)
    val counter = Counter()
    runIt(counter::bump)
    runIt(Counter(7)::bump)
    if (calls != 1 || counter.count != 1) return "fail run"
    if (applyIt(::twice, 4) != 8) return "fail unbound"
    if (applyIt(counter::plus, 4) != 5) return "fail bound"
    if (applyIt(::scaled, 4) != 12) return "fail adapted"
    if (get(::made).count != 5) return "fail function"
    if (get(::Fresh).count != 0) return "fail constructor"
    return "OK"
}
"#;

fn collect_classes(root: &Path, dir: &Path, classes: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_classes(root, &path, classes);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("class") {
            classes.push(
                path.strip_prefix(root)
                    .expect("class below output root")
                    .with_extension("")
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        }
    }
}

fn krusty_classes(sam_conversions: &str) -> Vec<String> {
    let work = common::scratch_dir().expect("allocate krusty fixture");
    let source = work.join("Main.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create krusty output");
    std::fs::write(&source, SOURCE).expect("write krusty fixture");
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output"), "-no-reflect"])
        .arg(format!("-Xsam-conversions={sam_conversions}"))
        .arg(&source)
        .output()
        .expect("run krusty CLI");
    assert!(
        result.status.success(),
        "krusty rejected the fixture: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort();
    let _ = std::fs::remove_dir_all(work);
    classes
}

fn kotlinc_classes(sam_conversions: &str) -> Vec<String> {
    let work = common::scratch_dir().expect("allocate kotlinc fixture");
    let source = work.join("Main.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    std::fs::write(&source, SOURCE).expect("write kotlinc fixture");
    let args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        format!("-Xsam-conversions={sam_conversions}"),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort();
    let _ = std::fs::remove_dir_all(work);
    classes
}

#[test]
fn references_to_java_interfaces_emit_kotlincs_classes() {
    for mode in ["indy", "class"] {
        assert_eq!(
            krusty_classes(mode),
            kotlinc_classes(mode),
            "-Xsam-conversions={mode}"
        );
    }
}

#[test]
fn references_to_java_interfaces_run_like_kotlinc() {
    for mode in ["indy", "class"] {
        common::expect_box_same_as_kotlinc_with_args(
            SOURCE,
            &format!("SamConvertedReference_{mode}"),
            &[&format!("-Xsam-conversions={mode}")],
        );
    }
}
