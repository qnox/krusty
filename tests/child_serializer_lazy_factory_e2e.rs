//! A cached element serializer is built on FIRST USE, behind its own factory.
//!
//! The point of the `$childSerializers` cache is deferral: a slot holds a `Lazy` over a factory,
//! not an already-built serializer. Building each serializer eagerly and wrapping it with
//! `LazyKt.lazyOf` produces the same values while defeating the deferral the cache exists for, and
//! emits neither the factory nor the `LazyThreadSafetyMode.PUBLICATION` argument kotlinc does:
//!
//! ```text
//! kotlinc: getstatic PUBLICATION; invokedynamic invoke()Function0; invokestatic LazyKt.lazy(…)
//! krusty : <serializer built here>;                                invokestatic LazyKt.lazyOf(…)
//! ```
//!
//! The factory is a private static synthetic bound as a `Function0` through `LambdaMetafactory`,
//! which is what an `IrExpr::Lambda` over that function already compiles to — so this needs no new
//! emitter machinery.

use super::common;
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{gradle_module_jar, plugin_and_runtime};

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Item(val id: Int)\n\
                   @Serializable\n\
                   data class Holder(val count: Int, val items: List<Item>)\n";

#[test]
fn a_cached_serializer_is_built_behind_a_factory() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built =
        compare_with_kotlinc_plugin("ChildSerializerFactory", SRC, "Holder", &cp, "25", &extra)
            .expect("the reference compiler and javap must be available to this regression");
    for (who, text) in [("kotlinc", &built.reference), ("krusty", &built.krusty)] {
        assert!(
            text.contains("_childSerializers$_anonymous_"),
            "{who} must emit the factory:\n{text}"
        );
        assert!(
            text.contains("LazyThreadSafetyMode.PUBLICATION"),
            "{who} must bind the slot with PUBLICATION:\n{text}"
        );
        assert!(
            !text.contains("lazyOf"),
            "{who} must not build the serializer eagerly:\n{text}"
        );
    }
}

/// The cache must still hand back the RIGHT serializer for each slot — a deferral that resolved to
/// the wrong element would compile and then round-trip incorrectly.
#[test]
fn a_lazily_cached_serializer_still_round_trips() {
    const BOX: &str = "import kotlinx.serialization.Serializable\n\
        import kotlinx.serialization.json.Json\n\
        @Serializable\n\
        data class Item(val id: Int)\n\
        @Serializable\n\
        data class Holder(val count: Int, val items: List<Item>)\n\
        fun box(): String {\n\
        \x20   val text = Json.encodeToString(Holder.serializer(), Holder(2, listOf(Item(7))))\n\
        \x20   val back = Json.decodeFromString(Holder.serializer(), text)\n\
        \x20   return if (back == Holder(2, listOf(Item(7)))) \"OK\" else \"FAIL: \" + text\n\
        }\n";
    let Some(json) = gradle_module_jar("org.jetbrains.kotlinx", "kotlinx-serialization-json-jvm")
    else {
        eprintln!("skipping: kotlinx-serialization-json is not in the gradle cache");
        return;
    };
    let (_, mut cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    cp.push(json);
    let jdk = common::jdk_modules();
    cp.push(jdk.clone());
    assert_eq!(
        common::compile_and_run_box(BOX, "Main", &cp, Some(jdk.as_path()))
            .expect("the lazily cached serializer must compile and run"),
        "OK"
    );
}
