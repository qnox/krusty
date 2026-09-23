//! A nullable value class held as its carrier keeps its nullability on the JVM declarations.
//!
//! `T?` over a non-null reference underlying (`value class T(val s: String)`) needs no box: the
//! carrier `String` holds `T`'s `null`. The projection onto the carrier dropped the `?`, so every
//! declaration of that type read as a non-null `String`:
//!
//! ```text
//! kotlinc:  @Nullable private final String t;   @Nullable public final String getT-…();
//! krusty:   @NotNull  private final String t;   @NotNull  public final String getT-…();
//! ```
//!
//! — fields, accessors, parameters and returns alike, and a `var`'s setter checked its argument
//! for null.
use super::common;
use super::serialization_companion_byte_parity_e2e::compare_with_kotlinc_plugin;

const SOURCE: &str = "@JvmInline value class T(val s: String)\n\
    class H(val t: T?, var u: T? = null) {\n\
    \x20   fun f(x: T?): T? = x ?: t\n\
    }\n\
    fun g(x: T?): T? = x\n";

/// Each non-constructor member's header with the nullability annotations javap lists under it.
fn member_nullability(disassembly: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in disassembly.lines() {
        let member_header = line.starts_with("  ")
            && !line.starts_with("   ")
            && !line.trim_start().starts_with('#')
            && line.trim_end().ends_with(';');
        if member_header {
            out.push((line.trim().to_string(), Vec::new()));
        } else if let Some(annotation) = line.trim().strip_prefix("org.jetbrains.annotations.") {
            if let Some((_, annotations)) = out.last_mut() {
                annotations.push(annotation.to_string());
            }
        }
    }
    // Constructors differ for an unrelated reason (a hidden constructor's annotations).
    out.retain(|(header, _)| !header.contains(" H(") && !header.starts_with("H("));
    out
}

#[test]
fn a_nullable_value_class_carrier_is_annotated_nullable() {
    for class in ["H", "NullableValueCarrierKt"] {
        let Some(built) = compare_with_kotlinc_plugin(
            "NullableValueCarrier",
            SOURCE,
            class,
            &[common::stdlib_jar()],
            "25",
            &[],
        ) else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        let reference = member_nullability(&built.reference);
        assert!(
            reference
                .iter()
                .any(|(_, annotations)| annotations.iter().any(|a| a == "Nullable")),
            "{class}: {reference:?}"
        );
        assert_eq!(member_nullability(&built.krusty), reference, "{class}");
    }
}

#[test]
fn a_nullable_value_class_carrier_runs() {
    let src = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   val h = H(T(\"a\"), null)\n\
         \x20   if (h.f(null)?.s != \"a\") return \"FAIL elvis\"\n\
         \x20   if (g(null) != null) return \"FAIL null\"\n\
         \x20   h.u = null\n\
         \x20   h.u = T(\"b\")\n\
         \x20   return if (h.u?.s == \"b\") \"OK\" else \"FAIL var\"\n\
         }}\n"
    );
    common::expect_box_ok_with_stdlib(&src, "a nullable value class carried unboxed");
}
