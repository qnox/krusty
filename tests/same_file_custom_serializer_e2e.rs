use super::common;
use std::path::PathBuf;

/// The newest `<group>/<artifact>` jar in the local Gradle cache, or `None` when it isn't there.
fn gradle_module_jar(group: &str, artifact: &str) -> Option<PathBuf> {
    let artifact_dir = std::env::var_os("HOME")
        .map(PathBuf::from)?
        .join(".gradle/caches/modules-2/files-2.1")
        .join(group)
        .join(artifact);
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for version in std::fs::read_dir(&artifact_dir).ok()?.flatten() {
        let name = version.file_name();
        let Some(name) = name.to_str() else { continue };
        let key = name
            .split(|character: char| !character.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>();
        let jar_name = format!("{artifact}-{name}.jar");
        for hash in std::fs::read_dir(version.path()).ok()?.flatten() {
            let jar = hash.path().join(&jar_name);
            if jar.is_file() && best.as_ref().is_none_or(|(current, _)| key > *current) {
                best = Some((key.clone(), jar));
            }
        }
    }
    best.map(|(_, jar)| jar)
}

fn serialization_classpath() -> Option<Vec<PathBuf>> {
    let core = gradle_module_jar("org.jetbrains.kotlinx", "kotlinx-serialization-core-jvm")?;
    Some(vec![core, common::stdlib_jar()])
}

fn disassemble(bytes: &[u8], file_name: &str) -> String {
    let dir = common::scratch_dir().expect("scratch dir");
    let class_file = dir.join(file_name);
    std::fs::write(&class_file, bytes).expect("write class");
    let text = common::javap(&["-c", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = std::fs::remove_file(&class_file);
    text
}

const CUSTOM_OBJECT: &str = "package demo\n\
    import kotlinx.serialization.KSerializer\n\
    import kotlinx.serialization.Serializable\n\
    import kotlinx.serialization.descriptors.PrimitiveKind\n\
    import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
    import kotlinx.serialization.descriptors.SerialDescriptor\n\
    import kotlinx.serialization.encoding.Decoder\n\
    import kotlinx.serialization.encoding.Encoder\n\
    @Serializable(with = TagSerializer::class)\n\
    class Tag(val raw: String)\n\
    object TagSerializer : KSerializer<Tag> {\n\
    \x20 override val descriptor: SerialDescriptor =\n\
    \x20\x20 PrimitiveSerialDescriptor(\"Tag\", PrimitiveKind.STRING)\n\
    \x20 override fun serialize(encoder: Encoder, value: Tag) = encoder.encodeString(value.raw)\n\
    \x20 override fun deserialize(decoder: Decoder): Tag = Tag(decoder.decodeString())\n\
    }\n\
    @Serializable\n\
    data class Holder(val tag: Tag)\n";

/// A class in THIS file carrying `@Serializable(with = X::class)` has no generated `$serializer`, so a
/// containing class's element serializer must be `X` itself. The derivation only ever consulted the
/// CLASSPATH map (a dependency's serializer), so a same-file `with` left the element underivable, the
/// plugin's `serialize-body` placeholder survived, and the whole file was declined by the backend —
/// costing its module every class.
#[test]
fn a_same_file_custom_serializer_is_used_as_an_element_serializer() {
    let Some(classpath) = serialization_classpath() else {
        eprintln!("skipping: kotlinx-serialization-core not in the Gradle cache");
        return;
    };
    let classes = common::compile_in_process_files(
        &[("CustomObject", CUSTOM_OBJECT)],
        &classpath,
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty compiles a same-file @Serializable(with = …) element");

    let serializer = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/Holder$$serializer").then_some(bytes))
        .expect("Holder's generated serializer");
    let text = disassemble(serializer, "Holder$$serializer.class");
    assert!(
        text.contains("demo/TagSerializer"),
        "the element serializer must be the class's own `with` serializer:\n{text}"
    );
}

/// The same, for a GENERIC class: its custom serializer takes one `KSerializer` per type parameter, so
/// the element serializer is a construction (`CellSerializer(Leaf$serializer.INSTANCE)`), not a
/// singleton read. This is the optics-adjacent shape a corpus module uses.
#[test]
fn a_generic_same_file_custom_serializer_takes_its_argument_serializers() {
    let Some(classpath) = serialization_classpath() else {
        eprintln!("skipping: kotlinx-serialization-core not in the Gradle cache");
        return;
    };
    let source = "package demo\n\
        import kotlinx.serialization.KSerializer\n\
        import kotlinx.serialization.Serializable\n\
        import kotlinx.serialization.descriptors.SerialDescriptor\n\
        import kotlinx.serialization.encoding.Decoder\n\
        import kotlinx.serialization.encoding.Encoder\n\
        @Serializable(with = CellSerializer::class)\n\
        class Cell<T>(val value: T)\n\
        class CellSerializer<T>(private val inner: KSerializer<T>) : KSerializer<Cell<T>> {\n\
        \x20 override val descriptor: SerialDescriptor = inner.descriptor\n\
        \x20 override fun serialize(encoder: Encoder, value: Cell<T>) =\n\
        \x20\x20 inner.serialize(encoder, value.value)\n\
        \x20 override fun deserialize(decoder: Decoder): Cell<T> = Cell(inner.deserialize(decoder))\n\
        }\n\
        @Serializable\n\
        data class Leaf(val name: String)\n\
        @Serializable\n\
        data class Intent(val one: Cell<Leaf>)\n";
    let classes = common::compile_in_process_files(
        &[("CustomGeneric", source)],
        &classpath,
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty compiles a generic same-file @Serializable(with = …) element");

    let serializer = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/Intent$$serializer").then_some(bytes))
        .expect("Intent's generated serializer");
    let text = disassemble(serializer, "Intent$$serializer.class");
    assert!(
        text.contains("demo/CellSerializer"),
        "the element serializer must construct the custom serializer:\n{text}"
    );
    assert!(
        text.contains("demo/Leaf$$serializer"),
        "the custom serializer must receive its type argument's serializer:\n{text}"
    );
}

/// A SEALED class (or interface) naming its own serializer must get THAT serializer, not the
/// `SealedClassSerializer`/`PolymorphicSerializer` its shape would otherwise imply: the custom
/// serializer decides ahead of every structural rule.
#[test]
fn a_sealed_classs_custom_serializer_wins_over_the_sealed_rule() {
    let Some(classpath) = serialization_classpath() else {
        eprintln!("skipping: kotlinx-serialization-core not in the Gradle cache");
        return;
    };
    let source = "package demo\n\
        import kotlinx.serialization.KSerializer\n\
        import kotlinx.serialization.Serializable\n\
        import kotlinx.serialization.descriptors.PrimitiveKind\n\
        import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
        import kotlinx.serialization.descriptors.SerialDescriptor\n\
        import kotlinx.serialization.encoding.Decoder\n\
        import kotlinx.serialization.encoding.Encoder\n\
        @Serializable(with = FlexSerializer::class)\n\
        sealed interface Flex {\n\
        \x20 data class Text(val raw: String) : Flex\n\
        }\n\
        object FlexSerializer : KSerializer<Flex> {\n\
        \x20 override val descriptor: SerialDescriptor =\n\
        \x20\x20 PrimitiveSerialDescriptor(\"Flex\", PrimitiveKind.STRING)\n\
        \x20 override fun serialize(encoder: Encoder, value: Flex) =\n\
        \x20\x20 encoder.encodeString((value as Flex.Text).raw)\n\
        \x20 override fun deserialize(decoder: Decoder): Flex = Flex.Text(decoder.decodeString())\n\
        }\n\
        @Serializable\n\
        data class Holder(val flex: Flex)\n";
    let classes = common::compile_in_process_files(
        &[("SealedCustom", source)],
        &classpath,
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty compiles a sealed @Serializable(with = …) element");

    let serializer = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/Holder$$serializer").then_some(bytes))
        .expect("Holder's generated serializer");
    let text = disassemble(serializer, "SealedHolder$$serializer.class");
    assert!(
        text.contains("demo/FlexSerializer"),
        "the sealed type's own `with` serializer must win:\n{text}"
    );
    assert!(
        !text.contains("SealedClassSerializer"),
        "a class naming its serializer never gets the sealed runtime serializer:\n{text}"
    );
}
