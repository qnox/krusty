//! A class implementing a dependency interface whose functions take a value class. The dependency
//! stores those functions under their mangled JVM names (`get-<hash>`); the implementation must
//! declare the same name once, not mangle the already-mangled name a second time. An unsigned
//! parameter and a value-class extension receiver mangle the name as a value class does.
use super::common;

const LIBRARY: &str = r#"package lib

import kotlin.reflect.KClass

@JvmInline value class Key(val value: String)

interface Registry {
    fun <T : Any> get(key: Key, type: KClass<T>): T
    fun all(key: Key): List<String>
    fun count(limit: UInt): Int
    fun Key.length(): Int
    val size: Int
}

fun read(registry: Registry, key: Key): String = with(registry) {
    get(key, String::class) + all(key).joinToString() + size + count(3u) + key.length()
}
"#;

const CONSUMER: &str = r#"import lib.*
import kotlin.reflect.KClass

class MapRegistry : Registry {
    @Suppress("UNCHECKED_CAST")
    override fun <T : Any> get(key: Key, type: KClass<T>): T = key.value as T
    override fun all(key: Key): List<String> = listOf(key.value, key.value)
    override fun count(limit: UInt): Int = limit.toInt()
    override fun Key.length(): Int = value.length
    override val size: Int get() = 2
}

fun box(): String {
    val text = read(MapRegistry(), Key("k"))
    return if (text == "kk, k231") "OK" else "fail: $text"
}
"#;

#[test]
fn an_implementation_of_a_dependency_value_class_function_runs() {
    assert_eq!(
        common::expect_box_run_against("ClasspathValueClassOverrideLibrary", LIBRARY, CONSUMER)
            .as_deref(),
        Some("OK")
    );
}

#[test]
fn an_implementation_of_a_dependency_value_class_function_is_byte_identical_to_kotlinc() {
    let library =
        common::compile_libs_build("ClasspathValueClassOverrideLibrary", &[("Lib.kt", LIBRARY)])
            .expect("krusty builds the dependency");
    let reference = library
        .reference_out()
        .expect("reference kotlinc builds the dependency")
        .to_path_buf();
    common::assert_classes_identical_to_kotlinc_over_own_libraries(
        "ClasspathValueClassOverride",
        CONSUMER,
        &["MapRegistry", "ClasspathValueClassOverrideKt"],
        &[reference],
        &[library.krusty_out().to_path_buf()],
    );
}
