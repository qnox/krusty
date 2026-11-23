//! The unused return value warning (`-Xreturn-value-checker`), compared with kotlinc: every word it
//! reports, its position, and its exit status.
//!
//! A discarded call or property read warns when the declaration it selects must be used: a
//! declaration marked `@MustUseReturnValues` or enclosed by a class, an object, a property or a
//! file that is, a standard library declaration that records so, and in `full` mode every
//! declaration but an `@IgnorableReturnValue` one.

use std::path::{Path, PathBuf};

use super::common;

/// The release whose behavior the tests pin: the stable checker belongs to it.
const RELEASE: &str = "2.5.0-RC";

/// The diagnostics a compiler printed, without kotlinc's source excerpts and with the scratch
/// directory the source sits in removed.
fn ledger(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|line| line.contains("Source.kt") && !line.starts_with("krusty:"))
        .map(|line| match line.find("Source.kt") {
            Some(at) => line[at..].to_string(),
            None => line.to_string(),
        })
        .collect()
}

/// Compile `source` with both compilers under `arguments`; both must exit alike and report the same
/// diagnostics.
fn assert_like_kotlinc(source: &str, arguments: &[&str]) {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(path("Source.kt"), source).expect("write the source");
    let command_line = |output: &str| {
        let mut command_line = vec![text(&path("Source.kt")), "-d".into(), text(&path(output))];
        command_line.extend(arguments.iter().map(|argument| argument.to_string()));
        command_line
    };
    let (kotlinc_code, kotlinc_stderr) =
        common::kotlinc_server::kotlinc_compile_for_release(RELEASE, &command_line("kotlinc"))
            .expect("the reference kotlinc is available");
    assert!(
        kotlinc_stderr.contains("warning:")
            || arguments.contains(&"-Xreturn-value-checker=disable"),
        "the case must make kotlinc warn:\n{kotlinc_stderr}"
    );
    let krusty = std::process::Command::new(common::krusty_binary())
        .arg(format!("-Xkotlin-reference-version={RELEASE}"))
        .args(command_line("krusty"))
        .output()
        .expect("run krusty");
    assert_eq!(
        (
            krusty.status.code(),
            ledger(&String::from_utf8_lossy(&krusty.stderr))
        ),
        (Some(kotlinc_code), ledger(&kotlinc_stderr)),
        "{arguments:?}"
    );
    let _ = std::fs::remove_dir_all(work);
}

const SCOPES: &str = "\
fun plain(): String = \"\"

@MustUseReturnValues
object Scoped {
    fun member(): String = \"\"

    @IgnorableReturnValue
    fun quiet(): String = \"\"

    val property: String = \"\"

    class Nested {
        fun deep(): String = \"\"
    }
}

@MustUseReturnValues
class Holder {
    fun member(): String = \"\"
    val property: String get() = \"\"
}

class Unmarked {
    fun other(): String = \"\"
}

fun test(holder: Holder, unmarked: Unmarked) {
    plain()
    Scoped.member()
    Scoped.quiet()
    Scoped.property
    Scoped.Nested().deep()
    holder.member()
    holder.property
    unmarked.other()
}
";

#[test]
fn a_marked_declaration_or_scope_makes_a_discarded_result_a_warning() {
    assert_like_kotlinc(SCOPES, &[]);
}

#[test]
fn the_check_mode_is_the_default_of_the_stable_release() {
    assert_like_kotlinc(SCOPES, &["-Xreturn-value-checker=check"]);
    assert_like_kotlinc(SCOPES, &["-Xreturn-value-checker=default"]);
}

/// With `-language-version=2.4` the checker is off unless a mode names it, and the annotations are
/// rejected while it is off.
#[test]
fn a_mode_selected_on_an_older_language_version_checks_the_same_declarations() {
    assert_like_kotlinc(
        SCOPES,
        &["-language-version=2.4", "-Xreturn-value-checker=check"],
    );
}

#[test]
fn the_disabled_checker_reports_no_unused_value() {
    assert_like_kotlinc(
        "@kotlin.MustUseReturnValues object O { fun marked(): String = \"\" }\nfun test() { O.marked() }\n",
        &["-Xreturn-value-checker=disable"],
    );
}

#[test]
fn the_full_mode_checks_every_declaration_except_an_ignorable_one() {
    assert_like_kotlinc(SCOPES, &["-Xreturn-value-checker=full"]);
}

const FILE_SCOPE: &str = "\
@file:MustUseReturnValues
package p

fun filed(): String = \"\"

object Inner {
    @IgnorableReturnValue
    fun ignorable(): String = \"\"
}

fun test() {
    filed()
    Inner.ignorable()
}
";

#[test]
fn a_file_annotation_marks_every_declaration_of_the_file() {
    assert_like_kotlinc(FILE_SCOPE, &[]);
}

const INHERITANCE: &str = "\
interface Base {
    @IgnorableReturnValue
    fun ignorable(): String = \"\"
    fun unspecified(): String = \"\"
}

@MustUseReturnValues
interface Marked {
    fun used(): String = \"\"
}

class Impl : Base, Marked {
    override fun ignorable(): String = \"\"
    override fun unspecified(): String = \"\"
    override fun used(): String = \"\"
}

fun test(impl: Impl) {
    impl.ignorable()
    impl.unspecified()
    impl.used()
}
";

#[test]
fn an_override_takes_the_status_of_the_declaration_it_overrides() {
    assert_like_kotlinc(INHERITANCE, &[]);
}

#[test]
fn the_full_mode_leaves_an_override_of_an_unspecified_declaration_alone() {
    assert_like_kotlinc(INHERITANCE, &["-Xreturn-value-checker=full"]);
}

const POSITIONS: &str = "\
@MustUseReturnValues
object M {
    fun marked(): String = \"\"
    fun nullable(): String? = null
    fun nothing(): Nothing = throw IllegalStateException()
    fun unit() {}
}

fun test(s: String?, flag: Boolean) {
    val used = M.marked()
    println(M.marked())
    M.unit()
    s?.length
    M.nullable()!!
    M.nullable() ?: M.marked()
    M.marked() as Any
    if (flag) M.marked() else M.marked()
    try {
        M.marked()
    } finally {
        M.marked()
    }
    \"\".lowercase()
    1.digitToChar()
    println(used)
}
";

#[test]
fn only_a_discarded_value_of_a_usable_type_is_reported() {
    assert_like_kotlinc(POSITIONS, &[]);
}

#[test]
fn a_suppressed_warning_is_not_reported() {
    assert_like_kotlinc(
        "@MustUseReturnValues object M { fun marked(): String = \"\" }\n\
         fun first() { @Suppress(\"RETURN_VALUE_NOT_USED\") M.marked() }\n\
         @Suppress(\"RETURN_VALUE_NOT_USED\") fun second() { M.marked() }\n\
         fun third() { M.marked() }\n",
        &[],
    );
}
