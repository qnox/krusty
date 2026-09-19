//! The `$childSerializers` cache a `@Serializable` class builds for its ALLOCATED element
//! serializers.
//!
//! The cache is built in a SECOND pass, after every class's `$serializer` exists. Building it
//! inside the generation loop asked for a sibling's `$serializer` before that class had been
//! reached and stored `null` in its place — and no single ordering fixes that, because two
//! `@Serializable` classes may hold collections of each other. These tests are the evidence for
//! both halves of that claim.
//!
//! Every step fails closed: a missing plugin, runtime, reference compiler or a failed krusty
//! compile is a failure, not a skip.
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, plugin_and_runtime,
};

/// A class's `<clinit>` instructions, with constant-pool indices erased because their numbering is
/// an emission-order artifact.
///
/// `static {}` is the LAST method in these classes, so nothing follows to stop the scan: the
/// `BootstrapMethods` table's own rows are numbered from 0 and would read as instructions. The
/// window ends at the first row whose program counter does not advance.
fn class_initializer(disassembly: &str) -> Vec<String> {
    let mut last: Option<u32> = None;
    disassembly
        .lines()
        .map(str::trim)
        .skip_while(|line| !line.starts_with("static {}"))
        .skip_while(|line| !line.starts_with("0:"))
        .map(|line| {
            let mut out = String::new();
            let mut rest = line;
            while let Some(at) = rest.find('#') {
                out.push_str(&rest[..at]);
                out.push('#');
                rest = rest[at + 1..].trim_start_matches(|c: char| c.is_ascii_digit());
            }
            out.push_str(rest);
            out
        })
        .take_while(|row| {
            let Some(pc) = row
                .split(':')
                .next()
                .and_then(|pc| pc.trim().parse::<u32>().ok())
            else {
                return false;
            };
            let advances = last.is_none_or(|previous| pc > previous);
            last = Some(pc);
            advances
        })
        .collect()
}

/// Compile `source` with both compilers and return `class`'s disassembly from each.
fn built(name: &str, source: &str, class: &str) -> (String, String) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(name, source, class, &cp, "25", &extra)
        .expect("the reference compiler and javap must be available to this regression");
    (built.reference, built.krusty)
}

const LEAF_FIRST: &str = "import kotlinx.serialization.Serializable\n\
                          @Serializable\n\
                          data class Leaf(val id: Int)\n\
                          @Serializable\n\
                          data class Branch(val count: Int, val leaves: List<Leaf>)\n";

/// The composed slot holds the nested class's own serializer rather than a null, and the reference
/// defers each one behind a lazy over a bound factory.
#[test]
fn a_composed_element_serializer_is_cached_rather_than_left_null() {
    let (reference, krusty) = built("ComposedCache", LEAF_FIRST, "Branch");
    let expected = class_initializer(&reference);
    assert!(
        expected
            .iter()
            .any(|row| row.contains("kotlin/LazyKt.lazy:"))
            && expected.iter().any(|row| row.contains("invokedynamic")),
        "the reference defers each composed serializer behind a lazy over a bound factory:\n\
         {expected:#?}"
    );
    let actual = class_initializer(&krusty);
    assert!(
        actual.iter().any(|row| row.contains("Leaf$$serializer")),
        "krusty's cache must reach the nested class's serializer, not store a null:\n{actual:#?}"
    );
}

/// The OWNER declared before the class it references — the adversarial order.
///
/// This is the shape the defect was found on: the generation loop reached `Branch` while `Leaf`
/// still had no `$serializer`, and the slot took a null. Declaration order must not change what is
/// emitted at all, so the two orders are compared against each other rather than against a
/// spelled-out expectation that could drift.
#[test]
fn the_declaration_order_does_not_change_the_cache() {
    let branch_first = "import kotlinx.serialization.Serializable\n\
                        @Serializable\n\
                        data class Branch(val count: Int, val leaves: List<Leaf>)\n\
                        @Serializable\n\
                        data class Leaf(val id: Int)\n";
    let (_, leaf_first) = built("CacheLeafFirst", LEAF_FIRST, "Branch");
    let (_, owner_first) = built("CacheOwnerFirst", branch_first, "Branch");

    let expected = class_initializer(&leaf_first);
    assert!(
        expected.iter().any(|row| row.contains("Leaf$$serializer")),
        "the baseline order must reach the nested serializer:\n{expected:#?}"
    );
    assert_eq!(
        class_initializer(&owner_first),
        expected,
        "the owner declared FIRST must emit the same cache; a slot that depends on declaration \
         order is the defect this pass exists to remove"
    );
}

/// Two `@Serializable` classes holding collections of each other — the shape no ordering can
/// satisfy, and the reason the cache cannot be built while the generation loop is still running.
#[test]
fn mutually_referential_classes_each_cache_the_other() {
    let source = "import kotlinx.serialization.Serializable\n\
                  @Serializable\n\
                  data class Node(val kids: List<Edge>)\n\
                  @Serializable\n\
                  data class Edge(val ends: List<Node>)\n";
    let (_, node) = built("CacheCycleNode", source, "Node");
    let (_, edge) = built("CacheCycleEdge", source, "Edge");

    let node_cache = class_initializer(&node);
    assert!(
        node_cache
            .iter()
            .any(|row| row.contains("Edge$$serializer")),
        "`Node`'s cache must reach `Edge`'s serializer:\n{node_cache:#?}"
    );
    let edge_cache = class_initializer(&edge);
    assert!(
        edge_cache
            .iter()
            .any(|row| row.contains("Node$$serializer")),
        "`Edge`'s cache must reach `Node`'s serializer — neither can be generated first:\n\
         {edge_cache:#?}"
    );
}
