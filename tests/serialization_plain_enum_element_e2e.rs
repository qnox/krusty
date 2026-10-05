//! A `@Serializable` class property whose type is an enum that is NOT itself `@Serializable`.
//!
//! Such an enum has no generated serializer to reach, so kotlinc's plugin builds one at the use
//! site, in the property's `$childSerializers` slot factory:
//!
//! ```text
//! ldc "<serial name>"; invokestatic E.values(); checkcast [Ljava/lang/Enum;
//! invokestatic EnumsKt.createSimpleEnumSerializer(String, Enum[])
//! ```
//!
//! An entry carrying `@SerialName` selects `createAnnotatedEnumSerializer(name, values, names,
//! entryAnnotations, classAnnotations)` instead, exactly as for a `@Serializable` enum's own
//! accessor. krusty rejected the whole file ("not yet supported by the IR backend") because the
//! element plan only knew how to read a generated enum serializer.
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{member_body, plugin_and_runtime};
use super::serialization_test_support::both_compilers_box;

const SRC: &str = "import kotlinx.serialization.SerialName\n\
                   import kotlinx.serialization.Serializable\n\
                   enum class Kind { A, B }\n\
                   enum class Named { @SerialName(\"x\") X, Y }\n\
                   @Serializable\n\
                   data class Cfg(val type: Kind = Kind.A)\n\
                   @Serializable\n\
                   data class Holder(val n: Named, val k: Kind?, val ks: List<Kind>, val k2: Kind)\n";

/// Members whose bodies carry the element serializers: every slot factory and the class
/// initializer that binds them.
const HOLDER_MEMBERS: &[&str] = &[
    "_childSerializers$_anonymous_()",
    "_childSerializers$_anonymous_$0()",
    "_childSerializers$_anonymous_$1()",
    "_childSerializers$_anonymous_$2()",
    "static {};",
];

fn compare(class: &str, members: &[&str]) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin("PlainEnumElement", SRC, class, &cp, "25", &extra)
        .expect("the reference compiler and javap must be available to this regression");
    for member in members {
        let want = member_body(&built.reference, member);
        assert!(
            want.len() > 1,
            "kotlinc must emit {member} on {class}:\n{}",
            built.reference
        );
        assert_eq!(
            member_body(&built.krusty, member),
            want,
            "{class}.{member} differs from kotlinc"
        );
    }
}

/// A plain enum property: one cached slot whose factory builds `createSimpleEnumSerializer`.
#[test]
fn a_plain_enum_property_builds_its_serializer_like_kotlinc() {
    compare("Cfg", &["_childSerializers$_anonymous_()", "static {};"]);
}

/// `@SerialName` on an entry (annotated factory), a nullable enum (the slot holds the non-null
/// serializer), and an enum as a `List` element (an operand of the constructed list serializer).
#[test]
fn plain_enum_elements_in_every_position_match_kotlinc() {
    compare("Holder", HOLDER_MEMBERS);
    compare("Holder$$serializer", &["childSerializers()"]);
}

/// The values must round-trip under the entries' serial names: a factory that ignored an entry's
/// `@SerialName` would verify and run, and write different data. Both compilers run the identical
/// fixture against the pinned, self-provisioned runtime and must agree.
#[test]
fn plain_enum_elements_round_trip() {
    let src = format!(
        "import kotlinx.serialization.json.Json\n\
         {SRC}\
         fun box(): String {{\n\
         \x20   val value = Holder(Named.X, null, listOf(Kind.A, Kind.B), Kind.B)\n\
         \x20   val text = Json.encodeToString(Holder.serializer(), value)\n\
         \x20   if (text != \"{{\\\"n\\\":\\\"x\\\",\\\"k\\\":null,\\\"ks\\\":[\\\"A\\\",\\\"B\\\"],\\\"k2\\\":\\\"B\\\"}}\") return \"FAIL: \" + text\n\
         \x20   if (Json.decodeFromString(Holder.serializer(), text) != value) return \"FAIL: decode\"\n\
         \x20   val cfg = Json.encodeToString(Cfg.serializer(), Cfg(Kind.B))\n\
         \x20   return if (cfg == \"{{\\\"type\\\":\\\"B\\\"}}\") \"OK\" else \"FAIL: \" + cfg\n\
         }}\n"
    );
    assert_eq!(both_compilers_box(&src, "plain_enum_round_trip"), "OK");
}
