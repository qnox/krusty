//! `-Xuse-inline-scopes-numbers`: the shipping CLI's output under the argument against the
//! reference compiler's. AGP passes it to every application's debug compiles, so the command-line
//! option has to reach the backend; the box regressions in `tests/box_regressions/inlineScopes`
//! cover the numbering itself.

use std::path::Path;

use super::common;

/// A library call the bytecode inliner places, a scope function, a lambda inlined into another,
/// and a same-module inline function.
const SOURCE: &str = r#"
inline fun twice(f: (Int) -> Int): Int {
    val a = f(1)
    return f(a)
}

fun lengths(words: List<String>): Int = words.map { it.length }.filter { it > 1 }.sum()

fun scoped(x: Int): Int = x.let { a -> a.let { b -> b * 2 } }

fun nested(numbers: List<Int>): Int {
    var sum = 0
    numbers.forEach { n -> sum += n.let { it * 2 } }
    return sum
}

fun box(): String =
    if (lengths(listOf("a", "bb", "ccc")) == 5 && scoped(1) == 2 && nested(listOf(1, 2)) == 6 &&
        twice { it + 1 } == 3) "OK" else "fail"
"#;

const ARGUMENT: &str = "-Xuse-inline-scopes-numbers";

fn collect_classes(root: &Path, dir: &Path, classes: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_classes(root, &path, classes);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("class") {
            let name = path
                .strip_prefix(root)
                .expect("class below output root")
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            classes.push((name, std::fs::read(path).expect("read emitted class")));
        }
    }
}

fn output_classes(output: &Path) -> Vec<(String, Vec<u8>)> {
    let mut classes = Vec::new();
    collect_classes(output, output, &mut classes);
    classes.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(!classes.is_empty(), "the compiler emitted no classes");
    classes
}

fn compile_krusty(work: &Path, source: &Path) -> Vec<(String, Vec<u8>)> {
    let output = work.join("krusty");
    std::fs::create_dir_all(&output).expect("create krusty output");
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output"), ARGUMENT])
        .arg(source)
        .output()
        .expect("run krusty CLI");
    assert!(
        result.status.success(),
        "krusty failed under {ARGUMENT}: stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    output_classes(&output)
}

fn compile_reference(work: &Path, source: &Path) -> Vec<(String, Vec<u8>)> {
    let output = work.join("kotlinc");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    let args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        ARGUMENT.to_string(),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc failed under {ARGUMENT}: {stderr}");
    output_classes(&output)
}

#[test]
fn the_cli_numbers_inline_scopes_like_kotlinc() {
    let work = common::scratch_dir().expect("allocate inline scopes fixture");
    let source = work.join("InlineScopes.kt");
    std::fs::write(&source, SOURCE).expect("write inline scopes fixture");
    let krusty = compile_krusty(&work, &source);
    let reference = compile_reference(&work, &source);
    let _ = std::fs::remove_dir_all(work);
    let names = |classes: &[(String, Vec<u8>)]| {
        classes
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&krusty), names(&reference));
    for ((name, krusty), (_, reference)) in krusty.iter().zip(&reference) {
        assert!(
            krusty == reference,
            "{name}.class differs from kotlinc's under {ARGUMENT}"
        );
    }
}
