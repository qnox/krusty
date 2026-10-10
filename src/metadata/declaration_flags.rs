//! Metadata flag words for classes and their declared functions, read from the IR's recorded
//! declaration facts.

use crate::ir::{IrDataClassMemberRole, IrFile};

pub(crate) fn declaration_visibility_bits(visibility: crate::types::Visibility) -> u64 {
    match visibility {
        crate::types::Visibility::Internal => 0,
        crate::types::Visibility::Private => 1,
        crate::types::Visibility::Protected => 2,
        crate::types::Visibility::Public => 3,
        crate::types::Visibility::PackagePrivate => {
            unreachable!("package-private is not a Kotlin declaration visibility")
        }
    }
}

pub(crate) fn class_flags(ir: &IrFile, c: &crate::ir::IrClass) -> u64 {
    // Visibility bits: INTERNAL=0, PRIVATE=1, PROTECTED=2, PUBLIC=3 — an `internal class` must
    // record explicit 0 so a consumer enforces the module boundary; synthesized classes without a
    // recorded visibility stay public.
    // A classifier declared in executable code, or nested in one, is LOCAL (5) whatever it says.
    // So is an enum entry's body: kotlinc models it as an anonymous object, although its class id
    // stays the entry's `Enum.ENTRY` name.
    let visibility: u64 = match ir.class_visibilities.get(&c.fq_name_id()) {
        _ if super::local_classifiers::is_local(ir, c) || c.is_enum_entry => 5,
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
    } else if c.is_enum {
        2
    } else if c.is_enum_entry {
        3
    } else if c.is_companion {
        6
    } else if c.is_object {
        5
    } else {
        0
    };
    // `hasAnnotations` (bit 0) is the metadata writer's, from the class's DECLARED annotations: the
    // `@JvmInline` the class file implies for a legacy `inline class` is not one (kotlinc: 134).
    (visibility << 1)
        | (modality << 4)
        | (kind << 6)
        // `IS_INNER` (bit 9): an `inner class` — the record is how a consumer knows construction
        // takes the enclosing instance (kotlinc: `inner class Item` flags 518).
        | (u64::from(c.is_inner_class) << 9)
        | (u64::from(c.is_data) << 10)
        | (u64::from(c.is_value) << 13)
        | (u64::from(c.is_fun_interface) << 14)
        | (u64::from(c.is_enum) << 15)
}

/// `Function.flags` (proto field 9) — ONE bitfield like [`class_flags`], not a per-shape
/// constant. Decoded from kotlinc 2.4.0 (copy 198, componentN 454, hashCode/toString 65750, equals
/// 66006): bit0 hasAnnotations | bits1-3 visibility (PUBLIC=3, PRIVATE=1) | bits4-5 modality
/// (FINAL=0, OPEN=1, ABSTRACT=2) | bits6-7 memberKind (DECLARATION=0, DELEGATION=2,
/// SYNTHESIZED=3) | bit8
/// isOperator | bit9 isInfix.
/// Used for a class's REAL declared members and its interface-delegation forwarders; the
/// data/value-class synthesized sets keep their own (already kotlinc-verified) constants.
pub(crate) fn function_flags(ir: &IrFile, fid: u32, f: &crate::ir::IrFunction) -> u64 {
    let visibility = declaration_visibility_bits(ir.method_visibility(fid));
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
    let tailrec = u64::from(ir.tailrec_fns.contains(&fid)) << 11;
    let return_value_status = ir.fn_return_value_statuses.get(&fid).map_or(0, |status| {
        status.metadata_value() << crate::metadata::function_flags::RETURN_VALUE_STATUS_SHIFT
    });
    // A `companion { … }` member is companion-associated, as kotlinc records it.
    let companion = if ir.companion_blocks.is_function(fid) {
        crate::metadata::function_flags::IS_STATIC
    } else {
        0
    };
    // `memberKind` (bits 6-7). A source declaration is DECLARATION (0). An `interface by`
    // forwarder is DELEGATION (2); the override edge names that function.
    let member_kind = if ir.is_interface_delegation_function(fid) {
        crate::metadata::function_flags::MEMBER_KIND_DELEGATION
    } else {
        0
    };
    (visibility << 1)
        | (modality << 4)
        | member_kind
        | operator
        | infix
        | inline
        | tailrec
        | return_value_status
        | companion
}

/// `Function.flags` for one synthesized `componentN`: `COMPONENT_FN_FLAGS` with the visibility
/// bits swapped to the constructor property's visibility. An internal component stays operator,
/// final, and synthesized, and records internal (0) rather than public (3). `None` when the class
/// generates no such member.
pub(crate) fn data_component_flags(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    ordinal: u32,
) -> Option<u64> {
    use crate::metadata::class_builder::COMPONENT_FN_FLAGS;
    let fid = ir.data_class_member(c.fq_name_id(), IrDataClassMemberRole::Component(ordinal))?;
    let visibility = declaration_visibility_bits(ir.method_visibility(fid));
    Some(
        (COMPONENT_FN_FLAGS & !crate::metadata::property_flags::VISIBILITY_MASK)
            | (visibility << 1),
    )
}

/// `Function.flags` for the synthesized `copy`: `COPY_FN_FLAGS` (public final SYNTHESIZED member)
/// with the visibility bits swapped to the copy's actual visibility — the primary constructor's
/// under `DataClassCopyRespectsConstructorVisibility` (kotlinc 2.4.10: private ctor → 0xC2).
pub(crate) fn data_copy_flags(ir: &IrFile, c: &crate::ir::IrClass) -> u64 {
    use crate::metadata::class_builder::COPY_FN_FLAGS;
    let Some(fid) = ir.data_class_member(c.fq_name_id(), IrDataClassMemberRole::Copy) else {
        return COPY_FN_FLAGS;
    };
    let visibility = declaration_visibility_bits(ir.method_visibility(fid));
    (COPY_FN_FLAGS & !crate::metadata::property_flags::VISIBILITY_MASK) | (visibility << 1)
}

#[cfg(test)]
mod tests {
    use super::function_flags;
    use crate::ir::IrFile;

    #[test]
    fn member_metadata_flags_keep_inline_operator_and_infix_capabilities() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(crate::ir::IrFunction {
            name: "convention".into(),
            params: Vec::new(),
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: false,
            dispatch_receiver: Some(crate::types::type_name("demo/Owner")),
            param_checks: Vec::new(),
        });
        ir.inline_fns.insert(function);
        ir.operator_fns.insert(function);
        ir.infix_fns.insert(function);

        let flags = function_flags(&ir, function, &ir.functions[function as usize]);
        assert_ne!(flags & (1 << 8), 0);
        assert_ne!(flags & (1 << 9), 0);
        assert_ne!(flags & (1 << 10), 0);
    }
}
