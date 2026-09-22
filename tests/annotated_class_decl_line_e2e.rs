//! A class whose annotation sits on its own line got the wrong primary-constructor
//! `LineNumberTable`, so no annotated class could be byte-identical to kotlinc.
//!
//! kotlinc uses TWO lines for a primary constructor, and they are not the same line:
//!
//!   * the `super()` call maps to where the DECLARATION starts — annotations included;
//!   * the trailing `return` maps back to the class HEADER line.
//!
//! They coincide for an unannotated class, which is why one line (`ClassDecl::decl_line`, the
//! header) served for both and every fixture stayed green. Put `@Mark` on the line above and
//! kotlinc emits `line 5 → super()`, `line 6 → the property stores`, while krusty emitted a single
//! `line 6` entry. Naively moving `decl_line` to the annotation is wrong in the other direction: the
//! trailing `return` then lands on line 5 too and kotlinc has no such entry.
//!
//! `@Serializable`, `@Entity`, `@JsonClass` and friends put this on the hot path for real code —
//! every annotated class in a project carries it.
use super::common;

#[test]
fn annotated_class_ctor_line_table_matches_kotlinc() {
    // The annotation MUST be on its own line — that is the whole difference under test.
    let src = "annotation class Mark\n\
               \n\
               @Mark\n\
               data class Pair2(val x: Int, val y: String)\n";
    let Some(result) = common::byte_diff_against_kotlinc("AnnotatedDeclLine", src, "Pair2") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("Pair2 byte-identical to kotlinc");
}

/// The header and the declaration start coincide here, so this pins that the fix changes nothing
/// for an unannotated class — including the trailing `return` entry, which a multi-line header
/// makes visible as a separate line.
#[test]
fn unannotated_multiline_header_ctor_line_table_is_unchanged() {
    let src = "data class Multi(\n\
               \x20   val a: Int,\n\
               \x20   val b: String,\n\
               )\n";
    let Some(result) = common::byte_diff_against_kotlinc("MultilineHeader", src, "Multi") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("Multi byte-identical to kotlinc");
}
