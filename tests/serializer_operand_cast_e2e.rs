//! The serializer operand of a `CompositeDecoder`/`CompositeEncoder` element call is NARROWED to
//! the parameter type.
//!
//! `decodeSerializableElement` takes a `DeserializationStrategy` and `encodeSerializableElement` a
//! `SerializationStrategy`. Every serializer krusty passes already implements both — a generated
//! `Foo$$serializer` and a builtin `StringSerializer` alike — so the verifier accepts the call
//! without a cast and krusty emitted none. kotlinc emits one unconditionally.
//!
//! Measured against kotlinc 2.4.10: the cast is there for a builtin singleton and a generated
//! serializer, for the nullable and non-nullable call on each side. It is three bytes plus its
//! pool entries at every element of every generated serializer, which is why it is worth its own
//! regression rather than being folded into a larger comparison.
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, plugin_and_runtime,
};

/// A builtin singleton, a generated serializer, and a nullable of each — so a rule that fired only
/// for one shape cannot pass.
const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Twig(val id: Int)\n\
                   @Serializable\n\
                   data class Bough(val maybe: String?, val twig: Twig, val maybeTwig: Twig?)\n";

const NULLABLE_PROPERTY_SRC: &str = "import kotlinx.serialization.Serializable\n\
                                     @Serializable\n\
                                     data class Leaf(val id: Int)\n\
                                     @Serializable\n\
                                     data class Twig(val id: Int)\n\
                                     @Serializable\n\
                                     data class Bough(\n\
                                         val maybeLeaf: Leaf?,\n\
                                         val twig: Twig,\n\
                                         val maybeTwig: Twig?,\n\
                                     )\n";

/// A NULLABLE property publishes the `.nullable` serializer, narrowed to the `KSerializer` that
/// wrapper takes.
///
/// Only the explicit-`@Serializable(with = …)` arm wrapped, so every nullable property whose
/// serializer is DERIVED from its type — a builtin, a nested class — published the NON-NULL
/// serializer from `childSerializers()`. The whole method is compared, so the wrap, the narrowing
/// and the slots that must NOT be wrapped are all pinned together.
#[test]
fn a_nullable_property_publishes_its_nullable_serializer() {
    let (reference, krusty) = built_from(NULLABLE_PROPERTY_SRC, "Bough$$serializer");
    let method = |text: &str| {
        text.lines()
            .map(str::trim)
            .skip_while(|line| !line.contains("childSerializers()"))
            .take_while(|line| !line.contains("typeParametersSerializers"))
            .map(|line| {
                let mut out = String::new();
                let mut rest = line;
                while let Some(at) = rest.find('#') {
                    out.push_str(&rest[..at]);
                    out.push('#');
                    rest = rest[at + 1..].trim_start_matches(|c: char| c.is_ascii_digit());
                }
                out.push_str(rest);
                out
            })
            .collect::<Vec<_>>()
    };
    let want = method(&reference);
    assert!(
        want.iter()
            .filter(|row| row.contains("getNullable"))
            .count()
            == 2,
        "the reference wraps exactly the two nullable properties:\n{want:#?}"
    );
    assert_eq!(
        method(&krusty),
        want,
        "krusty must publish the same element serializers"
    );
}

fn built(class: &str) -> (String, String) {
    built_from(SRC, class)
}

fn built_from(source: &str, class: &str) -> (String, String) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built =
        compare_with_kotlinc_plugin("SerializerOperandCast", source, class, &cp, "25", &extra)
            .expect("the reference compiler and javap must be available to this regression");
    (built.reference, built.krusty)
}

/// Each `(serializer operand, element call)` pair a method makes, as `(cast?, call)`.
fn operand_casts(disassembly: &str, direction: &str) -> Vec<(bool, String)> {
    let rows: Vec<&str> = disassembly.lines().map(str::trim).collect();
    let mut out = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        let Some(call) = row.split("// InterfaceMethod ").nth(1) else {
            continue;
        };
        // `decodeSerializableElement` and `decodeNullableSerializableElement` both end in it, and
        // the calls that take no serializer at all (`decodeSequentially`, `decodeElementIndex`) do
        // not.
        if !call.contains("SerializableElement") || !call.contains(direction) {
            continue;
        }
        // The operand is pushed a few instructions back; the cast, when present, is the one that
        // names the strategy interface.
        let cast = rows[at.saturating_sub(4)..at]
            .iter()
            .any(|earlier| earlier.contains("checkcast") && earlier.contains("Strategy"));
        out.push((cast, call.split(':').next().unwrap_or(call).to_string()));
    }
    out
}

#[test]
fn a_decoded_element_narrows_its_serializer_to_the_parameter_type() {
    let (reference, krusty) = built("Bough$$serializer");
    let want = operand_casts(&reference, "decode");
    assert!(
        want.len() >= 3 && want.iter().all(|(cast, _)| *cast),
        "the reference narrows every decode operand:\n{want:#?}"
    );
    assert_eq!(
        operand_casts(&krusty, "decode"),
        want,
        "krusty must narrow each one the same way"
    );
}

#[test]
fn an_encoded_element_narrows_its_serializer_to_the_parameter_type() {
    let (reference, krusty) = built("Bough");
    let want = operand_casts(&reference, "encode");
    assert!(
        want.len() >= 3 && want.iter().all(|(cast, _)| *cast),
        "the reference narrows every encode operand:\n{want:#?}"
    );
    assert_eq!(
        operand_casts(&krusty, "encode"),
        want,
        "krusty must narrow each one the same way"
    );
}
