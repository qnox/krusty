//! `KProperty.getDelegate` follows the runtime classpath.
//!
//! The stdlib implementation throws `KotlinReflectionNotSupportedError` unless `kotlin-reflect`
//! is visible. With that jar, and with `kotlin.reflect.jvm.isAccessible` set, the same call
//! returns the property delegate. The box runner keeps those two classpaths apart; these fixtures
//! are the owned regression for that split.

use super::common;

const WITHOUT_REFLECT: &str = "\
import kotlin.reflect.KProperty

class Store : kotlin.properties.ReadOnlyProperty<Any?, String> {
    override fun getValue(thisRef: Any?, property: KProperty<*>) = \"value\"
}

val p: String by Store()

fun box(): String = try {
    ::p.getDelegate()
    \"fail:returned\"
} catch (e: Throwable) {
    val name = e::class.simpleName ?: \"unknown\"
    if (name == \"KotlinReflectionNotSupportedError\") \"OK\" else \"fail:$name\"
}
";

const WITH_REFLECT: &str = "\
import kotlin.reflect.KProperty
import kotlin.reflect.jvm.isAccessible

class Store : kotlin.properties.ReadOnlyProperty<Any?, String> {
    override fun getValue(thisRef: Any?, property: KProperty<*>) = \"value\"
}

val p: String by Store()

fun box(): String = try {
    val ref = ::p
    ref.isAccessible = true
    val delegate = ref.getDelegate()
    if (delegate is Store) \"OK\" else \"fail:${delegate?.let { it::class.simpleName } ?: \"null\"}\"
} catch (e: Throwable) {
    \"fail:${e::class.simpleName ?: \"unknown\"}\"
}
";

fn reflect_jar() -> std::path::PathBuf {
    common::dist_jar("kotlin-reflect.jar")
        .or_else(|| common::find_jar("kotlin-reflect-", &["sources"]))
        .expect("kotlin-reflect.jar from the provisioned kotlinc distribution")
}

#[test]
fn get_delegate_without_reflect_reports_reflection_not_supported() {
    let reference = common::kotlinc_box_result(WITHOUT_REFLECT);
    assert_eq!(reference, "OK", "kotlinc without kotlin-reflect");
    assert_eq!(
        common::Fixture::new().run_box(WITHOUT_REFLECT),
        reference,
        "krusty without kotlin-reflect"
    );
}

#[test]
fn get_delegate_with_reflect_returns_the_delegate() {
    let reflect = reflect_jar();
    let reference =
        common::kotlinc_box_result_with_classpath(WITH_REFLECT, std::slice::from_ref(&reflect));
    assert_eq!(reference, "OK", "kotlinc with kotlin-reflect");
    assert_eq!(
        common::Fixture::new().with_reflect().run_box(WITH_REFLECT),
        reference,
        "krusty with kotlin-reflect"
    );
}
