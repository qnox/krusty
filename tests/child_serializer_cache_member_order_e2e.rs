//! The child-serializer cache's generated members emit AFTER the deserialization constructor.
//!
//! kotlinc's member order for a `@Serializable` class that carries a cache is
//!
//! ```text
//! … write$Self$main, <init>(I…), _childSerializers$_anonymous_…, access$get$childSerializers$cp, <clinit>
//! ```
//!
//! The deserialization constructor is placed explicitly, "after every declared member" — a rule
//! written before the cache's generated members existed. Those are generated, so they carry no
//! source order for the declared-member schedule to sort by, landed last among it, and pushed the
//! constructor behind them.
//!
//! Member order decides constant-pool layout, so this sits in front of any byte comparison of these
//! classes.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

/// `items` holds a collection of a `@Serializable` class, which is what gives `Holder` a cache at
/// all — a class whose every element serializer is a singleton has no accessor to order.
const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Item(val id: Int)\n\
                   @Serializable\n\
                   data class Holder(val count: Int, val items: List<Item>)\n";

/// The declared members in file order, `<clinit>` included. javap renders a constructor under the
/// class's own simple name and `<clinit>` as `static {};`; both are normalised here.
fn members(disassembly: &str, class: &str) -> Vec<String> {
    disassembly
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            line.starts_with("  ")
                && !line.starts_with("   ")
                && (trimmed.ends_with(");") || trimmed == "static {};")
                && !trimmed.contains("//")
        })
        .map(|line| {
            let trimmed = line.trim();
            if trimmed == "static {};" {
                return "<clinit>".to_string();
            }
            let head = &trimmed[..trimmed.find('(').unwrap_or(trimmed.len())];
            let name = head.rsplit([' ', '.']).next().unwrap_or(head);
            if name == class { "<init>" } else { name }.to_string()
        })
        .collect()
}

/// The accessor is the cache member master already emits; it must follow the deserialization
/// constructor, which is the LAST `<init>` in the table.
#[test]
fn the_cache_accessor_follows_the_deserialization_constructor() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built =
        compare_with_kotlinc_plugin("ChildSerializerOrder", SRC, "Holder", &cp, "25", &extra)
            .expect("the reference compiler and javap must be available to this regression");
    for (who, text) in [("kotlinc", &built.reference), ("krusty", &built.krusty)] {
        let order = members(text, "Holder");
        let accessor = order
            .iter()
            .position(|m| m == "access$get$childSerializers$cp")
            .unwrap_or_else(|| panic!("{who} must emit the cache accessor:\n{order:#?}"));
        let last_init = order
            .iter()
            .rposition(|m| m == "<init>")
            .unwrap_or_else(|| panic!("{who} must emit a constructor:\n{order:#?}"));
        assert!(
            last_init < accessor,
            "{who} must order the deserialization constructor before the accessor:\n{order:#?}"
        );
    }
}
