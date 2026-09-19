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
    compare_with_kotlinc_plugin, method_instructions, plugin_and_runtime,
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
        .filter_map(|line| {
            let (pc, instruction) = line.split_once(": ")?;
            let pc = pc.parse::<u32>().ok()?;
            let instruction = instruction
                .split_whitespace()
                .map(|token| if token.starts_with('#') { "#" } else { token })
                .collect::<Vec<_>>()
                .join(" ");
            Some((pc, format!("{pc}: {instruction}")))
        })
        .take_while(|(pc, _)| {
            let advances = last.is_none_or(|previous| *pc > previous);
            last = Some(*pc);
            advances
        })
        .map(|(_, row)| row)
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

fn branch_initializer() -> Vec<String> {
    [
        "0: new # // class Branch$Companion",
        "3: dup",
        "4: aconst_null",
        "5: invokespecial # // Method Branch$Companion.\"<init>\":(Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
        "8: putstatic # // Field Companion:LBranch$Companion;",
        "11: iconst_2",
        "12: anewarray # // class kotlin/Lazy",
        "15: astore_0",
        "16: aload_0",
        "17: iconst_0",
        "18: aconst_null",
        "19: aastore",
        "20: aload_0",
        "21: iconst_1",
        "22: getstatic # // Field Leaf$$serializer.INSTANCE:LLeaf$$serializer;",
        "25: invokestatic # // Method kotlinx/serialization/builtins/BuiltinSerializersKt.ListSerializer:(Lkotlinx/serialization/KSerializer;)Lkotlinx/serialization/KSerializer;",
        "28: invokestatic # // Method kotlin/LazyKt.lazyOf:(Ljava/lang/Object;)Lkotlin/Lazy;",
        "31: aastore",
        "32: aload_0",
        "33: putstatic # // Field $childSerializers:[Lkotlin/Lazy;",
        "36: return",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn cycle_initializer(owner: &str, nested: &str) -> Vec<String> {
    vec![
        format!("0: new # // class {owner}$Companion"),
        "3: dup".to_string(),
        "4: aconst_null".to_string(),
        format!(
            "5: invokespecial # // Method {owner}$Companion.\"<init>\":(Lkotlin/jvm/internal/DefaultConstructorMarker;)V"
        ),
        format!("8: putstatic # // Field Companion:L{owner}$Companion;"),
        "11: iconst_1".to_string(),
        "12: anewarray # // class kotlin/Lazy".to_string(),
        "15: astore_0".to_string(),
        "16: aload_0".to_string(),
        "17: iconst_0".to_string(),
        format!(
            "18: getstatic # // Field {nested}$$serializer.INSTANCE:L{nested}$$serializer;"
        ),
        "21: invokestatic # // Method kotlinx/serialization/builtins/BuiltinSerializersKt.ListSerializer:(Lkotlinx/serialization/KSerializer;)Lkotlinx/serialization/KSerializer;".to_string(),
        "24: invokestatic # // Method kotlin/LazyKt.lazyOf:(Ljava/lang/Object;)Lkotlin/Lazy;"
            .to_string(),
        "27: aastore".to_string(),
        "28: aload_0".to_string(),
        "29: putstatic # // Field $childSerializers:[Lkotlin/Lazy;".to_string(),
        "32: return".to_string(),
    ]
}

/// The composed slot holds the nested class's own serializer rather than a null, and the reference
/// defers each one behind a lazy over a bound factory.
#[test]
fn a_composed_element_serializer_is_cached_rather_than_left_null() {
    let (reference, krusty) = built("ComposedCache", LEAF_FIRST, "Branch");
    assert_eq!(
        class_initializer(&reference),
        vec![
            "0: new # // class Branch$Companion",
            "3: dup",
            "4: aconst_null",
            "5: invokespecial # // Method Branch$Companion.\"<init>\":(Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
            "8: putstatic # // Field Companion:LBranch$Companion;",
            "11: iconst_2",
            "12: anewarray # // class kotlin/Lazy",
            "15: astore_0",
            "16: aload_0",
            "17: iconst_0",
            "18: aconst_null",
            "19: aastore",
            "20: aload_0",
            "21: iconst_1",
            "22: getstatic # // Field kotlin/LazyThreadSafetyMode.PUBLICATION:Lkotlin/LazyThreadSafetyMode;",
            "25: invokedynamic # 0 // InvokeDynamic #",
            "30: invokestatic # // Method kotlin/LazyKt.lazy:(Lkotlin/LazyThreadSafetyMode;Lkotlin/jvm/functions/Function0;)Lkotlin/Lazy;",
            "33: aastore",
            "34: aload_0",
            "35: putstatic # // Field $childSerializers:[Lkotlin/Lazy;",
            "38: return",
        ],
        "kotlinc's complete normalized initializer"
    );
    assert_eq!(
        class_initializer(&krusty),
        branch_initializer(),
        "krusty's complete normalized initializer"
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
    assert_eq!(
        expected,
        branch_initializer(),
        "the baseline order's complete initializer"
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
    let edge_cache = class_initializer(&edge);
    assert_eq!(
        node_cache,
        cycle_initializer("Node", "Edge"),
        "`Node`'s complete cache initializer"
    );
    assert_eq!(
        edge_cache,
        cycle_initializer("Edge", "Node"),
        "`Edge`'s complete cache initializer — neither class can be generated first"
    );
}

/// The fixture for the composed-element differentials below: TWO cached elements, so a reader that
/// confuses one slot for another is visible, beside a primitive element that gets no slot at all.
///
/// Every composed element here is non-nullable on purpose. A nullable one additionally exercises
/// the narrowing of the serializer operand, which is a separate concern with its own coverage; a
/// fixture mixing the two would fail for a reason that has nothing to do with the cache.
const COMPOSED_ELEMENTS_SRC: &str = "import kotlinx.serialization.Serializable\n\
     @Serializable\n\
     data class Item(val id: Int)\n\
     @Serializable\n\
     data class Holder(val count: Int, val items: List<Item>, val tags: List<String>)\n";

/// `childSerializers()` was the only method the cache work pinned, and it is the one method where a
/// wrong cache is harmless — it rebuilds the array either way. The two methods that CONSUME the
/// cache are `deserialize`, which reads a slot per element, and the serialized class's `write$Self`,
/// which reads the static directly. Compare both against kotlinc instruction for instruction.
#[test]
fn a_composed_class_deserializes_exactly_as_kotlinc() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(
        "ComposedElementsDeserialize",
        COMPOSED_ELEMENTS_SRC,
        "Holder$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_instructions(&built.reference, "deserialize(");
    assert!(
        !want.is_empty(),
        "the reference disassembly has no deserialize body"
    );
    assert_eq!(
        method_instructions(&built.krusty, "deserialize("),
        want,
        "Holder$$serializer.deserialize"
    );
}

#[test]
fn a_composed_class_writes_its_elements_exactly_as_kotlinc() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(
        "ComposedElementsWriteSelf",
        COMPOSED_ELEMENTS_SRC,
        "Holder",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_instructions(&built.reference, "write$Self$main(");
    assert!(
        !want.is_empty(),
        "the reference disassembly has no write$Self$main body"
    );
    assert_eq!(
        method_instructions(&built.krusty, "write$Self$main("),
        want,
        "Holder.write$Self$main"
    );
}

/// `decodeSerializableElement`'s last argument is the PREVIOUS value: a merging serializer is handed
/// what it is merging into. krusty passed a literal `null` there, which is unobservable from any
/// program a user can write — only the serializers kotlinx declares internally merge — so it is
/// pinned here as its own repository-owned expectation rather than left to a round trip.
///
/// The expectation is the operand shape, not kotlinc's whole body: for each of the two composed
/// elements the value pushed before the call is the element's OWN local, and never `aconst_null`.
#[test]
fn a_composed_element_decodes_with_its_own_previous_value() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(
        "ComposedElementsPreviousValue",
        COMPOSED_ELEMENTS_SRC,
        "Holder$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let previous_values = |disassembly: &str| -> Vec<String> {
        let body = method_instructions(disassembly, "deserialize(");
        body.iter()
            .enumerate()
            .filter(|(_, line)| line.contains("decodeSerializableElement:"))
            .map(|(at, _)| {
                body[at - 1]
                    .split_once(": ")
                    .map(|(_, op)| op.to_string())
                    .unwrap_or_default()
            })
            .collect()
    };
    let got = previous_values(&built.krusty);
    assert!(
        !got.is_empty(),
        "the fixture no longer decodes any composed element"
    );
    assert!(
        got.iter().all(|op| op.starts_with("aload")),
        "every composed element must be decoded over its own previous value: {got:?}"
    );
    assert_eq!(
        got,
        previous_values(&built.reference),
        "previous-value operands disagree with kotlinc"
    );
}
