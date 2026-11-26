//! A Java `?` argument for an F-bounded self-type parameter (`AbstractThrowableAssert<?, ? extends
//! Throwable>`, AssertJ's fluent API) is bounded by its declaration, so a member returning `SELF`
//! still exposes the class's own members, as in kotlinc.

use std::path::PathBuf;

use super::common;

const BASE: &str = "public abstract class Base<SELF extends Base<SELF, ACTUAL>, ACTUAL> {\n\
    \x20   public SELF isInstanceOf(Class<?> type) { return null; }\n\
    }\n";

const MID: &str = "public abstract class Mid<SELF extends Mid<SELF, ACTUAL>, ACTUAL> extends Base<SELF, ACTUAL> {}\n";

const THROWABLE: &str = "public class ThrowableAssert<SELF extends ThrowableAssert<SELF, ACTUAL>, ACTUAL extends Throwable>\n\
    \x20   extends Mid<SELF, ACTUAL> {\n\
    \x20   public SELF hasMessage(String message) { return null; }\n\
    \x20   public static ThrowableAssert<?, ? extends Throwable> assertThatThrownBy() { return null; }\n\
    }\n";

const USE: &str = "fun inherited() {\n\
    ThrowableAssert.assertThatThrownBy().isInstanceOf(String::class.java).hasMessage(\"m\")\n\
    }\n\
    fun declared() {\n\
    ThrowableAssert.assertThatThrownBy().hasMessage(\"m\").hasMessage(\"n\")\n\
    }\n";

fn classpath() -> Vec<PathBuf> {
    let sources = [
        ("Base.java".to_string(), BASE.to_string()),
        ("Mid.java".to_string(), MID.to_string()),
        ("ThrowableAssert.java".to_string(), THROWABLE.to_string()),
    ];
    let (library, _) =
        common::javac_compile(&sources, &[]).expect("javac must compile the self-type fixture");
    vec![library, common::stdlib_jar()]
}

#[test]
fn a_java_wildcard_self_type_keeps_its_declared_bound_like_kotlinc() {
    let classpath = classpath();
    let classes = common::classes_against_kotlinc_module_cp(&[("Use.kt", USE)], &classpath[..1]);
    assert_eq!(classes.differences(), Vec::<String>::new());

    let jdk = common::jdk_modules();
    let compiled = common::compile_in_process(USE, "Use", &classpath, Some(jdk.as_path()));
    assert!(
        compiled.is_some(),
        "the accepted fixture must lower and emit: {:?}",
        common::front_end_diagnostics(USE, &classpath, Some(jdk.as_path()))
    );
}

/// A JDK `Class<?>` is bounded by `Class`'s own `T`, which Java declares without a bound, so the
/// wildcard reads as `Any!` and satisfies the `T : Any` of `Class<T>.kotlin` and of a source
/// extension with the same bound, as in kotlinc. The star's placeholder bound `Any?` does not.
const CLASS_WILDCARD: &str = r#"
import kotlin.reflect.KClass

fun <T : Any> Class<T>.declared(): KClass<T> = kotlin
val <T : Any> Class<T>.declaredName: String get() = simpleName

fun loaded(): KClass<*> = Class.forName("kotlin.Unit").kotlin
fun viaLoader(loader: ClassLoader): KClass<*> = loader.loadClass("kotlin.Unit")!!.kotlin
fun viaExtension(loader: ClassLoader) = loader.loadClass("kotlin.Unit").declared()
fun viaProperty(loader: ClassLoader) = loader.loadClass("kotlin.Unit").declaredName

fun box(): String {
    val loader = Unit::class.java.classLoader
    if (loaded() != Unit::class || viaLoader(loader) != Unit::class) return "kotlin"
    if (viaExtension(loader) != Unit::class) return "extension"
    return if (viaProperty(loader) == "Unit") "OK" else "property"
}
"#;

/// The program resolves and runs as kotlinc's does. Its classes are not yet kotlinc's: an extension
/// property read on a platform receiver lacks kotlinc's `checkNotNullExpressionValue`, and the
/// result type inferred from the star is `KClass<*>` where kotlinc approximates `KClass<out Any>`.
#[test]
fn a_jdk_class_wildcard_satisfies_a_non_null_receiver_bound_like_kotlinc() {
    common::expect_box_same_as_kotlinc(CLASS_WILDCARD, "ClassWildcard");
}
