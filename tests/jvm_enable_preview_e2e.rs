//! `-Xjvm-enable-preview` marks every class file as compiled with preview features (minor version
//! 65535) and, from JVM target 28, gives each non-interface `InnerClasses` row the identity flag.
//! Both compilers compile one program under the argument and every class must carry the same minor
//! version and `InnerClasses` rows.

use std::path::{Path, PathBuf};

use super::common;

/// One compiler's classes: file name and bytes.
type Classes = Vec<(String, Vec<u8>)>;

const SOURCE: &str = "import java.util.AbstractMap\n\
    class A {\n\
    \x20 class B\n\
    \x20 abstract class Abstract\n\
    \x20 open class C\n\
    \x20 interface I\n\
    \x20 annotation class Marker\n\
    \x20 enum class E { X { override fun f() = 1 }, Y; open fun f() = 0 }\n\
    \x20 object O\n\
    \x20 companion object\n\
    \x20 fun local(): Any { class L; return L() }\n\
    \x20 fun anonymous(): Any = object : Runnable { override fun run() {} }\n\
    \x20 fun external(): Any = AbstractMap.SimpleEntry(\"k\", 1)\n\
    \x20 fun mapped(e: E) = when (e) { E.X -> 1; E.Y -> 2 }\n\
    }\n";

fn classes(dir: &Path) -> Classes {
    let mut found: Vec<_> = std::fs::read_dir(dir)
        .expect("read compiler output")
        .map(|entry| entry.expect("compiler output entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "class"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("class file name")
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read(&path).expect("read class"))
        })
        .collect();
    found.sort();
    found
}

/// The class file's minor version.
fn minor_version(class: &[u8]) -> u16 {
    u16::from_be_bytes([class[4], class[5]])
}

/// What both compilers print on stderr: `-Xjvm-enable-preview` announces itself.
fn announcement(arguments: &[&str]) -> String {
    if arguments.contains(&"-Xjvm-enable-preview") {
        "info: using preview Java language features\n".to_string()
    } else {
        String::new()
    }
}

/// Compile `SOURCE` with both compilers under `arguments`; return each one's classes.
fn compile_both(arguments: &[&str]) -> (Classes, Classes) {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(path("A.kt"), SOURCE).expect("write the source");
    let command = |output: &str| -> Vec<String> {
        let mut command = vec![text(&path("A.kt")), "-d".into(), text(&path(output))];
        command.extend(arguments.iter().map(|argument| argument.to_string()));
        command
    };
    let (code, stderr) =
        common::kotlinc_compile(&command("kotlinc")).expect("the reference kotlinc is available");
    assert_eq!(code, 0, "kotlinc {arguments:?}: {stderr}");
    assert_eq!(stderr, announcement(arguments), "kotlinc {arguments:?}");
    let result = std::process::Command::new(common::krusty_binary())
        .args(command("krusty"))
        .output()
        .expect("run krusty");
    assert_eq!(
        (
            result.status.code(),
            String::from_utf8_lossy(&result.stderr).into_owned()
        ),
        (Some(0), announcement(arguments)),
        "krusty {arguments:?}"
    );
    let pair = (classes(&path("kotlinc")), classes(&path("krusty")));
    let _ = std::fs::remove_dir_all(work);
    pair
}

/// A class's minor version and `InnerClasses` rows, the two things `-Xjvm-enable-preview` changes.
fn preview_shape(bytes: &[u8]) -> (u16, Vec<krusty::jvm::classreader::InnerClassRef>) {
    let info = krusty::jvm::classreader::parse_class(bytes).expect("a parseable class");
    (minor_version(bytes), info.inner_classes)
}

/// The newest `-jvm-target` the reference kotlinc accepts, and the one before it. The supported
/// releases stop at 26, below target 28 where the identity flag begins.
fn newest_targets() -> (&'static str, &'static str) {
    use krusty::kotlin_version::KotlinVersion;
    common::for_reference(&[(KotlinVersion::V2_4_21, ("26", "25"))])
}

fn assert_matches_kotlinc(arguments: &[&str], preview: bool) {
    let (expected, actual) = compile_both(arguments);
    assert!(!expected.is_empty(), "kotlinc emitted no classes");
    let names = |classes: &[(String, Vec<u8>)]| -> Vec<String> {
        classes.iter().map(|(name, _)| name.clone()).collect()
    };
    assert_eq!(names(&actual), names(&expected), "{arguments:?}");
    for ((name, ours), (_, reference)) in actual.iter().zip(&expected) {
        let (minor, rows) = preview_shape(ours);
        assert_eq!(minor == 0xFFFF, preview, "{arguments:?}: {name}");
        assert_eq!(
            (minor, rows),
            preview_shape(reference),
            "{arguments:?}: {name}"
        );
    }
}

#[test]
fn preview_marks_every_class_with_the_preview_minor_version_below_target_28() {
    for target in ["17", newest_targets().1] {
        assert_matches_kotlinc(&["-jvm-target", target, "-Xjvm-enable-preview"], true);
    }
}

#[test]
fn preview_from_target_28_gives_every_non_interface_inner_class_row_the_identity_flag() {
    assert_matches_kotlinc(
        &["-jvm-target", newest_targets().0, "-Xjvm-enable-preview"],
        true,
    );
}

#[test]
fn preview_marks_class_mode_lambda_classes_like_kotlinc() {
    assert_matches_kotlinc(
        &[
            "-jvm-target",
            newest_targets().0,
            "-Xjvm-enable-preview",
            "-Xlambdas=class",
        ],
        true,
    );
}

#[test]
fn without_preview_no_class_carries_the_minor_version_or_the_identity_flag() {
    assert_matches_kotlinc(&["-jvm-target", newest_targets().0], false);
}
