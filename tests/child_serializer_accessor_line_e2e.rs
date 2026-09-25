//! The generated `access$get$childSerializers$cp` accessor records its declaration line.
//!
//! kotlinc gives every member it generates for the child-serializer cache exactly ONE
//! `LineNumberTable` entry, and the line is the serialized class's annotation-inclusive declaration
//! line — the line its `@Serializable` sits on, not the `class` keyword's. krusty emitted no table
//! at all on the accessor, which is a whole missing `Code` sub-attribute on every `@Serializable`
//! class that carries a cache.
//!
//! All three generated members are asserted: the accessor's entry is at offset 0, the factory's at
//! its `areturn`, and `<clinit>`'s at the offset where the array construction begins. The offsets
//! differ, so a rule that emitted one entry per member at a fixed pc would pass only one of them.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

/// `items` holds a collection of a `@Serializable` class, which is what gives `Holder` a cache.
/// The blank line and the annotation above the class put the declaration line at 4 while the
/// `class` keyword is on 5, so a table that recorded the wrong one is visible here.
const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Item(val id: Int)\n\
                   @Serializable\n\
                   data class Holder(val count: Int, val items: List<Item>)\n";

/// `(start_pc, line)` rows of a method's `LineNumberTable`.
///
/// Anchored on the method's DECLARATION line — javap prints the constant pool first, and the
/// generated names appear there as Utf8 entries long before the method itself.
fn line_table(disassembly: &str, method: &str) -> Vec<String> {
    let lines: Vec<&str> = disassembly.lines().collect();
    // javap renders `<clinit>` as `static {};`, not as a parenthesised signature.
    let declared = lines.iter().position(|line| {
        let trimmed = line.trim();
        if !line.starts_with("  ") || line.starts_with("   ") {
            return false;
        }
        if method == "<clinit>" {
            return trimmed == "static {};";
        }
        trimmed.ends_with(");") && trimmed.contains(&format!(" {method}("))
    });
    let Some(declared) = declared else {
        return Vec::new();
    };
    lines[declared..]
        .iter()
        .skip_while(|line| !line.contains("LineNumberTable"))
        .skip(1)
        .take_while(|line| line.trim_start().starts_with("line "))
        .map(|line| line.trim().to_string())
        .collect()
}

/// Every member the cache generates carries exactly one entry, all on the same line, at three
/// different offsets.
#[test]
fn every_generated_cache_member_records_the_declaration_line() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(
        "ChildSerializerMemberLines",
        SRC,
        "Holder",
        &cp,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    for member in [
        "_childSerializers$_anonymous_",
        "access$get$childSerializers$cp",
        "<clinit>",
    ] {
        let want = line_table(&built.reference, member);
        assert_eq!(
            want.len(),
            1,
            "the reference records exactly one entry for {member}:\n{}",
            built.reference
        );
        assert_eq!(
            line_table(&built.krusty, member),
            want,
            "krusty must record the same entry for {member}"
        );
    }
}
