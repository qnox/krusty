//! The synthetic bridges on a generated `$$serializer` keep the generated method's parameter names.
//!
//! `GeneratedSerializer<T>` is generic, so every `$$serializer` carries two covariant bridges —
//! `serialize(Encoder, Object)` and `deserialize(Decoder)Object` — that forward to the typed
//! methods. kotlinc gives their locals the same names the typed methods have (`encoder`, `value`,
//! `decoder`); krusty fell back to `p0`/`p1`, because bridge emission reads parameter names from
//! `IrFile::fn_params`, which a plugin-generated function does not populate — its names live in its
//! generated-member publication.
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, plugin_and_runtime,
};

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Point(val x: Int, val y: String)\n";

/// Every `LocalVariableTable` of a method whose declaration line contains `signature`, as
/// `(name, descriptor)` in slot order.
fn locals_of(disassembly: &str, signature: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut inside = false;
    let mut collecting = false;
    for raw in disassembly.lines() {
        let line = raw.trim();
        if line.contains(signature) && line.ends_with(';') {
            inside = true;
            collecting = false;
            continue;
        }
        if !inside {
            continue;
        }
        if line.starts_with("LocalVariableTable:") {
            collecting = true;
            continue;
        }
        if !collecting {
            continue;
        }
        if line.starts_with("Start") {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        match fields.as_slice() {
            [_, _, _, name, descriptor] => out.push(((*name).to_string(), (*descriptor).to_string())),
            _ => return out,
        }
    }
    out
}

fn assert_bridge_locals(signature: &str) {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) =
        compare_with_kotlinc_plugin("BridgeNames", SRC, "Point$$serializer", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = locals_of(&built.reference, signature);
    assert!(
        want.len() > 1,
        "the reference must carry named locals for `{signature}`, or this test is not about what it says: {want:?}"
    );
    assert!(
        want.iter().all(|(name, _)| !name.starts_with('p')),
        "the reference names every bridge local after the typed method: {want:?}"
    );
    assert_eq!(
        locals_of(&built.krusty, signature),
        want,
        "`{signature}` local variable table"
    );
}

#[test]
fn the_serialize_bridge_names_its_encoder_and_value() {
    assert_bridge_locals("public void serialize(kotlinx.serialization.encoding.Encoder, java.lang.Object)");
}

#[test]
fn the_deserialize_bridge_names_its_decoder() {
    assert_bridge_locals("public java.lang.Object deserialize(kotlinx.serialization.encoding.Decoder)");
}
