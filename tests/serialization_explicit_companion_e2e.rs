//! A `@Serializable` class that declares its own companion object.
//!
//! kotlinc's serialization plugin adds the generated `serializer()` accessor to that companion
//! instead of synthesizing one. krusty declared the accessor only on a companion it synthesized, so
//! a class with a hand-written companion reached the backend without the declaration and the
//! compiler panicked.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;
use super::serialization_test_support::both_compilers_box;

#[test]
fn a_serializable_class_with_its_own_companion_round_trips() {
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable
class Token(val value: String) {
    companion object {
        fun make(): Token = Token("made")
    }
}

@JvmInline
@Serializable
value class Handle(val value: String) {
    companion object {
        operator fun invoke(): Handle = Handle("fresh")
    }
}

fun box(): String {
    val token = Json.encodeToString(Token.serializer(), Token.make())
    val back = Json.decodeFromString(Token.serializer(), """{"value":"in"}""").value
    val handle = Json.encodeToString(Handle.serializer(), Handle())
    return "$token|$back|$handle"
}
"#;
    assert_eq!(
        both_compilers_box(src, "serializable_explicit_companion"),
        "{\"value\":\"made\"}|in|\"fresh\""
    );
}

#[test]
fn the_serializer_of_a_class_with_a_hand_written_companion_matches_kotlinc() {
    let (plugin, cp) =
        plugin_and_runtime().expect("the serialization plugin and runtime are provisioned");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               class Token(val value: String) {\n\
               \x20   companion object {\n\
               \x20       fun make(): Token = Token(\"made\")\n\
               \x20   }\n\
               }\n";
    for class in ["Token$$serializer"] {
        let built = compare_with_kotlinc_plugin("ExplicitCompanion", src, class, &cp, "25", &extra)
            .expect("reference kotlinc and javap are provisioned");
        if built.krusty_bytes != built.reference_bytes {
            // The first lines name the scratch file and its checksum; the class itself follows.
            let body = |text: &str| text.lines().skip(3).map(str::to_string).collect::<Vec<_>>();
            assert_eq!(body(&built.krusty), body(&built.reference), "{class}");
            panic!("{class}: same disassembly, different bytes");
        }
    }
}
