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

/// The console functions and `Any?.toString()` run through their stdlib bodies: a safe call over
/// the argument and the root class's members, which the runtime dispatches on the receiver.
const PRINTING: &str = "\
class Named(private val name: String) {
    override fun toString(): String = \"Named($name)\"
}

fun render(value: Any?): String = value.toString()

fun box(): String {
    if (render(null) != \"null\") return \"absent\"
    if (render(42) != \"42\") return \"number\"
    if (render(Named(\"value\")) != \"Named(value)\") return \"named\"
    return \"OK\"
}

fun main() {
    val result = box()
    if (result != \"OK\") throw IllegalStateException(result)
    val absent: Any? = null
    println(\"text\")
    println(absent)
    println(42)
    println(Named(\"value\"))
    print(\"no newline\")
    println()
}
";

#[test]
fn a_program_prints_through_the_stdlib_bodies() {
    if !native_compiler_available() {
        return;
    }
    assert_eq!(common::kotlinc_box_result(PRINTING), "OK");
    common::expect_box_ok_with_stdlib(PRINTING, "compilerCommandPrinting");

    let scratch = common::scratch_dir().expect("allocate the compiler command's directory");
    let main = scratch.join("main.kt");
    std::fs::write(&main, PRINTING).expect("write main.kt");
    let compiled = Command::new(common::krusty_binary())
        .args(["kotlinc-native", "-o"])
        .arg(scratch.join("app"))
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
    assert_eq!(
        completed(run),
        (
            Some(0),
            "text\nnull\n42\nNamed(value)\nno newline\n".to_owned(),
            String::new()
        )
    );
    std::fs::remove_dir_all(&scratch).expect("remove the compiler command's directory");
}

/// Iterables of the program next to the stdlib's: `map` reaches `collectionSizeOrDefault`, whose
/// body smart-casts its receiver to `Collection<*>` and reads `size` through the interface's
/// accessor, and `is` against a read-only collection interface answers for runtime collections
/// and for classes of the program alike.
const COLLECTION_CLASSES: &str = "\
class Letters : Iterable<String> {
    override fun iterator(): Iterator<String> = listOf(\"c\", \"b\", \"a\").iterator()
}

open class Pair2 : Collection<String> {
    override val size: Int get() = 2
    override fun isEmpty(): Boolean = size == 0
    override fun contains(element: String): Boolean = element == \"x\" || element == \"y\"
    override fun containsAll(elements: Collection<String>): Boolean {
        for (element in elements) if (!contains(element)) return false
        return true
    }
    override fun iterator(): Iterator<String> = listOf(\"x\", \"y\").iterator()
}

class SubPair : Pair2()

";

const COLLECTION_MAIN: &str = "\
fun sized(values: Iterable<String>): Boolean = values is Collection<*>

fun box(): String {
    val list: Iterable<String> = listOf(\"a\", \"bb\", \"ccc\")
    if (!sized(list)) return \"a list is a collection\"
    if (sized(Letters())) return \"a plain iterable is no collection\"
    if (!sized(Pair2())) return \"a program collection is a collection\"
    if (!sized(SubPair())) return \"a subclass of a program collection is a collection\"
    val anything: Any = Letters()
    if (anything !is Iterable<*>) return \"letters are iterable\"
    if (anything is List<*>) return \"letters are no list\"
    if (list.map { it.length } != listOf(1, 2, 3)) return \"map\"
    if (Letters().map { it + \"!\" } != listOf(\"c!\", \"b!\", \"a!\")) return \"map without a size\"
    if (list.filter { it.length > 1 } != listOf(\"bb\", \"ccc\")) return \"filter\"
    return \"OK\"
}

fun main() {
    val result = box()
    if (result != \"OK\") throw IllegalStateException(result)
    println(listOf(1, 2).map { it * 2 })
}
";

#[test]
fn a_program_maps_and_checks_collections_through_the_stdlib_bodies() {
    if !native_compiler_available() {
        return;
    }
    let source = format!("{COLLECTION_CLASSES}\n{COLLECTION_MAIN}");
    assert_eq!(common::kotlinc_box_result(&source), "OK");
    common::expect_box_ok_with_stdlib(&source, "compilerCommandCollections");

    let scratch = common::scratch_dir().expect("allocate the compiler command's directory");
    let classes = scratch.join("classes.kt");
    let main = scratch.join("main.kt");
    std::fs::write(&classes, COLLECTION_CLASSES).expect("write classes.kt");
    std::fs::write(&main, COLLECTION_MAIN).expect("write main.kt");
    let compiled = Command::new(common::krusty_binary())
        .args(["kotlinc-native", "-o"])
        .arg(scratch.join("app"))
        .arg(&classes)
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
    assert_eq!(
        completed(run),
        (Some(0), "[2, 4]\n".to_owned(), String::new())
    );
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
