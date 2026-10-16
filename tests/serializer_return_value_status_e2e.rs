//! A generated `$serializer`'s members inherit the return-value status of the runtime declarations
//! they override.
//!
//! kotlinx.serialization 1.11 marks its serializer API must-use: `SerializationStrategy.serialize`,
//! `DeserializationStrategy.deserialize` and `GeneratedSerializer.childSerializers` record
//! `ReturnValueStatus.MustUse` in their `@Metadata`. An override takes the status of the
//! declaration it overrides, and kotlinc's plugin-generated overrides are no exception, so each of
//! them records it too (`Function.flags` 65542 instead of the default 6). krusty recorded nothing
//! for them, which made every generated serializer's `@Metadata` differ from kotlinc's.
//!
//! Against an older core (1.9) the overridden declarations record no status and neither do the
//! overrides; that case is the control.

use std::path::PathBuf;

use super::common::{self, compare_with_kotlinc_plugin};

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Point(val x: Int, val y: String)\n";

fn serialization_core(version: &str) -> PathBuf {
    krusty::toolchain::ensure_maven(
        "org.jetbrains.kotlinx",
        "kotlinx-serialization-core-jvm",
        version,
    )
    .unwrap_or_else(|| {
        panic!(
            "could not provision kotlinx-serialization-core-jvm:{version}; this test must not \
                 self-skip. Check network access or set KRUSTY_DEPS_CACHE."
        )
    })
}

fn assert_serializer_matches_kotlinc(name: &str, core_version: &str) {
    let plugin = common::kotlinc_lib_dir()
        .unwrap_or_else(|| panic!("{name}: no reference compiler lib directory"))
        .join("kotlinx-serialization-compiler-plugin.jar");
    assert!(
        plugin.is_file(),
        "{name}: the reference serialization plugin is missing at {}",
        plugin.display()
    );
    let cp = vec![serialization_core(core_version), common::stdlib_jar()];
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(name, SRC, "Point$$serializer", &cp, "25", &extra)
        .unwrap_or_else(|| panic!("{name}: the reference comparison could not run"));
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{name}: Point$$serializer differs from kotlinc's against core {core_version}\n\
         --- kotlinc ---\n{}\n--- krusty ---\n{}",
        built.reference,
        built.krusty
    );
}

/// Against a must-use runtime, the generated overrides record must-use as kotlinc's do.
#[test]
fn generated_serializer_members_inherit_a_must_use_runtime_status() {
    assert_serializer_matches_kotlinc("SerializerMustUseStatus", "1.11.0");
}

/// Against a runtime that records no status, the generated overrides record none either.
#[test]
fn generated_serializer_members_record_no_status_the_runtime_lacks() {
    assert_serializer_matches_kotlinc("SerializerUnspecifiedStatus", "1.9.0");
}
