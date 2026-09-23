//! Exact debug-table ownership for serialization-generated members.

use super::common;
use super::serialization_companion_byte_parity_e2e::compare_with_kotlinc_plugin;

const SERIALIZATION_VERSION: &str = "1.9.0";

const SOURCE: &str = "import kotlinx.serialization.Serializable\n\
                     \n\
                     @Serializable\n\
                     data class Spread(\n\
                     \x20   val first: Int,\n\
                     \x20   val second: String,\n\
                     \x20   val third: Boolean,\n\
                     )\n";

fn method_lines(text: &str, member: &str) -> Vec<String> {
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    let method = lines
        .iter()
        .position(|line| line.contains(member) && line.ends_with(';'))
        .unwrap_or_else(|| panic!("missing method {member}:\n{text}"));
    lines
        .into_iter()
        .skip(method + 1)
        .skip_while(|line| !line.starts_with("LineNumberTable"))
        .skip(1)
        .take_while(|line| line.starts_with("line "))
        .map(str::to_string)
        .collect()
}

fn comparison(class: &str) -> super::serialization_companion_byte_parity_e2e::ReferenceComparison {
    let plugin = common::kotlinc_lib_dir()
        .unwrap_or_else(|| panic!("reference compiler must be provisioned for this regression"))
        .join("kotlinx-serialization-compiler-plugin.jar");
    assert!(
        plugin.is_file(),
        "reference serialization plugin is missing at {}",
        plugin.display()
    );
    let runtime = krusty::toolchain::ensure_maven(
        "org.jetbrains.kotlinx",
        "kotlinx-serialization-core-jvm",
        SERIALIZATION_VERSION,
    )
    .unwrap_or_else(|| {
        panic!(
            "could not provision kotlinx-serialization-core-jvm:{SERIALIZATION_VERSION}; check \
             network access or set KRUSTY_DEPS_CACHE"
        )
    });
    let classpath = [runtime, common::stdlib_jar()];
    compare_with_kotlinc_plugin(
        "SerializeElementLines",
        SOURCE,
        class,
        &classpath,
        "25",
        &[format!("-Xplugin={}", plugin.display())],
    )
    .unwrap_or_else(|| panic!("reference kotlinc and javap must be available for this regression"))
}

/// `serialize` closes on the line the source declaration ends. Sibling generated members and the
/// source-class `write$Self` helper do not inherit that producer-owned fallthrough role.
#[test]
fn only_serialize_closes_on_the_declarations_end_line() {
    let serializer = comparison("Spread$$serializer");
    let serialize = method_lines(
        &serializer.reference,
        "void serialize(kotlinx.serialization.encoding.Encoder, Spread)",
    );
    assert_eq!(serialize, ["line 3: 12", "line 8: 40"]);
    assert_eq!(
        method_lines(
            &serializer.krusty,
            "void serialize(kotlinx.serialization.encoding.Encoder, Spread)",
        ),
        serialize
    );

    for member in [
        "Spread deserialize(kotlinx.serialization.encoding.Decoder)",
        "childSerializers()",
    ] {
        let expected = method_lines(&serializer.reference, member);
        assert!(
            !expected.is_empty(),
            "kotlinc omitted {member}'s line table"
        );
        assert!(
            expected.iter().all(|row| !row.starts_with("line 8:")),
            "{member} must not consume serialize's closing-line role: {expected:?}"
        );
        assert_eq!(
            method_lines(&serializer.krusty, member),
            expected,
            "{member}"
        );
    }

    let source_class = comparison("Spread");
    let expected = method_lines(&source_class.reference, "write$Self");
    assert!(
        !expected.is_empty(),
        "kotlinc omitted write$Self's line table"
    );
    assert!(
        expected.iter().all(|row| !row.starts_with("line 8:")),
        "write$Self must not consume serialize's closing-line role: {expected:?}"
    );
    assert_eq!(
        method_lines(&source_class.krusty, "write$Self"),
        expected,
        "write$Self"
    );
}
