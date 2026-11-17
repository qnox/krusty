//! `@UseSerializers(S::class)` on a file makes every occurrence of the type `S`
//! serves, in a serializable property's declared type, use `S`, as if it were written
//! `@Serializable(with = S::class)`. Each program runs under kotlinc as well.

use super::serialization_test_support::both_compilers_box;

const IMPORTS: &str = r#"import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.UseSerializers
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json
"#;

const SERVED: &str = r#"
class Token(val raw: String)

object TokenAsString : KSerializer<Token> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor("Token", PrimitiveKind.STRING)
    override fun serialize(encoder: Encoder, value: Token) = encoder.encodeString(value.raw)
    override fun deserialize(decoder: Decoder): Token = Token(decoder.decodeString())
}
"#;

fn run(file_annotation: &str, holder: &str, check: &str, stem: &str) {
    let source = format!(
        "{file_annotation}\n{IMPORTS}{SERVED}\n@Serializable\n{holder}\nfun box(): String {{\n{check}\n}}\n"
    );
    assert_eq!(both_compilers_box(&source, stem), "OK");
}

fn expect_json(expression: &str, json: &str) -> String {
    format!(
        r#"    val json = Json.encodeToString(Holder.serializer(), {expression})
    return if (json == """{json}""") "OK" else "FAIL: " + json"#
    )
}

#[test]
fn a_file_level_serializer_serves_a_direct_property() {
    run(
        "@file:UseSerializers(TokenAsString::class)",
        "data class Holder(val token: Token)",
        &expect_json(r#"Holder(Token("a"))"#, r#"{"token":"a"}"#),
        "use_serializers_file_direct",
    );
}

#[test]
fn a_file_level_serializer_serves_a_type_argument_and_a_nullable() {
    run(
        "@file:UseSerializers(TokenAsString::class)",
        "data class Holder(val tokens: List<Token>, val maybe: Token?)",
        &expect_json(
            r#"Holder(listOf(Token("a"), Token("b")), null)"#,
            r#"{"tokens":["a","b"],"maybe":null}"#,
        ),
        "use_serializers_file_nested",
    );
}

#[test]
fn an_explicit_serializer_on_the_property_keeps_precedence() {
    let override_serializer = r#"
object TokenUpper : KSerializer<Token> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor("Token", PrimitiveKind.STRING)
    override fun serialize(encoder: Encoder, value: Token) = encoder.encodeString(value.raw.uppercase())
    override fun deserialize(decoder: Decoder): Token = Token(decoder.decodeString())
}
"#;
    run(
        "@file:UseSerializers(TokenAsString::class)",
        &format!(
            "data class Holder(val token: Token, @Serializable(with = TokenUpper::class) val loud: Token)\n{override_serializer}"
        ),
        &expect_json(
            r#"Holder(Token("a"), Token("b"))"#,
            r#"{"token":"a","loud":"B"}"#,
        ),
        "use_serializers_explicit_wins",
    );
}
