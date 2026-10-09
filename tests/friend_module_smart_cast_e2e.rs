//! Smart-cast stability of properties decoded from a friend module (`-Xfriend-paths`, as a test
//! source set compiles against its main output). The dependency is built by kotlinc so this tests
//! the metadata consumer rather than allowing Krusty's writer and reader to agree on a wrong flag.

use super::common;

const LIB: &str = "package lib\n\
    sealed interface Shape\n\
    class Circle(val radius: Int) : Shape\n\
    class Square : Shape\n\
    class Holder(val stable: Shape, var mutable: Shape) {\n\
    \x20 val custom: Shape get() = stable\n\
    \x20 val delegated: Shape by lazy { stable }\n\
    }\n\
    open class OpenHolder(open val openShape: Shape)\n\
    class FinalHolder(shape: Shape) : OpenHolder(shape)\n";

const ACCEPTED: &str = "import lib.*\n\
    fun stable(h: Holder): Int = if (h.stable is Circle) h.stable.radius else -1\n\
    fun finalReceiver(h: FinalHolder): Int =\n\
    \x20 if (h.openShape is Circle) h.openShape.radius else -1\n\
    fun box(): String {\n\
    \x20 val stable = stable(Holder(Circle(7), Square()))\n\
    \x20 val inherited = finalReceiver(FinalHolder(Circle(9)))\n\
    \x20 return if (stable == 7 && inherited == 9) \"OK\" else \"$stable $inherited\"\n\
    }\n";

const UNSTABLE: &str = "import lib.*\n\
    fun mutable(h: Holder): Int = if (h.mutable is Circle) h.mutable.radius else -1\n\
    fun custom(h: Holder): Int = if (h.custom is Circle) h.custom.radius else -1\n\
    fun delegated(h: Holder): Int = if (h.delegated is Circle) h.delegated.radius else -1\n\
    fun open(h: OpenHolder): Int = if (h.openShape is Circle) h.openShape.radius else -1\n";

fn reference_library() -> std::path::PathBuf {
    common::kotlinc_library(LIB).expect("reference compiler builds the metadata dependency")
}

fn diagnostic_classpath(library: &std::path::Path) -> Vec<std::path::PathBuf> {
    vec![
        library.to_path_buf(),
        common::stdlib_jar(),
        common::jdk_modules(),
    ]
}

#[test]
fn friend_final_reads_match_kotlinc_and_run_with_the_narrowed_value() {
    let library = reference_library();
    let classpath = diagnostic_classpath(&library);
    let result = common::compiler_diagnostics_with_friend_paths(
        &[("Main.kt", ACCEPTED)],
        &classpath,
        std::slice::from_ref(&library),
    );
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, "")
    );
    assert_eq!((result.krusty_code, result.krusty_stderr.as_str()), (0, ""));
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);

    let stdlib = common::stdlib_jar();
    let runtime_classpath = [stdlib.clone(), library.clone()];
    let classes = common::compile_in_process_with_friend_paths(
        ACCEPTED,
        "Main",
        &runtime_classpath,
        std::slice::from_ref(&library),
        Some(common::jdk_modules().as_path()),
    )
    .expect("friend-stable properties compile");
    let box_class = common::find_box_class(&classes).expect("box() class");
    assert_eq!(
        common::run_box(&classes, &box_class, &runtime_classpath).expect("pooled box runner"),
        "OK"
    );
}

#[test]
fn friend_access_does_not_stabilize_mutable_custom_delegated_or_open_reads() {
    let library = reference_library();
    let classpath = diagnostic_classpath(&library);
    let result = common::compiler_diagnostics_with_friend_paths(
        &[("Main.kt", UNSTABLE)],
        &classpath,
        std::slice::from_ref(&library),
    );
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 2,
                column: 66,
                message: "unresolved reference 'radius'.".to_string(),
            },
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 3,
                column: 63,
                message: "unresolved reference 'radius'.".to_string(),
            },
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 4,
                column: 72,
                message: "unresolved reference 'radius'.".to_string(),
            },
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 5,
                column: 71,
                message: "unresolved reference 'radius'.".to_string(),
            },
        ]
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        [
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 2,
                column: 56,
                message: "smart cast to 'Circle' is impossible, because 'mutable' is a mutable property that could be mutated concurrently.".to_string(),
            },
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 3,
                column: 54,
                message: "smart cast to 'Circle' is impossible, because 'custom' is a property that has an open or custom getter.".to_string(),
            },
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 4,
                column: 60,
                message: "smart cast to 'Circle' is impossible, because 'delegated' is a property that has an open or custom getter.".to_string(),
            },
            common::CompilerError {
                file: "Main.kt".to_string(),
                line: 5,
                column: 59,
                message: "smart cast to 'Circle' is impossible, because 'openShape' is a property that has an open or custom getter.".to_string(),
            },
        ]
    );
}

#[test]
fn the_same_final_property_is_unstable_without_friend_access() {
    const MAIN: &str = "import lib.*\n\
        fun stable(h: Holder): Int = if (h.stable is Circle) h.stable.radius else -1\n";
    let library = reference_library();
    let result =
        common::compiler_diagnostics(&[("Main.kt", MAIN)], &diagnostic_classpath(&library));
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 2,
            column: 63,
            message: "unresolved reference 'radius'.".to_string(),
        }]
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 2,
            column: 54,
            message: "smart cast to 'Circle' is impossible, because 'stable' is a public API property declared in different module.".to_string(),
        }]
    );
}
