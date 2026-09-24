//! Focused JVM realization checks for the generated serializer's semantic default-member call.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{member_body, plugin_and_runtime};

#[test]
fn non_generic_serializer_delegates_type_parameter_serializers_to_the_default() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Point(val x: Int, val y: String)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "InheritedTypeParameterSerializers",
        src,
        "Point$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = member_body(&built.reference, "typeParametersSerializers()");
    assert!(
        want.iter().any(|line| line.contains("invokespecial")),
        "kotlinc delegates to the interface default: {want:?}"
    );
    assert_eq!(
        member_body(&built.krusty, "typeParametersSerializers()"),
        want,
        "a non-generic serializer's typeParametersSerializers"
    );
}

#[test]
fn generic_serializer_returns_its_recorded_type_parameter_serializers() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Boxed<T>(val first: T, val second: String)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "GenericTypeParameterSerializers",
        src,
        "Boxed$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = member_body(&built.reference, "typeParametersSerializers()");
    assert!(
        want.iter().any(|line| line.contains("getfield"))
            && !want.iter().any(|line| line.contains("invokespecial")),
        "kotlinc reads the serializer's recorded type parameters: {want:?}"
    );
    assert_eq!(
        member_body(&built.krusty, "typeParametersSerializers()"),
        want,
        "a generic serializer's typeParametersSerializers"
    );
}

/// A terminal integer switch in the generated pre-test decode loop branches directly to the loop
/// header. Common IR states only the semantic switch; this exact member comparison pins the JVM
/// backend's generic fallthrough threading without adding layout-only `Continue` expressions.
#[test]
fn deserialize_terminal_switch_matches_kotlinc() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Point(val x: Int, val y: String)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "TerminalDeserializeSwitch",
        src,
        "Point$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let member = "deserialize(kotlinx.serialization.encoding.Decoder)";
    let want = member_body(&built.reference, member);
    assert!(
        want.iter().any(|line| line.contains("tableswitch")),
        "reference deserialize uses one integer switch: {want:?}"
    );
    assert_eq!(
        member_body(&built.krusty, member),
        want,
        "deserialize terminal switch layout"
    );
}
