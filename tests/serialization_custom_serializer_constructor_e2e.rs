//! A custom serializer CLASS named by `@Serializable(with = X::class)` is constructed by the served
//! class's generated `serializer(typeSerial0, …)` accessor through `X`'s PRIMARY constructor, which
//! must take no parameters or exactly one `KSerializer` per type parameter of the served class.
//! The accessor passes its operands to those parameters positionally — not by matching `X`'s own
//! type parameters — and kotlinc's plugin rejects any other primary constructor at the
//! `@Serializable` annotation.

use std::path::PathBuf;

use super::common;
use super::serialization_test_support::both_compilers_box_files;

const TAGGED: &str = "package model

import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder

@Serializable(with = TaggedSerializer::class)
data class Tagged<K, V>(val key: K, val value: V)

// The parameters are in the reverse order of the type parameters they are typed with, and a
// secondary constructor of a different shape sits beside the primary one.
class TaggedSerializer<K, V>(
    private val valueSerializer: KSerializer<V>,
    private val keySerializer: KSerializer<K>,
) : KSerializer<Tagged<K, V>> {
    @Suppress(\"UNCHECKED_CAST\")
    constructor(only: KSerializer<K>) : this(only as KSerializer<V>, only)

    override val descriptor: SerialDescriptor = keySerializer.descriptor

    override fun serialize(encoder: Encoder, value: Tagged<K, V>) = error(\"unused\")

    override fun deserialize(decoder: Decoder): Tagged<K, V> = error(\"unused\")
}
";

/// The accessor's first operand (`String`'s serializer) reaches the first constructor parameter,
/// whatever type parameter that parameter is typed with, so `keySerializer` holds `Int`'s. The
/// serializer's descriptor makes the mapping observable, both through the accessor and through an
/// element of another file's class.
#[test]
fn a_custom_serializer_class_takes_its_operands_positionally() {
    let main = "import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.serializer
import model.Tagged

@Serializable
data class Holder(val tagged: Tagged<String, Int>)

fun box(): String {
    val direct = Tagged.serializer(String.serializer(), Int.serializer()).descriptor.serialName
    val element = Holder.serializer().descriptor.getElementDescriptor(0).serialName
    return if (direct == \"kotlin.Int\" && element == \"kotlin.Int\") \"OK\" else \"FAIL: $direct $element\"
}
";
    assert_eq!(
        both_compilers_box_files(
            &[("Tagged.kt", TAGGED), ("Main.kt", main)],
            "custom_serializer_positional_operands"
        ),
        "OK"
    );
}

/// Every primary-constructor shape kotlinc rejects, beside one it accepts (no parameters).
const MODEL: &str = "package model

import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder

@Serializable(with = PlainSerializer::class)
class Plain(val raw: String)

class PlainSerializer(private val inner: KSerializer<String>) : KSerializer<Plain> {
    override val descriptor: SerialDescriptor = inner.descriptor
    override fun serialize(encoder: Encoder, value: Plain) = inner.serialize(encoder, value.raw)
    override fun deserialize(decoder: Decoder): Plain = Plain(inner.deserialize(decoder))
}

@Serializable(with = WrongSerializer::class)
data class Wrapped<T>(val value: T)

class WrongSerializer<T>(private val a: KSerializer<T>, private val b: String) : KSerializer<Wrapped<T>> {
    override val descriptor: SerialDescriptor = a.descriptor
    override fun serialize(encoder: Encoder, value: Wrapped<T>) = a.serialize(encoder, value.value)
    override fun deserialize(decoder: Decoder): Wrapped<T> = Wrapped(a.deserialize(decoder))
}

@Serializable(with = TypedSerializer::class)
class Typed<A, B>(val a: A, val b: B)

@Serializable(with = NoArgSerializer::class)
class Boxed<T>(val t: T)

class NoArgSerializer<T> : KSerializer<Boxed<T>> {
    override val descriptor: SerialDescriptor get() = error(\"unused\")
    override fun serialize(encoder: Encoder, value: Boxed<T>) = error(\"unused\")
    override fun deserialize(decoder: Decoder): Boxed<T> = error(\"unused\")
}
";

/// The rejected serializer of `Typed`, declared in another file than the class it serves.
const SERIALIZERS: &str = "package model

import kotlinx.serialization.KSerializer
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder

class TypedSerializer<X, Y : Any>(private val label: String, private val second: KSerializer<Y>) : KSerializer<Typed<X, Y>> {
    override val descriptor: SerialDescriptor = second.descriptor
    override fun serialize(encoder: Encoder, value: Typed<X, Y>) = error(label)
    override fun deserialize(decoder: Decoder): Typed<X, Y> = error(label)
}
";

fn plugin_switch() -> String {
    let plugin = common::kotlinc_lib_dir()
        .expect("the reference kotlinc distribution is provisioned")
        .join("kotlinx-serialization-compiler-plugin.jar");
    assert!(
        plugin.is_file(),
        "the reference serialization plugin is missing at {}",
        plugin.display()
    );
    format!("-Xplugin={}", plugin.display())
}

fn classpath() -> Vec<PathBuf> {
    vec![
        common::stdlib_jar(),
        krusty::toolchain::serialization_core_jar()
            .expect("the serialization core runtime is provisioned"),
    ]
}

#[test]
fn a_broken_custom_serializer_constructor_is_rejected_like_kotlinc() {
    let sources = [("Model.kt", MODEL), ("Serializers.kt", SERIALIZERS)];
    let result =
        common::compiler_diagnostics_with_shared_args(&sources, &classpath(), &[plugin_switch()]);
    let render = |errors: Vec<common::CompilerError>| {
        errors
            .into_iter()
            .map(|error| {
                format!(
                    "{}:{}:{}: {}",
                    error.file, error.line, error.column, error.message
                )
            })
            .collect::<Vec<_>>()
    };
    let kotlinc = render(common::compiler_errors(&result.reference_stderr));
    assert_ne!(result.reference_code, 0, "kotlinc accepted the fixture");
    assert_ne!(result.krusty_code, 0, "krusty accepted the fixture");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    let krusty = render(krusty);
    assert_eq!(
        krusty, kotlinc,
        "krusty: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
