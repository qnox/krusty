//! `(val a, val b) = expression` without `NameBasedDestructuring` is the language-version
//! diagnostic kotlinc reports on each `val`. It is not a parse error: a parse error drops the
//! file's signatures, and capture discovery then panics with "checked class has a collected
//! signature" on the file's anonymous object.

use super::common;

const SOURCE: &str = "\
data class Pair2(val a: Int, val b: Int)\n\
interface Marker\n\
fun select() {\n\
(val a, val b) = Pair2(1, 2)\n\
object : Marker {}\n\
}\n\
";

#[test]
fn full_form_destructure_reports_the_language_version() {
    let mut diagnostics = krusty::diag::DiagSink::new();
    let (_file, symbols, info) =
        krusty::frontend::analyze_source_standalone(SOURCE, &mut diagnostics);
    assert!(symbols.is_some());
    assert!(info.is_some());
    let rendered = diagnostics.render("Full.kt", SOURCE);
    assert_eq!(
        rendered,
        "\
Full.kt:4:2: error: the feature \"name based destructuring\" is only available since language version 2.5
Full.kt:4:9: error: the feature \"name based destructuring\" is only available since language version 2.5
"
    );

    let root = common::scratch_dir().expect("scratch dir");
    let dir = root.join("full_form_destructure");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("Full.kt");
    std::fs::write(&source, SOURCE).unwrap();
    let out = dir.join("classes");
    std::fs::create_dir_all(&out).unwrap();
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("provisioned kotlinc");
    assert_ne!(code, 0, "{stderr}");
    let errors = stderr
        .lines()
        .filter(|line| line.contains(": error:"))
        .map(|line| line.split_once(':').map(|(_, rest)| rest).unwrap_or(line))
        .collect::<Vec<_>>();
    assert_eq!(
        errors,
        vec![
            "4:2: error: the feature \"name based destructuring\" is only available since language version 2.5",
            "4:9: error: the feature \"name based destructuring\" is only available since language version 2.5",
        ]
    );
}
