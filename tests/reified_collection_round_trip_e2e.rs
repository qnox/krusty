//! A reified `decodeFromString`/`encodeToString` whose type argument is a COLLECTION of a
//! `@Serializable` class.
//!
//! The plugin plans these round-trip calls only when the type argument is itself an annotated
//! classifier, so `fmt.decodeFromString<List<Twig>>(text)` fell through to the generic inline
//! splice. That path cannot reify the kotlinx "magic API" the library body uses to obtain
//! `serializer<T>()`, and it failed two different ways: the `Json` receiver bailed the whole file
//! with `inline splice failed`, and the `StringFormat` receiver SPLICED the body and left the
//! marker behind —
//!
//! ```text
//! aconst_null
//! ldc           // String kotlinx.serialization.serializer.withModule
//! invokestatic  // MagicApiIntrinsics.voidMagicApiCall
//! ```
//!
//! which compiles and then throws when the method runs. The emitted marker is asserted against
//! directly: it is the difference between a miscompile and a correct method, and no byte
//! comparison of a differing method would name it.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   import kotlinx.serialization.StringFormat\n\
                   import kotlinx.serialization.decodeFromString\n\
                   import kotlinx.serialization.encodeToString\n\
                   @Serializable\n\
                   data class Twig(val id: Int)\n\
                   fun parse(fmt: StringFormat, text: String): List<Twig> =\n\
                   \u{20}   fmt.decodeFromString<List<Twig>>(text)\n\
                   fun write(fmt: StringFormat, twigs: List<Twig>): String =\n\
                   \u{20}   fmt.encodeToString<List<Twig>>(twigs)\n";

fn built() -> (String, String) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(
        "ReifiedCollectionRoundTrip",
        SRC,
        "ReifiedCollectionRoundTripKt",
        &cp,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    (built.reference, built.krusty)
}

/// The members a method touches, in order, with pool indices dropped.
fn members(disassembly: &str, method: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .skip_while(|line| !line.contains(&format!(" {method}(")))
        .skip(1)
        .take_while(|line| {
            !line.contains("public static final") || line.contains(&format!(" {method}("))
        })
        .filter_map(|line| line.split("// ").nth(1).map(str::to_string))
        .collect()
}

#[test]
fn a_collection_round_trip_leaves_no_magic_api_marker() {
    let (reference, krusty) = built();
    assert!(
        !reference.contains("MagicApiIntrinsics"),
        "the reference resolves the magic API at compile time:\n{reference}"
    );
    assert!(
        !krusty.contains("MagicApiIntrinsics"),
        "krusty left an unreified magic-API marker in the emitted body:\n{krusty}"
    );
    assert!(
        !krusty.contains("reifiedOperationMarker"),
        "krusty left an unreified operation marker in the emitted body:\n{krusty}"
    );
}

#[test]
fn a_list_type_argument_constructs_the_element_serializer() {
    let (reference, krusty) = built();
    for method in ["parse", "write"] {
        let want = members(&reference, method);
        assert!(
            want.iter()
                .any(|row| row.contains("ArrayListSerializer.\"<init>\"")),
            "the reference constructs ArrayListSerializer in {method}:\n{want:#?}"
        );
        let got = members(&krusty, method);
        // Building the list serializer is this change's contract; CONSTRUCTING it rather than
        // fetching it from the stdlib factory is #1062's, and the two serialize identically.
        // `ListSerializer(x)` is an inline stdlib function over `ArrayListSerializer(x)`, so both
        // spellings satisfy this; #1062 narrows it to the construction the reference emits.
        assert!(
            got.iter().any(|row| {
                row.contains("ArrayListSerializer.\"<init>\"")
                    || row.contains("BuiltinSerializersKt.ListSerializer")
            }),
            "krusty must build the list serializer in {method}:\n{got:#?}"
        );
        assert!(
            got.iter()
                .any(|row| row.contains("Twig$Companion.serializer")
                    || row.contains("Twig$$serializer")),
            "krusty must pass the element serializer in {method}:\n{got:#?}"
        );
    }
}

/// The decoded result is erased `Object` on the member ABI, so the call site must cast it back.
#[test]
fn a_decoded_collection_is_cast_back_to_its_type() {
    let (_, krusty) = built();
    let got = members(&krusty, "parse");
    assert!(
        got.iter()
            .any(|row| row.contains("StringFormat.decodeFromString")),
        "krusty must call the two-argument member:\n{got:#?}"
    );
    assert!(
        got.iter().any(|row| row.contains("class java/util/List")),
        "krusty must cast the erased result back to List:\n{got:#?}"
    );
}
