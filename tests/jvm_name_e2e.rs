use super::common;

#[test]
fn cross_file_jvm_name_is_realized_after_stable_frontend_selection() {
    let sources = [
        (
            "Declaration.kt",
            r#"import kotlin.jvm.JvmName

               @JvmName("physicalSource")
               fun source(value: String): String = value"#,
        ),
        (
            "Use.kt",
            r#"fun box(): String {
                   val reference = ::source
                   return source("O") + reference("K")
               }"#,
        ),
    ];

    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK")
    );
}

#[test]
fn cross_module_member_jvm_name_keeps_its_kotlin_declaration_name() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(library) = common::compile_lib(
        "member_jvm_name",
        r#"package lib
           open class A {
               @JvmName("physicalF")
               fun <T> f(value: T, fallback: Int = 1): T = value
           }"#,
    ) else {
        return;
    };
    let output = common::compile_and_run_box(
        r#"import lib.A
           fun box(): String = A().f("OK")"#,
        "Main",
        &[library, stdlib, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(output.as_deref(), Some("OK"));
}

// PLATFORM-SPECIFIC: `@JvmName` is JVM-only (the klib lanes reject it outside common sources), so the
// original non-null `Any`-bound program lives in this JVM-owned leaf. The portable regression
// `genericOverloadsDifferByParameterFormal.kt` covers a nominal bound and a transitive bound; a
// non-null `Any` bound is a different constraint, and the two overloads here differ only by the
// JVM name that lets them coexist.
#[test]
fn generic_overloads_differing_by_parameter_formal_with_any_bound_select_the_constrained_one() {
    const SOURCE: &str = r####"import kotlin.jvm.JvmName

class Failure(val code: Int)

// Two generic overloads whose only difference is which formal their parameter passes through:
// `L : Any` is the more specific one, whatever its declaration ordinal.
class Box<out L : Any, out R> private constructor(val value: Any?) {
    companion object {
        fun <L : Any, R> make(value: Any?): Box<L, R> = Box(value)

        operator fun <L : Any, R> invoke(left: L): Box<L, R> = make(left)

        @JvmName("right1")
        operator fun <L : Any, R> invoke(right: R): Box<L, R> = make(right)
    }
}

class Factory {
    fun <L : Any, R> create(left: L): Box<L, R> = Box.make(left)

    @JvmName("createRight")
    fun <L : Any, R> create(right: R): Box<L, R> = Box.make(right)
}

fun <T : Any> viaCompanion(): Box<Failure, T> = Box(Failure(1))

fun <T : Any> viaMember(): Box<Failure, T> = Factory().create(Failure(2))

fun box(): String {
    val companion = viaCompanion<String>().value
    if (companion !is Failure || companion.code != 1) return "companion invoke: $companion"
    val member = viaMember<String>().value
    if (member !is Failure || member.code != 2) return "member: $member"
    return "OK"
}
"####;
    common::expect_box_same_as_kotlinc(SOURCE, "generic_overloads_any_bound_jvm_name");
}
