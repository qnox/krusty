//! A defaulted element's write is guarded by a SHORT-CIRCUIT disjunction.
//!
//! kotlinx.serialization writes a property that has a default only when the encoder asks for
//! defaults or the value differs from the default:
//!
//! ```text
//! if (output.shouldEncodeElementDefault(desc, i) || self.x != <default>) { … }
//! ```
//!
//! Both compilers agree on the SEMANTICS — a round trip through `Json` produces the same text on
//! either — but krusty built that condition with `IrBinOp::Or`, the EAGER form, which holds the
//! left operand in a temporary and combines with `ior`. kotlinc branches: ask the encoder, answer
//! `true` without reading the property at all, otherwise fall through to the comparison.
//!
//! Every defaulted property of every `@Serializable` class carries one of these, so the two
//! compilers' `write$Self` differed on essentially every generated model class in a real project.

use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, plugin_and_runtime,
};

/// The instruction rows of one method, pool indices erased.
fn method_body(disassembly: &str, marker: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for raw in disassembly.lines() {
        let line = raw.trim();
        if line.contains(marker) && line.ends_with(");") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.starts_with("LineNumberTable") || line.starts_with("LocalVariableTable") {
            break;
        }
        let Some((pc, rest)) = line.split_once(": ") else {
            continue;
        };
        if pc.parse::<u32>().is_err() {
            continue;
        }
        out.push(format!("{pc}: {}", regex_free_erase_pool(rest)));
    }
    out
}

/// Erase `#123` constant-pool indices without pulling in a regex dependency.
fn regex_free_erase_pool(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '#' {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        }
    }
    out
}

#[test]
fn a_defaulted_element_is_guarded_the_way_kotlinc_guards_it() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    // A primitive default and a nullable-reference default: the comparison against the default is
    // a different instruction in each, and both sit under the same guard.
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Opt(val a: Int = 1, val b: String? = null)\n";
    let Some(built) =
        compare_with_kotlinc_plugin("DefaultElementGuard", src, "Opt", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_body(&built.reference, "write$Self$main");
    assert!(
        want.iter()
            .any(|row| row.contains("shouldEncodeElementDefault")),
        "the fixture must exercise the defaulted-element guard: {want:?}"
    );
    assert!(
        !want.iter().any(|row| row.contains(": ior")),
        "kotlinc's guard branches rather than combining with `ior`: {want:?}"
    );
    assert_eq!(
        method_body(&built.krusty, "write$Self$main"),
        want,
        "Opt.write$Self$main"
    );
}
