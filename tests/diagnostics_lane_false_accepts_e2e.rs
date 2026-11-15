//! Programs the diagnostics lane must reject because kotlinc rejects them. Each one is a case
//! the 2.4.21 false-accept list does not excuse.

use super::common;

#[test]
fn a_supertype_typealias_constructor_does_not_infer_type_arguments() {
    let source = "\
open class Ref<T>(var x: T)\n\
typealias R<T> = Ref<T>\n\
class Test1 : R(0)\n\
class Test2 : R<Int>(0)\n";
    let sources = [("alias.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
}

#[test]
fn a_supertype_class_constructor_does_not_infer_type_arguments() {
    let source = "\
open class Box<T>(var x: T)\n\
class Test : Box(0)\n";
    let sources = [("classctor.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
}

#[test]
fn star_imports_of_the_same_name_are_ambiguous() {
    let sources = [
        (
            "first/KtNodeTypes.java",
            "package first;\n\
public interface KtNodeTypes {\n\
    String SOME = \"Some\";\n\
}\n",
        ),
        (
            "SomeEnum.kt",
            "package second\n\
enum class SomeEnum {\n\
    SOME;\n\
}\n",
        ),
        (
            "OtherEnum.kt",
            "package third\n\
enum class SomeEnum {\n\
    SOME;\n\
}\n",
        ),
        (
            "test.kt",
            "import first.KtNodeTypes.*\n\
import second.SomeEnum.*\n\
import third.SomeEnum.*\n\
fun test(arg: String): Boolean {\n\
    return when (arg) {\n\
        SOME -> true\n\
        else -> false\n\
    }\n\
}\n",
        ),
    ];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
}

#[test]
fn a_spread_array_of_mixed_elements_does_not_fit_a_string_vararg_annotation() {
    let source = "\
annotation class B(vararg val args: String)\n\
@B(*arrayOf(1, \"b\"))\n\
fun test() {}\n";
    let sources = [("ann.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
}

#[test]
fn a_spread_array_of_mixed_elements_does_not_fit_a_java_string_array_annotation() {
    let sources = [
        (
            "A.java",
            "public @interface A {\n    String[] value();\n}\n",
        ),
        ("b.kt", "@A(*arrayOf(1, \"b\"))\nfun test() {}\n"),
    ];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
}

#[test]
fn a_collection_literal_is_ambiguous_between_a_mutable_sibling_and_the_expected_set() {
    let source = "\
// LANGUAGE: +CollectionLiterals, +UnnamedLocalVariables\n\
fun <T> select(vararg x: T): T = x[0]\n\
fun ambiguities() {\n\
    val _: Set<*> = select(mutableSetOf(), [{}])\n\
    val _: Set<*> = select(mutableSetOf(), [{ x: Int -> }])\n\
    val _: Set<*> = select(mutableSetOf(), [{ x -> }])\n\
}\n\
fun noAmbiguities() {\n\
    val _: Set<*> = select(setOf(), [{}])\n\
    val _: Set<*> = select(setOf(), [{ x: Int -> }])\n\
    val _: Set<*> = select(setOf(), [{ x -> }])\n\
    val _: Set<(Int) -> Unit> = select(setOf(), [{ x -> }])\n\
}\n";
    let sources = [("cl.kt", source)];
    let reference_args = common::language_directives::kotlinc_args(source);
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn a_java_box_of_null_stays_nullable_nothing_when_precise_simplification_is_off() {
    let kotlin = "\
// LANGUAGE: -PreciseSimplificationToFlexibleLowerConstraint\n\
import java.util.function.Supplier\n\
fun test() {\n\
    Supplier<String> { JavaBox(null).a }\n\
    val sam: Supplier<String> = Supplier { JavaBox(null).a }\n\
    val sam2 = object : Supplier<String> {\n\
        override fun get() = JavaBox(null).a\n\
    }\n\
}\n";
    let sources = [
        (
            "JavaBox.java",
            "public class JavaBox<T> {\n\
    public JavaBox(T b) { a = b; }\n\
    public T a;\n\
}\n",
        ),
        ("Test.kt", kotlin),
    ];
    let reference_args = common::language_directives::kotlinc_args(kotlin);
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn an_actual_typealias_does_not_match_an_atomic_int_to_a_java_atomic_integer() {
    let work = common::scratch_dir().expect("scratch");
    let common_path = work.join("common.kt");
    let platform_path = work.join("test.kt");
    let java_path = work.join("Bar.java");
    std::fs::write(
        &common_path,
        "import kotlin.concurrent.atomics.AtomicInt\n\
import kotlin.concurrent.atomics.ExperimentalAtomicApi\n\
\n\
@OptIn(ExperimentalAtomicApi::class)\n\
expect class Foo {\n\
    fun test(a: AtomicInt): AtomicInt\n\
}\n",
    )
    .expect("write common");
    std::fs::write(&platform_path, "actual typealias Foo = Bar\n").expect("write platform");
    std::fs::write(
        &java_path,
        "import java.util.concurrent.atomic.AtomicInteger;\n\
public class Bar {\n\
    public AtomicInteger test(AtomicInteger i) { return i; }\n\
}\n",
    )
    .expect("write java");
    let arguments = [
        "-Xmulti-platform".to_string(),
        "-Xfragments=m1-common".to_string(),
        format!("-Xfragment-sources=m1-common:{}", common_path.display()),
        "-Xfragments=m2-jvm".to_string(),
        format!("-Xfragment-sources=m2-jvm:{}", platform_path.display()),
        "-Xfragment-refines=m2-jvm:m1-common".to_string(),
        "-d".to_string(),
        work.join("out").display().to_string(),
        common_path.display().to_string(),
        platform_path.display().to_string(),
        java_path.display().to_string(),
    ];
    let (_, reference_stderr) =
        common::kotlinc_compile(&arguments).expect("the reference kotlinc is available");
    let krusty = std::process::Command::new(common::krusty_binary())
        .args(&arguments)
        .output()
        .expect("run krusty");
    let krusty_stderr = String::from_utf8_lossy(&krusty.stderr);
    assert_eq!(
        common::reference_error_blocks_from(&krusty_stderr),
        common::reference_error_blocks_from(&reference_stderr)
    );
    let _ = std::fs::remove_dir_all(work);
}
