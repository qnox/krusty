//! A `super` call to a `suspend` member on the current instance is an ordinary suspension point.
//!
//! The checker records that the selected declaration suspends, lowering keeps that fact on the
//! call, and JVM coroutine lowering passes the continuation and names the member's CPS descriptor
//! on the `invokespecial`. The declaration may be in this file, a sibling file or a dependency,
//! and reached through a class or an interface qualifier. Each fixture runs, and the override's
//! instructions match kotlinc's.

use super::common;

/// `box()` runs `B().f(...)` to completion from a non-suspend caller.
const BUILDER: &str = "import kotlin.coroutines.*\n\
fun builder(c: suspend () -> String): String {\n\
    var result = \"\"\n\
    c.startCoroutine(object : Continuation<String> {\n\
        override val context = EmptyCoroutineContext\n\
        override fun resumeWith(value: Result<String>) { result = value.getOrThrow() }\n\
    })\n\
    return result\n\
}\n";

fn with_builder(source: &str, call: &str) -> String {
    format!("{BUILDER}{source}fun box(): String = builder {{ {call} }}\n")
}

fn assert_runs(source: &str, stem: &str) {
    assert_eq!(
        common::compile_and_run_with_stdlib(source, stem).as_deref(),
        Some("OK"),
        "{stem}"
    );
}

fn assert_method_matches_kotlinc(
    name: &str,
    lib: &[(&str, &str)],
    source: &str,
    class: &str,
    method: &str,
) {
    match common::method_code_diff_against_kotlinc(name, lib, source, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const DIRECT: &str = "suspend fun mark() {}\n\
abstract class A {\n\
    open suspend fun f(): String = \"OK\"\n\
}\n\
class B : A() {\n\
    override suspend fun f(): String {\n\
        val value = super.f()\n\
        mark()\n\
        return value\n\
    }\n\
}\n";

/// An override whose body calls `super.f()` on a suspend member and suspends again after it, so
/// the super call is a suspension point inside the override's own state machine.
#[test]
fn a_direct_super_call_to_a_suspend_member_runs() {
    assert_runs(&with_builder(DIRECT, "B().f()"), "SuspendSuperDirect");
    assert_method_matches_kotlinc(
        "SuspendSuperDirectCode",
        &[],
        DIRECT,
        "B",
        "public java.lang.Object f(",
    );
}

const WITH_PARAMETER: &str = "suspend fun mark() {}\n\
abstract class A {\n\
    open suspend fun f(v: String): String = v\n\
}\n\
class B : A() {\n\
    override suspend fun f(v: String): String {\n\
        val value = super.f(v)\n\
        mark()\n\
        return value\n\
    }\n\
}\n";

/// With a parameter, where the descriptor is visible: the call names
/// `(Ljava/lang/String;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;`.
#[test]
fn a_super_call_with_a_parameter_runs() {
    assert_runs(
        &with_builder(WITH_PARAMETER, "B().f(\"OK\")"),
        "SuspendSuperParameter",
    );
    assert_method_matches_kotlinc(
        "SuspendSuperParameterCode",
        &[],
        WITH_PARAMETER,
        "B",
        "public java.lang.Object f(",
    );
}

const TYPED: &str = "suspend fun mark() {}\n\
interface I {\n\
    suspend fun f(): String = \"OK\"\n\
}\n\
class B : I {\n\
    override suspend fun f(): String {\n\
        val value = super<I>.f()\n\
        mark()\n\
        return value\n\
    }\n\
}\n";

/// The TYPED spelling `super<I>.f()`, which reaches an interface's default body.
#[test]
fn a_typed_super_call_to_an_interface_default_runs() {
    assert_runs(&with_builder(TYPED, "B().f()"), "SuspendSuperTyped");
    assert_method_matches_kotlinc(
        "SuspendSuperTypedCode",
        &[],
        TYPED,
        "B",
        "public java.lang.Object f(",
    );
}

/// The superclass in a SIBLING SOURCE FILE, whose declaration is realized only after lowering.
#[test]
fn a_super_call_across_source_files_runs() {
    let library = "abstract class A {\n\
                   \x20   open suspend fun f(): String = \"O\"\n\
                   }\n";
    let main = with_builder(
        "class B : A() {\n\
         \x20   override suspend fun f(): String = super.f() + \"K\"\n\
         }\n",
        "B().f()",
    );
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&[("A.kt", library), ("Main.kt", &main)])
            .as_deref(),
        Some("OK"),
    );
}

const DEPENDENCY: &str = "abstract class A {\n\
    open suspend fun f(): String = \"OK\"\n\
}\n";

const DEPENDENT: &str = "suspend fun mark() {}\n\
class B : A() {\n\
    override suspend fun f(): String {\n\
        val value = super.f()\n\
        mark()\n\
        return value\n\
    }\n\
}\n";

/// The superclass in a DEPENDENCY compiled by kotlinc, with no source identity here at all.
#[test]
fn a_super_call_into_a_dependency_runs() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(DEPENDENCY, &with_builder(DEPENDENT, "B().f()"),)
            .as_deref(),
        Some("OK"),
    );
    assert_method_matches_kotlinc(
        "SuspendSuperDependencyCode",
        &[("A.kt", DEPENDENCY)],
        DEPENDENT,
        "B",
        "public java.lang.Object f(",
    );
}

const DEPENDENCY_INTERFACE: &str = "interface I {\n\
    suspend fun f(): String = \"OK\"\n\
}\n";

const INTERFACE_DEPENDENT: &str = "suspend fun mark() {}\n\
class B : I {\n\
    override suspend fun f(): String {\n\
        val value = super<I>.f()\n\
        mark()\n\
        return value\n\
    }\n\
}\n";

/// An interface in a dependency compiled by kotlinc under `-jvm-default=<mode>`, and `box()` over
/// it compiled by krusty.
fn run_over_dependency_interface(mode: &str) -> Option<String> {
    let work = common::scratch_dir().expect("allocate the dependency fixture");
    let source = work.join("I.kt");
    let output = work.join("lib");
    std::fs::create_dir_all(&output).expect("create the dependency output");
    std::fs::write(&source, DEPENDENCY_INTERFACE).expect("write the dependency source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        format!("-jvm-default={mode}"),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        code, 0,
        "kotlinc failed under -jvm-default={mode}: {stderr}"
    );
    let result = common::compile_and_run_box(
        &with_builder(INTERFACE_DEPENDENT, "B().f()"),
        "Main",
        &[output, common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    let _ = std::fs::remove_dir_all(work);
    result
}

/// A dependency interface's default body, as an interface default method.
#[test]
fn a_typed_super_call_into_a_dependency_interface_runs() {
    assert_eq!(
        run_over_dependency_interface("enable").as_deref(),
        Some("OK")
    );
    assert_method_matches_kotlinc(
        "SuspendSuperDependencyInterfaceCode",
        &[("I.kt", DEPENDENCY_INTERFACE)],
        INTERFACE_DEPENDENT,
        "B",
        "public java.lang.Object f(",
    );
}

/// The same body realized on the receiver-first `I$DefaultImpls.f(I, Continuation)` static, as
/// `-jvm-default=disable` compiles it: the super call is a direct static call, not `invokespecial`.
/// The interface method itself is abstract there, so an `invokespecial` on it could not run.
#[test]
fn a_typed_super_call_into_a_default_impls_holder_runs() {
    assert_eq!(
        run_over_dependency_interface("disable").as_deref(),
        Some("OK")
    );
}
