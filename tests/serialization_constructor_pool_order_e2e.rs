//! The deserialization constructor's local names are interned before its frames.
//!
//! ASM visits `visitLocalVariable` before `visitMaxs`, so kotlinc's constant pool carries a local's
//! name and descriptor ahead of every class constant the frame computation introduces. krusty builds
//! the `StackMapTable` inside `add_method`, which interns each parameter's verification type — so
//! `kotlinx/serialization/internal/SerializationConstructorMarker` landed AHEAD of `seen0` and
//! `serializationConstructorMarker` instead of behind them.
//!
//! The marker's `Class` entry is referenced by nothing in either compiler's output: it exists only
//! because the frame names that parameter's type. Its position was the last non-debug difference on
//! a `@Serializable` class with defaulted properties.

use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, plugin_and_runtime,
};

/// The `Utf8` entries of a `javap -v` pool, in order, filtered to those asked for.
fn pool_order(disassembly: &str, wanted: &[&str]) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter_map(|line| line.split_once("= Utf8"))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| wanted.contains(&value.as_str()))
        .collect()
}

#[test]
fn the_serialization_constructors_locals_intern_before_its_frames() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    // Defaulted properties give the constructor branches, so it HAS frames — without them the
    // stackmap is never built and the ordering under test does not arise.
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Opt2(\n\
               \x20   val a: Int = 1,\n\
               \x20   val b: String? = null,\n\
               )\n";
    let Some(built) =
        compare_with_kotlinc_plugin("SerializationCtorPoolOrder", src, "Opt2", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let wanted = [
        "seen0",
        "serializationConstructorMarker",
        "Lkotlinx/serialization/internal/SerializationConstructorMarker;",
        "kotlinx/serialization/internal/SerializationConstructorMarker",
    ];
    let want = pool_order(&built.reference, &wanted);
    assert_eq!(
        want,
        vec![
            "seen0",
            "serializationConstructorMarker",
            "Lkotlinx/serialization/internal/SerializationConstructorMarker;",
            "kotlinx/serialization/internal/SerializationConstructorMarker",
        ],
        "kotlinc's interning order, stated so a change in the reference is visible here"
    );
    assert_eq!(
        pool_order(&built.krusty, &wanted),
        want,
        "krusty interns the constructor's locals where kotlinc does"
    );
}
