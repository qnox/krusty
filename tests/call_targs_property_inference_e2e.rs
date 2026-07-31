//! The lightweight signature inferer (property signature collection) must preserve EXPLICIT call
//! type arguments: `val servers = mutableMapOf<String, JsonObject>()` at any property level used
//! to record `MutableMap<Any, Any>` — the `Expr::Call` path resolved the callee with NO type
//! arguments, so an unbound `K`/`V` defaulted to `Any`. The recorded type is asserted directly via
//! a deliberate mismatch (the diagnostic renders it), because assignability is too lenient to
//! expose the erasure (`Any` accepts everything).

use super::common;

const JSON_OBJECT: &str = "class JsonObject\n";

/// One `servers` declaration shape → one expression reading it.
const SHAPES: &[(&str, &str)] = &[
    (
        "top-level property",
        "val servers = mutableMapOf<String, JsonObject>()\nfun box() {\n    val x: Int = servers\n}\n",
    ),
    (
        "class property",
        "class Holder {\n    val servers = mutableMapOf<String, JsonObject>()\n}\nfun box() {\n    val x: Int = Holder().servers\n}\n",
    ),
    (
        "companion property",
        "class Holder {\n    companion object {\n        val servers = mutableMapOf<String, JsonObject>()\n    }\n}\nfun box() {\n    val x: Int = Holder.servers\n}\n",
    ),
    (
        "companion fun (expression body)",
        "class Holder {\n    companion object {\n        fun servers() = mutableMapOf<String, JsonObject>()\n    }\n}\nfun box() {\n    val x: Int = Holder.servers()\n}\n",
    ),
];

#[test]
fn explicit_call_targs_survive_property_signature_inference() {
    for (label, body) in SHAPES {
        let src = format!("{JSON_OBJECT}{body}");
        let Some(diagnostics) = common::checker_diags_with_stdlib(&src) else {
            eprintln!("skipping: no kotlinc/stdlib toolchain");
            return;
        };
        assert_eq!(
            diagnostics.len(),
            1,
            "{label}: expected exactly the deliberate mismatch: {diagnostics:?}"
        );
        assert!(
            diagnostics[0].contains("actual 'MutableMap<String, JsonObject>'"),
            "{label}: recorded type lost the explicit type arguments: {}",
            diagnostics[0]
        );
    }
}

/// Positive guard: correct downstream use of the property type-checks clean.
#[test]
fn explicit_call_targs_property_use_is_clean() {
    let src = format!(
        "{JSON_OBJECT}val servers = mutableMapOf<String, JsonObject>()\n\
         fun box() {{\n\
         \x20   val v: JsonObject? = servers[\"a\"]\n\
         \x20   val k: String = servers.keys.iterator().next()\n\
         }}\n"
    );
    let Some(diagnostics) = common::checker_diags_with_stdlib(&src) else {
        eprintln!("skipping: no kotlinc/stdlib toolchain");
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}
