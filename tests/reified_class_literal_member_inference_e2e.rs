//! A reified extension delegating to a generic member with `T::class`.
//!
//! Shape (service-locator APIs): `interface Registry { fun <T : Any> services(type: KClass<T>):
//! List<T> }` with `inline fun <reified T : Any> Registry.services(): List<T> = services(T::class)`.
//! The member's formal `T` must bind to the extension's reified `T` through the `KClass<T>`
//! argument; failing to unify leaves the member's own formal in the return type and the extension
//! body fails with "return type mismatch: expected 'List<T (of fun services)>', actual 'List<T>'"
//! — both spellings naming the same source type. A plain `KClass<R>` PARAMETER already binds
//! (probed); the `T::class` literal channel did not.
use super::common;

fn run_box(src: &str) -> String {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    common::expect_box_run(src, "Main", &[sl], Some(jdk.as_path()))
}

#[test]
fn reified_extension_binds_member_formal_through_class_literal() {
    const SRC: &str = "import kotlin.reflect.KClass\n\
        class Provider<T>(val name: String)\n\
        class Registry {\n\
            fun <T : Any> services(type: KClass<T>): List<T> = emptyList()\n\
            fun <T : Any> provider(type: KClass<T>): Provider<T> = Provider(type.simpleName ?: \"?\")\n\
        }\n\
        inline fun <reified T : Any> Registry.services(): List<T> = services(T::class)\n\
        inline fun <reified T : Any> Registry.provider(): Provider<T> = provider(T::class)\n\
        fun box(): String {\n\
            val r = Registry()\n\
            val s: List<String> = r.services()\n\
            val p: Provider<String> = r.provider()\n\
            return if (s.isEmpty() && p.name == \"String\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run_box(SRC), "OK");
}

#[test]
fn nested_reified_extension_chain_never_miscompiles() {
    // A reified extension calling ANOTHER reified extension on the IMPLICIT receiver. The nested
    // splice may decline, but the fallback must never emit the local facade — that body embeds the
    // `ldc T` name marker and dies at runtime with NoClassDefFoundError. Either the chain splices
    // fully (box() == "OK") or the backend bails the file (the helper panics on a bail, which is
    // the honest failure — a silent wrong-code jar is the one forbidden outcome).
    const SRC: &str = "import kotlin.reflect.KClass\n\
        class Registry { fun <T : Any> services(type: KClass<T>): List<T> = emptyList() }\n\
        inline fun <reified T : Any> Registry.services(): List<T> = services(T::class)\n\
        inline fun <reified T : Any> Registry.firstName(): String? = services<T>().firstOrNull()?.let { it::class.simpleName }\n\
        fun box(): String {\n\
            val missing: String? = Registry().firstName<String>()\n\
            return if (missing == null) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run_box(SRC), "OK");
}

#[test]
fn reified_top_level_binds_member_formal_through_class_literal() {
    // The same binding through a NON-extension reified function, so the fix is not keyed to the
    // extension-receiver channel.
    const SRC: &str = "import kotlin.reflect.KClass\n\
        class Registry { fun <T : Any> one(type: KClass<T>): T? = null }\n\
        inline fun <reified T : Any> fetch(r: Registry): T? = r.one(T::class)\n\
        fun box(): String = if (fetch<String>(Registry()) == null) \"OK\" else \"fail\"\n";
    assert_eq!(run_box(SRC), "OK");
}

#[test]
fn krusty_dependency_preserves_reified_extension_binding() {
    const LIB: &str = "package lib\n\
        import kotlin.reflect.KClass\n\
        class Provider<T>(val name: String)\n\
        class Registry {\n\
            fun <T : Any> provider(type: KClass<T>): Provider<T> =\n\
                Provider(type.simpleName ?: \"?\")\n\
        }\n\
        inline fun <reified T : Any> Registry.provider(): Provider<T> = provider(T::class)\n";
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
            val provider: Provider<String> = Registry().provider()\n\
            return if (provider.name == \"String\") \"OK\" else provider.name\n\
        }\n";
    assert_eq!(
        common::expect_box_run_against("reified-extension-dependency", LIB, MAIN).as_deref(),
        Some("OK")
    );
}
