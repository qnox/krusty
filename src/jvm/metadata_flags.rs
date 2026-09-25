//! `@Metadata` flag words for classes and their declared functions, read from the IR's recorded
//! declaration facts.

use crate::ir::IrFile;

pub(super) fn class_metadata_flags(ir: &IrFile, c: &crate::ir::IrClass) -> u64 {
    // Visibility bits: INTERNAL=0, PRIVATE=1, PROTECTED=2, PUBLIC=3 — an `internal class` must
    // record explicit 0 so a consumer enforces the module boundary; synthesized classes without a
    // recorded visibility stay public.
    // A classifier declared in executable code, or nested in one, is LOCAL (5) whatever it says.
    let visibility: u64 = match ir.class_visibilities.get(&c.fq_name_id()) {
        _ if super::local_classifiers::is_local(ir, c) => 5,
        Some(crate::types::Visibility::Internal) => 0,
        Some(crate::types::Visibility::Private) => 1,
        Some(crate::types::Visibility::Protected) => 2,
        _ => 3,
    };
    let modality: u64 = if c.is_sealed {
        3
    } else if c.is_abstract || c.is_interface {
        2
    } else if c.is_open {
        1
    } else {
        0
    };
    let kind: u64 = if c.is_annotation {
        4
    } else if c.is_interface {
        1
    } else if !c.enum_entries.is_empty() {
        2
    } else if c.enum_entry_of.is_some() {
        3
    } else if c.is_companion {
        6
    } else if c.is_object {
        5
    } else {
        0
    };
    // A value class carries `@JvmInline`, which sets `hasAnnotations`.
    let has_annotations = u64::from(c.is_value || !c.applied_annotations.is_empty());
    has_annotations
        | (visibility << 1)
        | (modality << 4)
        | (kind << 6)
        // `IS_INNER` (bit 9): an `inner class` — the record is how a consumer knows construction
        // takes the enclosing instance (kotlinc: `inner class Item` flags 518).
        | (u64::from(c.is_inner_class) << 9)
        | (u64::from(c.is_data) << 10)
        | (u64::from(c.is_value) << 13)
        | (u64::from(c.is_fun_interface) << 14)
        | (u64::from(!c.enum_entries.is_empty()) << 15)
}

/// `Function.flags` (proto field 9) — ONE bitfield like [`class_metadata_flags`], not a per-shape
/// constant. Decoded from kotlinc 2.4.0 (copy 198, componentN 454, hashCode/toString 65750, equals
/// 66006): bit0 hasAnnotations | bits1-3 visibility (PUBLIC=3, PRIVATE=1) | bits4-5 modality
/// (FINAL=0, OPEN=1, ABSTRACT=2) | bits6-7 memberKind (DECLARATION=0, SYNTHESIZED=3) | bit8
/// isOperator | bit9 isInfix.
/// Used for a class's REAL declared members; the data/value-class synthesized sets keep their own
/// (already kotlinc-verified) constants.
pub(super) fn function_flags(ir: &IrFile, fid: u32, f: &crate::ir::IrFunction) -> u64 {
    let visibility: u64 = if ir.private_methods.contains(&fid) {
        1
    } else if ir.internal_methods.contains(&fid) {
        0 // INTERNAL — only metadata carries the module boundary
    } else {
        3
    };
    let modality: u64 = if f.body.is_none() {
        2 // abstract (an interface method or an `abstract fun`)
    } else if ir.open_methods.contains(&fid) {
        1
    } else {
        0
    };
    // `isOperator` (bit 8) — only `@Metadata` carries it; without it a consumer rejects the
    // conventional call form (`recv(args)`, `a[i]`) with "expression is not callable", and
    // convention resolution (`getValue`/`provideDelegate`/`invoke`) cannot filter on it.
    let operator = u64::from(ir.operator_fns.contains(&fid)) << 8;
    // `isInfix` (bit 9) — same metadata-only channel as `isOperator`: without it a consumer
    // rejects the `a f b` call form.
    let infix = u64::from(ir.infix_fns.contains(&fid)) << 9;
    // `isInline` (bit 10) is a Kotlin declaration capability, not a bytecode access flag. It must
    // survive class metadata so downstream frontends can select and splice member inline bodies.
    let inline = u64::from(ir.inline_fns.contains(&fid)) << 10;
    let return_value_status = ir.fn_return_value_statuses.get(&fid).map_or(0, |status| {
        status.metadata_value() << crate::metadata::function_flags::RETURN_VALUE_STATUS_SHIFT
    });
    (visibility << 1) | (modality << 4) | operator | infix | inline | return_value_status
}
