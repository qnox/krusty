//! `-Xmulti-platform`, `-Xfragments` and `-Xfragment-sources` as a build tool passes them for a
//! JVM-only module: one fragment holding every source. kotlinc's output is then the plain
//! compilation's, and a source no fragment lists is a configuration error in kotlinc's words.

use std::path::{Path, PathBuf};

use super::common;

const FIRST: &str =
    "package p\nclass A(val x: Int) { fun f() = x + 1 }\nfun top(): String = \"a\"\n";
const SECOND: &str = "package p\nfun useA() = A(1).f()\n";

fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("compiler output entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let name = path
                    .strip_prefix(dir)
                    .expect("file below the output root")
                    .to_string_lossy()
                    .into_owned();
                found.push((name, std::fs::read(&path).expect("read output file")));
            }
        }
    }
    found.sort();
    found
}

/// Both compilers over `First.kt Second.kt` with `fragment_arguments`: kotlinc's exit code and
/// stderr, krusty's, and each one's output files. krusty's stdout is its own progress line.
struct Compiled {
    kotlinc: (i32, String, Vec<(String, Vec<u8>)>),
    krusty: (i32, String, Vec<(String, Vec<u8>)>),
}

fn compile(fragment_arguments: impl Fn(&str, &str) -> Vec<String>) -> Compiled {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(path("First.kt"), FIRST).expect("write the first source");
    std::fs::write(path("Second.kt"), SECOND).expect("write the second source");
    let (first, second) = (text(&path("First.kt")), text(&path("Second.kt")));
    let arguments = |output: &str| {
        let mut arguments = vec![
            first.clone(),
            second.clone(),
            "-d".to_string(),
            text(&path(output)),
        ];
        arguments.extend(fragment_arguments(&first, &second));
        arguments
    };
    let (code, stderr) =
        common::kotlinc_compile(&arguments("kotlinc")).expect("the reference kotlinc is available");
    let kotlinc = (code, stderr, files(&path("kotlinc")));
    let result = std::process::Command::new(common::krusty_binary())
        .args(arguments("krusty"))
        .output()
        .expect("run krusty");
    let krusty = (
        result.status.code().expect("krusty exited"),
        String::from_utf8_lossy(&result.stderr).into_owned(),
        files(&path("krusty")),
    );
    let _ = std::fs::remove_dir_all(work);
    Compiled { kotlinc, krusty }
}

#[test]
fn one_fragment_with_every_source_compiles_as_kotlinc_does() {
    let compiled = compile(|first, second| {
        vec![
            "-Xmulti-platform".to_string(),
            "-Xfragments=main".to_string(),
            format!("-Xfragment-sources=main:{first}"),
            format!("-Xfragment-sources=main:{second}"),
        ]
    });
    let (code, stderr, output) = &compiled.kotlinc;
    assert_eq!((*code, stderr.as_str()), (0, ""), "kotlinc");
    assert_eq!(
        output
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [
            "META-INF/main.kotlin_module",
            "p/A.class",
            "p/FirstKt.class",
            "p/SecondKt.class"
        ]
    );
    assert_eq!(compiled.krusty, compiled.kotlinc);
}

#[test]
fn a_source_outside_every_fragment_is_kotlincs_configuration_error() {
    let compiled = compile(|first, _| {
        vec![
            "-Xmulti-platform".to_string(),
            "-Xfragments=main".to_string(),
            format!("-Xfragment-sources=other:{first}"),
        ]
    });
    let (code, stderr, output) = &compiled.kotlinc;
    assert_eq!(*code, 1, "kotlinc");
    assert_eq!(stderr.lines().count(), 3, "kotlinc: {stderr}");
    assert!(output.is_empty(), "kotlinc wrote {output:?}");
    assert_eq!(compiled.krusty, compiled.kotlinc);
}

#[test]
fn more_than_one_fragment_is_refused() {
    let compiled = compile(|first, second| {
        vec![
            "-Xmulti-platform".to_string(),
            "-Xfragments=common,jvm".to_string(),
            format!("-Xfragment-sources=common:{first}"),
            format!("-Xfragment-sources=jvm:{second}"),
            "-Xfragment-refines=jvm:common".to_string(),
        ]
    });
    let (code, stderr, _) = &compiled.kotlinc;
    assert_eq!((*code, stderr.as_str()), (0, ""), "kotlinc");
    assert_eq!(
        compiled.krusty,
        (
            2,
            "krusty: error: krusty does not implement the kotlinc argument \
             '-Xfragment-refines=jvm:common'\n\
             krusty: error: krusty compiles a single fragment, but -Xfragments declares 2: \
             common,jvm\n"
                .to_string(),
            Vec::new()
        )
    );
}
