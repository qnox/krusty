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

const TOKEN_UPPER: &str = r#"
object TokenUpper : KSerializer<Token> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor("Token", PrimitiveKind.STRING)
    override fun serialize(encoder: Encoder, value: Token) = encoder.encodeString(value.raw.uppercase())
    override fun deserialize(decoder: Decoder): Token = Token(decoder.decodeString())
}
"#;

#[test]
fn a_serializer_inheriting_kserializer_through_a_generic_base_serves_its_type() {
    run(
        "@file:UseSerializers(TokenUpperViaBase::class)",
        r#"data class Holder(val token: Token)

abstract class UppercaseBase<T> : KSerializer<T> {
    override val descriptor: SerialDescriptor = PrimitiveSerialDescriptor("Token", PrimitiveKind.STRING)
}

object TokenUpperViaBase : UppercaseBase<Token>() {
    override fun serialize(encoder: Encoder, value: Token) = encoder.encodeString(value.raw.uppercase())
    override fun deserialize(decoder: Decoder): Token = Token(decoder.decodeString())
}"#,
        &expect_json(r#"Holder(Token("a"))"#, r#"{"token":"A"}"#),
        "use_serializers_file_generic_base",
    );
}

#[test]
fn a_later_entry_for_the_same_served_type_replaces_an_earlier_one() {
    let holder = format!("data class Holder(val token: Token)\n{TOKEN_UPPER}");
    run(
        "@file:UseSerializers(TokenAsString::class, TokenUpper::class)",
        &holder,
        &expect_json(r#"Holder(Token("a"))"#, r#"{"token":"A"}"#),
        "use_serializers_file_later_wins",
    );
    run(
        "@file:UseSerializers(TokenUpper::class, TokenAsString::class)",
        &holder,
        &expect_json(r#"Holder(Token("a"))"#, r#"{"token":"a"}"#),
        "use_serializers_file_earlier_replaced",
    );
}

#[test]
fn a_nullable_use_prefers_the_nullable_entry_and_a_non_null_use_never_takes_it() {
    let holder = r#"data class Holder(val token: Token, val maybe: Token?)

object TokenOrNull : KSerializer<Token?> {
    override val descriptor: SerialDescriptor =
        PrimitiveSerialDescriptor("TokenOrNull", PrimitiveKind.STRING).nullable
    override fun serialize(encoder: Encoder, value: Token?) = encoder.encodeString(value!!.raw + "?")
    override fun deserialize(decoder: Decoder): Token? = Token(decoder.decodeString())
}"#;
    run(
        "@file:UseSerializers(TokenAsString::class, TokenOrNull::class)\nimport kotlinx.serialization.descriptors.nullable",
        holder,
        &expect_json(
            r#"Holder(Token("a"), Token("b"))"#,
            r#"{"token":"a","maybe":"b?"}"#,
        ),
        "use_serializers_file_nullable_key",
    );
}
