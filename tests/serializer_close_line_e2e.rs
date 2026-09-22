//! Where a generated serializer's methods say they END.
//!
//! kotlinc gives `serialize` / `deserialize` on a `$$serializer` two `LineNumberTable` entries: the
//! body at the annotated declaration's FIRST line (its annotation, not its header), and the trailing
//! `return` at the declaration's LAST line — the `)` that closes the constructor, or the `}` that
//! closes a class body.
//!
//! krusty mapped that trailing return to the declaration's HEADER line instead. On a one-line
//! declaration the two coincide and nothing shows; every multi-line `@Serializable` class in a real
//! program has a different closing line, which is why this is one of the most common single-entry
//! differences in a serialization-heavy module.
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, plugin_and_runtime,
};

/// A declaration whose annotation, header and closing line are all different.
const SPANNING: &str = "import kotlinx.serialization.Serializable\n\
                        @Serializable\n\
                        data class Point(\n\
                            val x: Int,\n\
                            val y: String,\n\
                        )\n";

/// The same, with a body, so the declaration closes on a `}` well past its last property.
const WITH_BODY: &str = "import kotlinx.serialization.Serializable\n\
                         @Serializable\n\
                         data class Point(\n\
                             val x: Int,\n\
                             val y: String,\n\
                         ) {\n\
                             fun sum(): String = x.toString() + y\n\
                         }\n";

/// The `LineNumberTable` entries of one method, as `(line, pc)` pairs.
fn method_lines(disassembly: &str, method: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut inside = false;
    let mut collecting = false;
    for raw in disassembly.lines() {
        let line = raw.trim();
        if line.contains(&format!(" {method}(")) && line.ends_with(';') {
            inside = true;
            collecting = false;
            continue;
        }
        if !inside {
            continue;
        }
        if line == "LineNumberTable:" {
            collecting = true;
            continue;
        }
        if collecting {
            let parsed = line
                .strip_prefix("line ")
                .and_then(|rest| rest.split_once(": "))
                .and_then(|(line, pc)| Some((line.parse().ok()?, pc.parse().ok()?)));
            match parsed {
                Some(entry) => out.push(entry),
                None => return out,
            }
        }
    }
    out
}

fn assert_serializer_lines(name: &str, src: &str, method: &str, closing: u32) {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) =
        compare_with_kotlinc_plugin(name, src, "Point$$serializer", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_lines(&built.reference, method);
    assert_eq!(
        want.last().map(|(line, _)| *line),
        Some(closing),
        "{name}: the reference must end {method} on line {closing}, or this test is not about what it says: {want:?}"
    );
    assert_eq!(
        method_lines(&built.krusty, method),
        want,
        "{name}: {method} line table"
    );
}

#[test]
fn serialize_ends_on_the_declarations_closing_line() {
    assert_serializer_lines("SpanningSerialize", SPANNING, "serialize", 6);
}

/// The control: not every generated method takes the closing line. `deserialize` carries a single
/// entry, on the declaration's first line, so the fix cannot be "end every generated method there".
#[test]
fn deserialize_keeps_its_single_entry() {
    assert_serializer_lines("SpanningDeserialize", SPANNING, "deserialize", 2);
}

/// With a class body the closing line is the `}`, not the constructor's `)`.
#[test]
fn a_class_body_closes_on_its_brace() {
    assert_serializer_lines("BodySerialize", WITH_BODY, "serialize", 8);
}
