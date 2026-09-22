//! A `suspend fun` and the anonymous objects it declares share one `$N` sequence.
//!
//! The reference compiler numbers the continuation FIRST: a suspend function reserves
//! `<owner>$<fn>$1` for it and its anonymous objects start at `$2`. The reservation is made even
//! when the function never suspends and no continuation class is emitted, so the ordinal a later
//! same-named overload's object gets does not depend on whether an earlier one suspends.
//!
//! krusty named the objects first, so a suspend function returning an `object : T` already owned
//! `<owner>$<fn>$1` when the coroutine transform asked for it: two classes, one name. The
//! continuation's own constructor call then resolved to the anonymous object's constructor and
//! emission aborted the WHOLE FILE on the arity disagreement, so one such function stopped its
//! module emitting anything at all.
use super::common;

/// The names krusty gives the classes of a source file, sorted.
fn krusty_class_names(name: &str, src: &str) -> Vec<String> {
    let cp = [
        common::stdlib_jar(),
        common::coroutines_jar(),
        common::jdk_modules(),
    ];
    let mut names: Vec<String> =
        common::compile_in_process_metadata_cp_module_target(src, name, &cp, "main", None)
            .unwrap_or_else(|| panic!("{name}: krusty failed to compile"))
            .into_iter()
            .map(|(class, _)| class)
            .collect();
    names.sort();
    names
}

/// The names the REFERENCE compiler gives them, sorted — the whole set, not a chosen subset.
///
/// `None` only when no reference compiler is provisioned at all. When one is, it must compile the
/// fixture: a silent skip would leave the measured claim unchecked, which is the thing these tests
/// exist to prevent.
fn reference_class_names(name: &str, src: &str) -> Option<Vec<String>> {
    let work = common::scratch_dir()?.join(format!("{name}-continuation-ordinal-reference"));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("create the reference fixture directory");
    let source = work.join(format!("{name}.kt"));
    std::fs::write(&source, src).expect("write the reference fixture");
    let out = work.join("classes");
    let classpath = std::env::join_paths([
        common::stdlib_jar(),
        common::coroutines_jar(),
        common::jdk_modules(),
    ])
    .expect("join the reference classpath");
    let args = vec![
        "-jvm-target".to_string(),
        "25".to_string(),
        "-cp".to_string(),
        classpath.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.display().to_string(),
        source.display().to_string(),
    ];
    let (code, diagnostics) = common::kotlinc_compile(&args)?;
    assert_eq!(
        code, 0,
        "{name}: the reference compiler rejected the fixture: {diagnostics}"
    );
    let mut names = Vec::new();
    let mut pending = vec![out.clone()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read the reference output directory") {
            let path = entry.expect("read a reference output entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path
                .extension()
                .is_some_and(|extension| extension == "class")
            {
                let relative = path.strip_prefix(&out).expect("a path under the output");
                names.push(
                    relative
                        .with_extension("")
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                );
            }
        }
    }
    names.sort();
    Some(names)
}

/// Both compilers' complete class sets for one source, against each other and against the
/// spelling this test pins.
fn assert_class_names(name: &str, src: &str, expected: &[&str]) {
    assert_eq!(krusty_class_names(name, src), expected, "{name}: krusty");
    if let Some(reference) = reference_class_names(name, src) {
        assert_eq!(reference, expected, "{name}: the reference compiler");
    }
}

const OBJECT_IN_A_SUSPEND_FUNCTION: &str = "\
interface Holder { val first: String; val second: String }
suspend fun fetch(id: String): String = id + \"!\"
suspend fun make(before: String, id: String): Holder {
    val after: String = fetch(id)
    return object : Holder {
        override val first: String = before
        override val second: String = after
    }
}
";

/// The continuation holds `$1` and the object it would have collided with holds `$2`.
#[test]
fn a_suspend_function_reserves_the_first_ordinal_for_its_continuation() {
    assert_class_names(
        "Names",
        OBJECT_IN_A_SUSPEND_FUNCTION,
        &["Holder", "NamesKt", "NamesKt$make$1", "NamesKt$make$2"],
    );
}

const OVERLOADS: &str = "\
interface Holder { val first: String }
suspend fun fetch(id: String): String = id + \"!\"
suspend fun make(id: String): Holder {
    val after: String = fetch(id)
    return object : Holder { override val first: String = after }
}
suspend fun make(id: Int): Holder {
    val after: String = fetch(id.toString())
    return object : Holder { override val first: String = after }
}
";

/// One sequence spans the overloads: each takes its continuation's ordinal before its own object's,
/// so the second overload's continuation is `$3`, not `$2`.
#[test]
fn same_named_suspend_overloads_share_one_ordinal_sequence() {
    assert_class_names(
        "Overloads",
        OVERLOADS,
        &[
            "Holder",
            "OverloadsKt",
            "OverloadsKt$make$1",
            "OverloadsKt$make$2",
            "OverloadsKt$make$3",
            "OverloadsKt$make$4",
        ],
    );
}

const MIXED_OVERLOADS: &str = "\
interface Holder { val first: String }
suspend fun fetch(id: String): String = id + \"!\"
fun make(id: String): Holder = object : Holder { override val first: String = id }
suspend fun make(id: Int): Holder {
    val after: String = fetch(id.toString())
    return object : Holder { override val first: String = after }
}
fun make(id: Long): Holder = object : Holder { override val first: String = id.toString() }
";

/// Only a `suspend` overload reserves an ordinal, and the sequence stays in declaration order: the
/// ordinary overload's object takes `$1`, the suspend one's continuation `$2` and its object `$3`,
/// and the ordinary overload after it `$4`.
#[test]
fn only_the_suspend_overloads_reserve_an_ordinal() {
    assert_class_names(
        "Mixed",
        MIXED_OVERLOADS,
        &[
            "Holder",
            "MixedKt",
            "MixedKt$make$1",
            "MixedKt$make$2",
            "MixedKt$make$3",
            "MixedKt$make$4",
        ],
    );
}

const NO_SUSPENSION_POINT: &str = "\
interface Holder { val first: String }
suspend fun make(id: String): Holder = object : Holder { override val first: String = id }
";

/// The ordinal is reserved by the `suspend` modifier, not by an actual suspension: this function
/// emits no continuation class and its object is still `$2`.
#[test]
fn a_suspend_function_that_never_suspends_still_reserves_its_ordinal() {
    assert_class_names(
        "Idle",
        NO_SUSPENSION_POINT,
        &["Holder", "IdleKt", "IdleKt$make$2"],
    );
}

const MEMBER: &str = "\
interface Holder { val first: String }
class Service {
    suspend fun fetch(id: String): String = id + \"!\"
    suspend fun make(id: String): Holder {
        val after: String = fetch(id)
        return object : Holder { override val first: String = after }
    }
}
";

/// A member's sequence is owned by its class, and reserves the same way.
#[test]
fn a_suspend_member_reserves_its_ordinal_under_its_own_class() {
    assert_class_names(
        "Member",
        MEMBER,
        &["Holder", "Service", "Service$make$1", "Service$make$2"],
    );
}

const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
    interface Holder { val first: String; val second: String }\n\
    suspend fun fetch(id: String): String = id + \"!\"\n\
    suspend fun make(before: String, id: String): Holder {\n\
    \x20   val after: String = fetch(id)\n\
    \x20   return object : Holder {\n\
    \x20       override val first: String = before\n\
    \x20       override val second: String = after\n\
    \x20   }\n\
    }\n\
    fun box(): String = runBlocking {\n\
    \x20   val h = make(\"A\", \"B\")\n\
    \x20   if (h.first == \"A\" && h.second == \"B!\") \"OK\" else h.first + \"/\" + h.second\n\
    }\n";

/// It runs, and the object still carries BOTH captures across the suspension: one from before the
/// suspension point and one produced after it.
#[test]
fn a_suspend_function_that_declares_an_anonymous_object_runs() {
    let jdk = common::jdk_modules();
    let cp = [common::stdlib_jar(), common::coroutines_jar(), jdk.clone()];
    assert_eq!(
        common::expect_box_run(MAIN, "Main", &cp, Some(jdk.as_path())),
        "OK"
    );
}
