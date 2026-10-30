//! `krusty kotlinc-native`: sources in several files compiled against the Native stdlib KLIB and
//! linked into a static program that runs.
//!
//! The program's answer is first established on the JVM lane from the same declarations; the
//! Native program checks it and fails with an uncaught exception when it differs.

use std::process::{Command, Output};

use super::common;

const SHAPES: &str = "\
interface Shape {
    fun area(): Int
}

class Rectangle(private val width: Int, private val height: Int) : Shape {
    override fun area(): Int = width * height
}

fun Shape.doubled(): Int = area() * 2

fun total(shapes: List<Shape>): Int {
    var sum = 0
    for (shape in shapes) sum += shape.doubled()
    return sum
}
";

const MAIN: &str = "\
fun box(): String = if (total(listOf(Rectangle(2, 3), Rectangle(4, 5))) == 52) \"OK\" else \"fail\"

fun main() {
    val result = box()
    if (result != \"OK\") throw IllegalStateException(result)
}
";

fn completed(output: Output) -> (Option<i32>, String, String) {
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Whether this host links Native programs for a Kotlin/Native target and has the Native stdlib.
fn native_compiler_available() -> bool {
    let linkable = krusty::native::NativeTarget::host()
        .is_some_and(|target| target.konan_name().is_some() && krusty::native::can_link(target));
    if !linkable {
        return false;
    }
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(
        krusty::compilation_target::CompilationTarget::Native,
    );
    assert!(
        stdlib.is_some() || std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
        "required Native stdlib KLIB is unavailable"
    );
    stdlib.is_some()
}

#[test]
fn a_program_from_several_files_compiles_links_and_runs() {
    if !native_compiler_available() {
        return;
    }
    common::expect_box_ok_with_stdlib(&format!("{SHAPES}\n{MAIN}"), "compilerCommand");

    let scratch = common::scratch_dir().expect("allocate the compiler command's directory");
    let shapes = scratch.join("shapes.kt");
    let main = scratch.join("main.kt");
    std::fs::write(&shapes, SHAPES).expect("write shapes.kt");
    std::fs::write(&main, MAIN).expect("write main.kt");

    let compiled = Command::new(common::krusty_binary())
        .args(["kotlinc-native", "-o"])
        .arg(scratch.join("app"))
        .arg(&shapes)
        .arg(&main)
        .output()
        .expect("run krusty kotlinc-native");
    assert_eq!(
        completed(compiled),
        (Some(0), String::new(), String::new()),
        "the program compiles and links silently"
    );
    let run = common::run_freshly_written(&mut Command::new(scratch.join("app.kexe")))
        .expect("run the Native program");
    assert_eq!(completed(run), (Some(0), String::new(), String::new()));
    std::fs::remove_dir_all(&scratch).expect("remove the compiler command's directory");
}

#[test]
fn a_source_error_is_reported_and_no_program_is_written() {
    if !native_compiler_available() {
        return;
    }
    let scratch = common::scratch_dir().expect("allocate the compiler command's directory");
    let source = scratch.join("broken.kt");
    std::fs::write(&source, "fun main() {\n    missing()\n}\n").expect("write broken.kt");
    let compiled = Command::new(common::krusty_binary())
        .args(["kotlinc-native", "-o"])
        .arg(scratch.join("broken"))
        .arg(&source)
        .output()
        .expect("run krusty kotlinc-native");
    assert_eq!(
        completed(compiled),
        (
            Some(1),
            String::new(),
            format!(
                "{}:2:5: error: unresolved reference 'missing'.\n",
                source.display()
            )
        )
    );
    assert!(!scratch.join("broken.kexe").exists());
    std::fs::remove_dir_all(&scratch).expect("remove the compiler command's directory");
}
