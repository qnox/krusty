//! The child-serializer cache's factory narrows its operands to `KSerializer`.
//!
//! kotlinc casts the element serializer to `KSerializer` before handing it to the collection
//! serializer's constructor, and casts the constructed serializer again before returning it —
//! though both already conform, so neither cast is needed for the verifier. krusty emitted
//! neither, leaving the factory six bytes shorter than the reference:
//!
//! ```text
//! kotlinc: new; dup; getstatic; checkcast KSerializer; invokespecial <init>; checkcast KSerializer; areturn
//! krusty : new; dup; getstatic;                        invokespecial <init>;                        areturn
//! ```
//!
//! The casts also move the `areturn`, which is the instruction kotlinc hangs the factory's
//! `LineNumberTable` entry on, so the shape has to be right before that entry can be.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Item(val id: Int)\n\
                   @Serializable\n\
                   data class Holder(val count: Int, val items: List<Item>)\n";

/// The opcodes of one method, in order, with pool indices and operands dropped.
///
/// Scans the window between the method's DECLARATION line and the next one: javap prints the
/// constant pool first (where these names also appear) and puts `stack=…`/attribute lines inside
/// the body, so neither end can be found by a single marker.
fn opcodes(disassembly: &str, method: &str) -> Vec<String> {
    let lines: Vec<&str> = disassembly.lines().collect();
    let declaration = |line: &str| {
        let trimmed = line.trim();
        line.starts_with("  ") && !line.starts_with("   ") && trimmed.ends_with(");")
    };
    let Some(start) = lines
        .iter()
        .position(|line| declaration(line) && line.contains(&format!(" {method}(")))
    else {
        return Vec::new();
    };
    let end = lines[start + 1..]
        .iter()
        .position(|line| declaration(line))
        .map_or(lines.len(), |offset| start + 1 + offset);
    lines[start..end]
        .iter()
        .filter_map(|line| line.trim().split_once(": "))
        .filter(|(pc, _)| !pc.is_empty() && pc.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|(_, rest)| rest.split_whitespace().next().map(str::to_string))
        .collect()
}

#[test]
fn the_factory_narrows_its_operands_like_the_reference() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(
        "ChildSerializerFactoryNarrowing",
        SRC,
        "Holder",
        &cp,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    let want = opcodes(&built.reference, "_childSerializers$_anonymous_");
    assert_eq!(
        want.iter().filter(|op| *op == "checkcast").count(),
        2,
        "the reference narrows twice:\n{want:#?}"
    );
    assert_eq!(
        opcodes(&built.krusty, "_childSerializers$_anonymous_"),
        want,
        "krusty must emit the same factory body"
    );
}
