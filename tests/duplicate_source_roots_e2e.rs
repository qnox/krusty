//! A command line that names a source more than once compiles it once, as kotlinc does: a root
//! repeated with the same spelling is reported as `ROOTS_RESOLUTION_WARNING`, and a file reached
//! through a directory, or under another spelling, is dropped silently. Build tools pass both: the
//! Kotlin Gradle plugin hands the Build Tools API each source in its arguments and again as the
//! operation's sources.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use super::common;

fn output_files(root: &Path) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("read compiler output entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .expect("output is under its requested root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    files
}

#[test]
fn a_source_named_twice_compiles_once_like_kotlinc() {
    let dir = common::scratch_dir().expect("allocate a scratch directory");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("G.kt"), "fun g() = 1\n").unwrap();
    std::fs::write(src.join("H.kt"), "fun h() = 2\n").unwrap();
    let g = src.join("G.kt").display().to_string();
    let dotted = src.join(".").join("G.kt").display().to_string();
    let warning = format!("warning: duplicate source root: {g}");
    let werror = "error: warnings found and -Werror specified".to_string();

    let cases: [(&str, Vec<String>, i32, Vec<String>); 4] = [
        (
            "repeated",
            vec![g.clone(), g.clone()],
            0,
            vec![warning.clone()],
        ),
        (
            "reached-twice",
            vec![g.clone(), src.display().to_string(), dotted],
            0,
            Vec::new(),
        ),
        (
            "werror",
            vec![g.clone(), g.clone(), "-Werror".into()],
            1,
            vec![warning, werror],
        ),
        (
            "disabled",
            vec![
                g.clone(),
                g.clone(),
                "-Xwarning-level=ROOTS_RESOLUTION_WARNING:disabled".into(),
            ],
            0,
            Vec::new(),
        ),
    ];
    for (tag, arguments, code, diagnostics) in cases {
        let output = |compiler: &str| {
            let mut all = arguments.clone();
            all.push("-d".into());
            all.push(dir.join(tag).join(compiler).display().to_string());
            all
        };
        let (kotlinc_code, kotlinc_stderr) =
            common::kotlinc_server::kotlinc_compile_unrecorded(&output("kotlinc"))
                .expect("the reference kotlinc is available");
        let krusty = Command::new(common::krusty_binary())
            .args(output("krusty"))
            .output()
            .expect("run krusty");
        let lines = |text: &str| text.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            (kotlinc_code, lines(&kotlinc_stderr)),
            (code, diagnostics.clone()),
            "{tag}: kotlinc"
        );
        assert_eq!(
            (
                krusty.status.code(),
                lines(&String::from_utf8_lossy(&krusty.stderr))
            ),
            (Some(code), diagnostics),
            "{tag}: krusty"
        );
        assert_eq!(
            output_files(&dir.join(tag).join("krusty")),
            output_files(&dir.join(tag).join("kotlinc")),
            "{tag}"
        );
    }
}

/// kotlinc compares source files by their lexical absolute path, so one file reached through a
/// symlinked directory is a second input and its declarations conflict with the first's.
#[test]
fn a_symlinked_spelling_is_a_second_source_like_kotlinc() {
    let dir = common::scratch_dir().expect("allocate a scratch directory");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("G.kt"), "fun g() = 1\n").unwrap();
    std::os::unix::fs::symlink(&src, dir.join("alias")).unwrap();
    let arguments = |compiler: &str| {
        vec![
            src.join("G.kt").display().to_string(),
            dir.join("alias").join("G.kt").display().to_string(),
            "-d".to_string(),
            dir.join(compiler).display().to_string(),
        ]
    };
    let (kotlinc_code, kotlinc_stderr) =
        common::kotlinc_server::kotlinc_compile_unrecorded(&arguments("kotlinc"))
            .expect("the reference kotlinc is available");
    let krusty = Command::new(common::krusty_binary())
        .args(arguments("krusty"))
        .output()
        .expect("run krusty");
    // Both compilers print a source as it was spelled: relative to the working directory here.
    let working = std::env::current_dir().expect("the working directory");
    let conflict = |root: &Path| {
        format!(
            "{}:1:1: error: conflicting overloads:\nfun g(): Int\nfun g() = 1\n^^^^^^^\n",
            root.join("G.kt")
                .strip_prefix(&working)
                .expect("the scratch directory is under the working directory")
                .display()
        )
    };
    let expected = format!("{}{}", conflict(&dir.join("alias")), conflict(&src));
    assert_eq!(
        (kotlinc_code, kotlinc_stderr.as_str()),
        (1, expected.as_str()),
        "kotlinc"
    );
    // krusty reports the same two conflicts in its own rendering, one per spelling.
    let krusty_conflict = |root: &Path| {
        format!(
            "{}:1:1: error: conflicting overloads:\nfun g(): Int\nfun g() = 1\n^^^^^^^\n",
            root.join("G.kt")
                .strip_prefix(&working)
                .expect("the scratch directory is under the working directory")
                .display()
        )
    };
    let krusty_expected = format!(
        "{}{}",
        krusty_conflict(&src),
        krusty_conflict(&dir.join("alias"))
    );
    assert_eq!(
        (
            krusty.status.code(),
            String::from_utf8_lossy(&krusty.stderr).as_ref()
        ),
        (Some(1), krusty_expected.as_str()),
        "krusty"
    );
}

/// kotlinc folds `.` and `..` lexically before it reads a source, so `src/alias/../G.kt` is
/// `src/G.kt` even when `alias` is a symlink into another directory, and the file is read once.
#[test]
fn a_parent_component_after_a_symlink_folds_before_the_read_like_kotlinc() {
    let dir = common::scratch_dir().expect("allocate a scratch directory");
    let (src, other) = (dir.join("src"), dir.join("other"));
    std::fs::create_dir_all(other.join("deep")).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("G.kt"), "fun lexical() = 1\n").unwrap();
    std::fs::write(other.join("G.kt"), "fun reached() = 2\n").unwrap();
    std::os::unix::fs::symlink(Path::new("../other/deep"), src.join("alias")).unwrap();
    let through_link = src.join("alias").join("..").join("G.kt");
    let direct = src.join("G.kt");
    let program = |compiler: &str| {
        let output = dir.join(compiler);
        let arguments = vec![
            through_link.display().to_string(),
            direct.display().to_string(),
            "-d".to_string(),
            output.display().to_string(),
        ];
        let status = if compiler == "kotlinc" {
            common::kotlinc_server::kotlinc_compile_unrecorded(&arguments)
                .expect("the reference kotlinc is available")
                .0
        } else {
            Command::new(common::krusty_binary())
                .args(&arguments)
                .output()
                .expect("run krusty")
                .status
                .code()
                .expect("krusty exited")
        };
        let class = std::fs::read(output.join("GKt.class")).expect("the facade class");
        let holds = |name: &str| class.windows(name.len()).any(|w| w == name.as_bytes());
        (
            status,
            holds("lexical"),
            holds("reached"),
            output_files(&output),
        )
    };
    let kotlinc = program("kotlinc");
    assert_eq!(
        (kotlinc.0, kotlinc.1, kotlinc.2),
        (0, true, false),
        "kotlinc"
    );
    assert_eq!(program("krusty"), kotlinc, "krusty");
}

/// A relative spelling that climbs out of the working directory and back in names the same file as
/// its absolute spelling, so kotlinc compiles it once; a directory spelled with a trailing `..`
/// folds to the directory itself and is read.
#[test]
fn a_leading_parent_spelling_is_the_same_file_as_its_absolute_one_like_kotlinc() {
    let dir = common::scratch_dir().expect("allocate a scratch directory");
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("inner")).unwrap();
    std::fs::write(src.join("G.kt"), "fun g() = 1\n").unwrap();
    let working = std::env::current_dir().expect("the working directory");
    let name = working.file_name().expect("a named working directory");
    let up_and_down = Path::new("..")
        .join(name)
        .join(
            src.strip_prefix(&working)
                .expect("the scratch directory is under the working directory"),
        )
        .join("G.kt");
    let folding_away = src.join("inner").join("..");
    let cases: [(&str, Vec<String>); 2] = [
        (
            "climbs",
            vec![
                up_and_down.display().to_string(),
                src.join("G.kt").display().to_string(),
            ],
        ),
        ("folds-away", vec![folding_away.display().to_string()]),
    ];
    for (tag, sources) in cases {
        let arguments = |compiler: &str| {
            let mut all = sources.clone();
            all.push("-d".into());
            all.push(dir.join(tag).join(compiler).display().to_string());
            all
        };
        let (kotlinc_code, kotlinc_stderr) =
            common::kotlinc_server::kotlinc_compile_unrecorded(&arguments("kotlinc"))
                .expect("the reference kotlinc is available");
        let krusty = Command::new(common::krusty_binary())
            .args(arguments("krusty"))
            .output()
            .expect("run krusty");
        assert_eq!(
            (kotlinc_code, kotlinc_stderr.as_str()),
            (0, ""),
            "{tag}: kotlinc"
        );
        assert_eq!(
            (
                krusty.status.code(),
                String::from_utf8_lossy(&krusty.stderr).as_ref()
            ),
            (Some(0), ""),
            "{tag}: krusty"
        );
        let produced = output_files(&dir.join(tag).join("kotlinc"));
        assert!(produced.contains("GKt.class"), "{tag}: {produced:?}");
        assert_eq!(
            output_files(&dir.join(tag).join("krusty")),
            produced,
            "{tag}"
        );
    }
}
