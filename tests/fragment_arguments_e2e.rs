//! `-Xmulti-platform`, `-Xfragments`, `-Xfragment-sources` and `-Xfragment-refines` as a build tool
//! passes them: one fragment holding every source of a JVM-only module, or a common/platform
//! hierarchy of a multiplatform one. kotlinc's output is then the platform leaf's compilation, and
//! a malformed structure is a configuration error in kotlinc's words.

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
    compile_sources(FIRST, SECOND, fragment_arguments)
}

/// As [`compile`], over `first` and `second` as `First.kt` and `Second.kt`.
fn compile_sources(
    first_source: &str,
    second_source: &str,
    fragment_arguments: impl Fn(&str, &str) -> Vec<String>,
) -> Compiled {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(path("First.kt"), first_source).expect("write the first source");
    std::fs::write(path("Second.kt"), second_source).expect("write the second source");
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

/// kotlinc 2.4.20 treats a fragment name declared twice as one fragment, whether the list repeats
/// it or the argument does.
#[test]
fn a_fragment_declared_twice_compiles_as_kotlinc_does() {
    for fragments in [
        &["-Xfragments=main,main"][..],
        &["-Xfragments=main", "-Xfragments=main"],
    ] {
        let compiled = compile(|first, second| {
            let mut arguments = vec!["-Xmulti-platform".to_string()];
            arguments.extend(fragments.iter().map(ToString::to_string));
            arguments.push(format!("-Xfragment-sources=main:{first}"));
            arguments.push(format!("-Xfragment-sources=main:{second}"));
            arguments
        });
        let (code, stderr, output) = &compiled.kotlinc;
        assert_eq!((*code, stderr.as_str()), (0, ""), "kotlinc {fragments:?}");
        assert_eq!(output.len(), 4, "kotlinc {fragments:?}: {output:?}");
        assert_eq!(compiled.krusty, compiled.kotlinc, "{fragments:?}");
    }
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

/// kotlinc reports each repeated `-Xfragment-sources` value and each repeated free spelling once,
/// in first-seen order; `-Xfragment-sources=,` is one empty value.
#[test]
fn repeated_values_are_reported_once_as_kotlinc_does() {
    let compiled = compile(|first, second| {
        vec![
            "-Xfragments=main".to_string(),
            "-Xfragment-sources=bad,bad,other:x,other:x".to_string(),
            "-Xfragment-sources=,".to_string(),
            format!("-Xfragment-sources=main:{second}"),
            first.to_string(),
        ]
    });
    let (code, stderr, output) = &compiled.kotlinc;
    assert_eq!(*code, 1, "kotlinc");
    assert_eq!(stderr.lines().count(), 4, "kotlinc: {stderr}");
    assert!(output.is_empty(), "kotlinc wrote {output:?}");
    assert_eq!(compiled.krusty, compiled.kotlinc);
}

/// The empty name is a fragment: `-Xfragments=` with `-Xfragment-sources=:<file>` compiles.
#[test]
fn the_empty_fragment_name_compiles_as_kotlinc_does() {
    let compiled = compile(|first, second| {
        vec![
            "-Xfragments=".to_string(),
            format!("-Xfragment-sources=:{first}"),
            format!("-Xfragment-sources=:{second}"),
        ]
    });
    let (code, stderr, output) = &compiled.kotlinc;
    assert_eq!((*code, stderr.as_str()), (0, ""), "kotlinc");
    assert_eq!(output.len(), 4, "kotlinc: {output:?}");
    assert_eq!(compiled.krusty, compiled.kotlinc);
}

fn common_and_jvm(first: &str, second: &str) -> Vec<String> {
    vec![
        "-Xmulti-platform".to_string(),
        "-Xfragments=common,jvm".to_string(),
        format!("-Xfragment-sources=common:{first}"),
        format!("-Xfragment-sources=jvm:{second}"),
        "-Xfragment-refines=jvm:common".to_string(),
    ]
}

/// A common fragment the platform fragment refines compiles into the platform's output.
#[test]
fn a_refined_common_fragment_compiles_as_kotlinc_does() {
    let compiled = compile(common_and_jvm);
    let (code, stderr, output) = &compiled.kotlinc;
    assert_eq!((*code, stderr.as_str()), (0, ""), "kotlinc");
    assert_eq!(output.len(), 4, "kotlinc: {output:?}");
    assert_eq!(compiled.krusty, compiled.kotlinc);
}

/// The platform fragment actualizes the common fragment's `expect` declaration, which the common
/// fragment calls.
#[test]
fn an_actual_in_the_platform_fragment_compiles_as_kotlinc_does() {
    let compiled = compile_sources(
        "package p\nexpect fun top(): String\nfun useTop(): String = top() + \"!\"\n",
        "package p\nactual fun top(): String = \"a\"\n",
        common_and_jvm,
    );
    let (code, stderr, output) = &compiled.kotlinc;
    assert_eq!((*code, stderr.as_str()), (0, ""), "kotlinc");
    assert_eq!(
        output
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [
            "META-INF/main.kotlin_module",
            "p/FirstKt.class",
            "p/SecondKt.class"
        ]
    );
    assert_eq!(compiled.krusty, compiled.kotlinc);
}

/// kotlinc names the first fragment of its depth-first order whose refined fragment does not
/// precede it.
#[test]
fn a_refinement_cycle_is_kotlincs_configuration_error() {
    let compiled = compile(|first, second| {
        let mut arguments = common_and_jvm(first, second);
        arguments.push("-Xfragment-refines=common:jvm".to_string());
        arguments
    });
    let (code, stderr, output) = &compiled.kotlinc;
    assert_eq!(
        (*code, stderr.as_str()),
        (
            1,
            "error: there is a cycle in dependencies of module `jvm`\n"
        ),
        "kotlinc"
    );
    assert!(output.is_empty(), "kotlinc wrote {output:?}");
    assert_eq!(compiled.krusty, compiled.kotlinc);
}

#[test]
fn an_undeclared_refined_fragment_is_kotlincs_configuration_error() {
    let compiled = compile(|first, second| {
        let mut arguments = common_and_jvm(first, second);
        arguments.push("-Xfragment-refines=jvm:other,jvm:other".to_string());
        arguments
    });
    let (code, stderr, output) = &compiled.kotlinc;
    assert_eq!(
        (*code, stderr.as_str()),
        (
            1,
            "error: `-Xfragment-refines=jvm:other` Fragment `other` not found in -Xfragments \
             arguments\n"
        ),
        "kotlinc"
    );
    assert!(output.is_empty(), "kotlinc wrote {output:?}");
    assert_eq!(compiled.krusty, compiled.kotlinc);
}

/// Without a refinement edge kotlinc's leaf sees no other fragment (`Second.kt` cannot resolve
/// `A`); krusty refuses to fold one in rather than compile what kotlinc rejects.
#[test]
fn a_fragment_the_leaf_does_not_refine_is_refused() {
    let compiled = compile(|first, second| {
        vec![
            "-Xmulti-platform".to_string(),
            "-Xfragments=common,jvm".to_string(),
            format!("-Xfragment-sources=common:{first}"),
            format!("-Xfragment-sources=jvm:{second}"),
        ]
    });
    assert_eq!(compiled.kotlinc.0, 1, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        compiled.krusty,
        (
            2,
            "krusty: error: krusty compiles fragments the leaf refines, but `jvm` does not \
             refine `common`\n"
                .to_string(),
            Vec::new()
        )
    );
}

/// A common source sees its own fragment and the fragments it refines, never the platform leaf
/// that refines it: kotlinc analyzes each fragment as its own module.
fn rejected_in_common(common_source: &str, platform_source: &str) {
    let compiled = compile_sources(common_source, platform_source, common_and_jvm);
    let (code, stderr, output) = &compiled.kotlinc;
    assert_eq!((*code, output.len()), (1, 0), "kotlinc: {stderr}");
    let (krusty_code, krusty_stderr, krusty_output) = &compiled.krusty;
    assert_eq!(
        (*krusty_code, krusty_output.len()),
        (1, 0),
        "krusty: {krusty_stderr}"
    );
    // Each compiler renders the source path its own way; the diagnostics are compared exactly.
    assert_eq!(
        common::compiler_errors(krusty_stderr),
        common::compiler_errors(stderr)
    );
}

#[test]
fn a_common_call_of_a_platform_function_is_kotlincs_unresolved_reference() {
    rejected_in_common(
        "package p\nfun c() = platformOnly()\n",
        "package p\nfun platformOnly() = 1\n",
    );
}

#[test]
fn a_common_signature_naming_a_platform_class_is_kotlincs_unresolved_reference() {
    rejected_in_common(
        "package p\nfun c(value: PlatformOnly): Int = 1\n",
        "package p\nclass PlatformOnly\n",
    );
}

#[test]
fn a_common_read_of_a_platform_property_is_kotlincs_unresolved_reference() {
    rejected_in_common(
        "package p\nfun c() = platformOnly + 1\n",
        "package p\nval platformOnly = 1\n",
    );
}

#[test]
fn a_common_call_of_a_platform_overload_binds_the_common_one_as_kotlinc_does() {
    let compiled = compile_sources(
        "package p\nfun f(value: Any) = \"any\"\nfun c() = f(1)\n",
        "package p\nfun f(value: Int) = \"int\"\nfun j() = f(1)\n",
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(compiled.krusty, compiled.kotlinc);
}

#[test]
fn a_common_signature_naming_a_platform_alias_is_kotlincs_unresolved_reference() {
    rejected_in_common(
        "package p\nfun c(value: Alias): Int = 1\n",
        "package p\ntypealias Alias = Int\n",
    );
}

#[test]
fn a_common_call_of_an_actual_only_member_is_kotlincs_unresolved_reference() {
    rejected_in_common(
        "package p\nexpect class Box()\nfun c() = Box().platformOnly()\n",
        "package p\nactual class Box actual constructor() {\n    fun platformOnly() = 1\n}\n",
    );
}

#[test]
fn a_common_read_of_an_actual_only_property_is_kotlincs_unresolved_reference() {
    rejected_in_common(
        "package p\nexpect class Box()\nfun c() = Box().extra\n",
        "package p\nactual class Box actual constructor() {\n    val extra = 1\n}\n",
    );
}

#[test]
fn a_common_call_of_an_actual_only_constructor_is_kotlincs_missing_parameter() {
    rejected_in_common(
        "package p\nexpect class Box()\nfun c() = Box(x = 1)\n",
        "package p\nactual class Box actual constructor() {\n    constructor(x: Int) : this()\n}\n",
    );
}

#[test]
fn a_common_call_of_an_expect_member_binds_its_actual_as_kotlinc_does() {
    let compiled = compile_sources(
        "package p\nexpect class Box() {\n    fun shared(): Int\n}\nfun c() = Box().shared()\n",
        "package p\nactual class Box actual constructor() {\n    actual fun shared() = 1\n    fun platformOnly() = shared()\n}\nfun j() = Box().platformOnly() + c()\n",
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        (compiled.krusty.0, &compiled.krusty.2),
        (compiled.kotlinc.0, &compiled.kotlinc.2)
    );
}

#[test]
fn a_common_read_of_a_platform_object_is_kotlincs_unresolved_reference() {
    rejected_in_common(
        "package p\nfun c() { val value = PlatformOnly }\n",
        "package p\nobject PlatformOnly\n",
    );
}

#[test]
fn a_common_import_of_a_platform_function_is_kotlincs_unresolved_reference() {
    rejected_in_common(
        "package p\nimport p.platformOnly\nfun c(): Int = 1\n",
        "package p\nfun platformOnly() = 1\n",
    );
}

#[test]
fn an_inferred_common_signature_binds_the_platform_actual_as_kotlinc_does() {
    let compiled = compile_sources(
        "package p\nexpect fun top(): String\nfun inferred() = top()\n",
        "package p\nactual fun top(): String = \"a\"\nfun j() = inferred()\n",
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(compiled.krusty, compiled.kotlinc);
}

#[test]
fn a_common_supertype_actualized_by_a_platform_alias_compiles_as_kotlinc_does() {
    let compiled = compile_sources(
        "package p\nexpect open class Lock()\nclass Guarded : Lock() {\n    fun value() = 1\n}\nfun read(guarded: Guarded) = guarded.value()\n",
        "package p\nactual typealias Lock = java.util.concurrent.locks.ReentrantLock\n",
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    // kotlinc also warns that expect/actual classes are Beta, which krusty does not report yet.
    let beta_warnings = compiled
        .kotlinc
        .1
        .lines()
        .filter(|line| line.contains(": warning: 'expect'/'actual' classes"))
        .count();
    assert_eq!(beta_warnings, 2, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        (
            compiled.krusty.0,
            compiled.krusty.1.as_str(),
            &compiled.krusty.2
        ),
        (compiled.kotlinc.0, "", &compiled.kotlinc.2)
    );
}

const ALIASED_LOCK: &str =
    "package p\nactual typealias Lock = java.util.concurrent.locks.ReentrantLock\n";

#[test]
fn a_common_call_of_an_alias_target_member_is_kotlincs_unresolved_reference() {
    rejected_in_common(
        "package p\nexpect open class Lock()\nfun c(lock: Lock) { lock.lock() }\nfun d(lock: Lock) = lock.newCondition()\n",
        ALIASED_LOCK,
    );
}

#[test]
fn a_common_alias_actualized_class_is_not_its_target_as_kotlinc_says() {
    rejected_in_common(
        "package p\nexpect open class Lock()\nfun c(lock: Lock): java.util.concurrent.locks.ReentrantLock = lock\n",
        ALIASED_LOCK,
    );
}

#[test]
fn a_common_call_of_the_alias_target_itself_compiles_as_kotlinc_does() {
    let compiled = compile_sources(
        "package p\nexpect open class Lock()\nfun c() = java.util.concurrent.locks.ReentrantLock().isLocked\n",
        ALIASED_LOCK,
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        (compiled.krusty.0, &compiled.krusty.2),
        (compiled.kotlinc.0, &compiled.kotlinc.2)
    );
}

#[test]
fn a_common_call_of_an_alias_actualized_expect_member_binds_the_target_as_kotlinc_does() {
    let compiled = compile_sources(
        "package p\nexpect open class Lock() {\n    fun lock()\n}\nfun c(lock: Lock): Lock { lock.lock(); return lock }\nclass Guarded(val l: Lock)\n",
        "package p\nactual typealias Lock = java.util.concurrent.locks.ReentrantLock\nfun j() = c(java.util.concurrent.locks.ReentrantLock()).isLocked && Guarded(Lock()).l.isLocked\n",
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        (compiled.krusty.0, &compiled.krusty.2),
        (compiled.kotlinc.0, &compiled.kotlinc.2)
    );
}

#[test]
fn every_declared_use_of_an_alias_actualized_expect_class_matches_kotlinc() {
    let compiled = compile_sources(
        "package p\nexpect open class Lock()\nclass Holder(val a: Lock?) {\n    constructor(x: Int) : this(null)\n    var b: Lock? = null\n    fun m(l: Lock): Lock? = l\n    fun Lock.ext(): Int = 1\n}\nfun Lock.top(): Map<String, Lock> = mapOf()\nval shared: Lock? = null\nfun d(lock: Lock) = lock\nopen class Sub : Lock()\n",
        "package p\nactual typealias Lock = java.util.concurrent.locks.ReentrantLock\nfun j(l: Lock): Lock = l\nclass JHolder(val l: Lock)\n",
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        (compiled.krusty.0, &compiled.krusty.2),
        (compiled.kotlinc.0, &compiled.kotlinc.2)
    );
}

#[test]
fn a_common_subclass_of_an_expect_class_actualized_as_any_matches_kotlinc() {
    let compiled = compile_sources(
        "package p\nexpect open class Lockable()\nexpect open class ThreadLocal<T>() {\n    fun get(): T?\n}\nabstract class Factory<T>(val name: String) : Lockable() {\n    val local: ThreadLocal<T> = ThreadLocal()\n    fun read(): T? = local.get()\n}\nclass Scope : Lockable()\n",
        "package p\nactual typealias Lockable = Any\nactual typealias ThreadLocal<T> = java.lang.ThreadLocal<T>\n",
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        (compiled.krusty.0, &compiled.krusty.2),
        (compiled.kotlinc.0, &compiled.kotlinc.2)
    );
}

#[test]
fn a_common_alias_of_an_alias_actualized_expect_class_stays_the_expect_class_as_kotlinc_says() {
    rejected_in_common(
        "package p\nexpect open class Lock()\ntypealias Local = Lock\nfun h(l: Local) = l.isLocked\n",
        ALIASED_LOCK,
    );
}

#[test]
fn common_aliases_and_bounds_of_an_alias_actualized_expect_class_match_kotlinc() {
    let compiled = compile_sources(
        "package p\nexpect open class Lock()\ntypealias Local = Lock\nfun <T : Lock> f(t: T): T = t\nfun g(l: Local): Local = l\nclass Box<T : Lock>(val t: T)\n",
        ALIASED_LOCK,
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        (compiled.krusty.0, &compiled.krusty.2),
        (compiled.kotlinc.0, &compiled.kotlinc.2)
    );
}

#[test]
fn function_types_projections_and_bounds_naming_an_alias_actualized_expect_class_match_kotlinc() {
    let compiled = compile_sources(
        concat!(
            "package p\nexpect open class Lock()\n",
            "fun a(x: (Lock) -> Lock): Lock.(Lock?) -> Lock? = { it }\n",
            "fun b(y: MutableList<out Lock?>, z: Array<in Lock>): Array<in Lock> = z\n",
            "suspend fun s(f: suspend (Lock) -> Lock): Lock = f(Lock())\n",
            "class Bound<T : Lock, U>(val u: U) where U : Lock?\n",
            "fun <T> c(t: T): T where T : Lock = t\n",
            "val Lock.p: List<Lock>? get() = null\n",
        ),
        concat!(
            "package p\nactual typealias Lock = java.util.concurrent.locks.ReentrantLock\n",
            "fun use(l: Lock, b: Bound<Lock, Lock?>): Boolean = c(l).isLocked && ",
            "a { it }(l, b.u) == null && b(mutableListOf(l), arrayOf<Any>(l)).size == 1 && l.p == null\n",
        ),
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        (compiled.krusty.0, &compiled.krusty.2),
        (compiled.kotlinc.0, &compiled.kotlinc.2)
    );
}

#[test]
fn a_platform_call_of_a_common_extension_of_an_alias_actualized_expect_class_matches_kotlinc() {
    let compiled = compile_sources(
        "package p\nexpect open class Lock()\nfun Lock.q(): Int = 1\nval Lock.p: Int get() = 2\n",
        concat!(
            "package p\nactual typealias Lock = java.util.concurrent.locks.ReentrantLock\n",
            "fun use(l: Lock) = l.q() + l.p\n",
            "fun target(l: java.util.concurrent.locks.ReentrantLock) = l.q()\n",
        ),
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        (compiled.krusty.0, &compiled.krusty.2),
        (compiled.kotlinc.0, &compiled.kotlinc.2)
    );
}

#[test]
fn a_platform_call_of_a_common_generic_extension_of_an_alias_actualized_expect_class_matches_kotlinc(
) {
    let compiled = compile_sources(
        concat!(
            "package p\nexpect open class Ref<V>()\n",
            "fun <T> Ref<T>.peekOrNull(): T? = null\nfun Ref<String>.joined(): String = \"\"\n",
        ),
        concat!(
            "package p\nactual typealias Ref<V> = java.util.concurrent.atomic.AtomicReference<V>\n",
            "fun use(s: Ref<String>): String? = s.peekOrNull() ?: s.joined()\n",
            "fun target(s: java.util.concurrent.atomic.AtomicReference<Int>): Int? = s.peekOrNull()\n",
        ),
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(
        (compiled.krusty.0, &compiled.krusty.2),
        (compiled.kotlinc.0, &compiled.kotlinc.2)
    );
}

#[test]
fn a_classifier_nested_in_an_alias_actualized_expect_class_is_the_targets_nested_class() {
    let compiled = compile_sources(
        "package p\nexpect class Outer() {\n    class Nested()\n}\nfun n(x: Outer.Nested): Outer.Nested = x\n",
        "package p\nclass OuterImpl { class Nested }\nactual typealias Outer = OuterImpl\n",
        common_and_jvm,
    );
    assert_eq!(compiled.kotlinc.0, 0, "kotlinc: {}", compiled.kotlinc.1);
    assert_eq!(compiled.krusty.0, 0, "krusty: {}", compiled.krusty.1);
    // Every class and every method is kotlinc's. The facade's metadata still names the target's
    // nested class where kotlinc keeps the expect's own `Outer.Nested`, so it is compared by name.
    let without_facade = |output: &[(String, Vec<u8>)]| {
        output
            .iter()
            .map(|(path, bytes)| {
                (
                    path.clone(),
                    (!path.ends_with("FirstKt.class")).then_some(bytes.clone()),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        without_facade(&compiled.krusty.2),
        without_facade(&compiled.kotlinc.2)
    );
}
