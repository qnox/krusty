//! Kotlin @Metadata emission (protobuf payload + encoding). WIP — Phase 4b.

use protobuf::Pb;

pub mod builder;
pub mod class_builder;
pub mod encoding;
pub mod module;
pub mod protobuf;
mod type_encoder;

/// Canonical `ProtoBuf.Property.flags` layout shared by metadata readers and writers. Property flags
/// include the declaration's two-bit MEMBER_KIND after MODALITY, so property-specific bits must not be
/// copied from the shorter Function/Class prefix. Keeping the raw schema facts here prevents the JVM
/// decoder, class encoder, and package encoder from drifting into separate numeric interpretations.
pub(crate) mod property_flags {
    /// Public, final property with a default getter; protobuf omits field 11 at this value.
    pub const DEFAULT: u64 = 518;
    pub const VISIBILITY_MASK: u64 = 0b1110;
    pub const IS_VAR: u64 = 1 << 8;
    pub const HAS_SETTER: u64 = 1 << 10;
    pub const IS_CONST: u64 = 1 << 11;
    pub const HAS_CONSTANT: u64 = 1 << 13;
    pub const MODALITY_ABSTRACT: u64 = 1 << 5;
    /// `Property.getter_flags`/`setter_flags` for a DECLARED accessor: public (visibility 3 in bits
    /// 1-3) · final · `isNotDefault` (bit 6). The accessor flag word has its own layout — bit 0
    /// `hasAnnotations`, bits 1-3 visibility, bits 4-5 modality, bit 6 `isNotDefault`.
    pub const DECLARED_ACCESSOR: u64 = 70;
    /// A plain `public final` accessor's own flags word (`Property.getter_flags`/`setter_flags`).
    /// kotlinc emits an accessor's word only when it differs from the default it derives FROM THE
    /// PROPERTY — whose `hasAnnotations` bit rides along — so an annotated property with plain
    /// accessors writes this value explicitly.
    pub const DEFAULT_ACCESSOR: u64 = 6;
}

/// The `Type` a `vararg` parameter RECORDS: `Array<out E>`, not the invariant `Array<E>` the
/// checker carries.
///
/// A `vararg` accepts any subtype of its element, and kotlinc writes that covariance into the
/// recorded array's `Argument.projection`. A primitive specialized array (`vararg xs: Int` →
/// `IntArray`) has no type argument at all and is recorded unchanged. The element's own record stays
/// unprojected — it travels separately as `ValueParameter.vararg_element_type`.
pub(crate) fn vararg_recorded_type(ty: crate::types::Ty) -> crate::types::Ty {
    use crate::types::Ty;
    let Ty::Obj(name, args) = ty else { return ty };
    match args.first().copied() {
        Some(element) if ty.is_reference_array() && !matches!(element, Ty::OutProjection(_)) => {
            Ty::obj_args_name(name, &[Ty::out_projection(element)])
        }
        _ => ty,
    }
}

/// Whether a signature position forces the declaration to record an explicit
/// `JvmMethodSignature.desc` because its JVM descriptor is not recoverable from the recorded Kotlin
/// type.
///
/// A reader maps a `Type`'s CLASS NAME through a flat table (`kotlin/Int` → `I`,
/// `kotlin/collections/List` → `Ljava/util/List;`, `kotlin/IntArray` → `[I`). `kotlin/Array` has no
/// entry: its descriptor depends on the type ARGUMENT (`Array<String>` → `[Ljava/lang/String;`),
/// which a name-keyed table cannot express. So kotlinc records the descriptor for any signature
/// holding one anywhere — parameter, extension receiver, or return — a `vararg` of a reference
/// element included, since that is recorded as an `Array`. Only the OUTERMOST classifier counts: an
/// array nested inside another type (`List<Array<String>>`) is erased away before the descriptor.
pub(crate) fn descriptor_needs_recording(ty: crate::types::Ty) -> bool {
    ty.non_null().is_reference_array()
}

pub(crate) fn serialize_string_table_types(records: &[Pb], local_names: &[u32]) -> Pb {
    let mut out = Pb::new();
    let mut i = 0;
    while i < records.len() {
        if !records[i].is_empty() {
            out.repeated_message(1, &records[i]);
            i += 1;
            continue;
        }

        let mut end = i + 1;
        while end < records.len() && records[end].is_empty() {
            end += 1;
        }
        let mut record = Pb::new();
        if end - i > 1 {
            record.field_varint(1, (end - i) as u64);
        }
        out.repeated_message(1, &record);
        i = end;
    }
    // `StringTableTypes.localName` (packed field 5): the indices whose strings are LOCAL class
    // names — raw internal names used as class ids verbatim (kotlinc's anonymous-class encoding).
    if !local_names.is_empty() {
        let mut packed = Pb::new();
        for &index in local_names {
            packed.varint(index as u64);
        }
        out.field_bytes(5, packed.as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::types::Ty;

    #[test]
    fn a_reference_vararg_records_an_out_projected_array() {
        let array = Ty::obj_args("kotlin/Array", &[Ty::String]);
        assert_eq!(
            vararg_recorded_type(array),
            Ty::obj_args("kotlin/Array", &[Ty::out_projection(Ty::String)])
        );
        // Idempotent: an already-projected argument is not double-wrapped.
        assert_eq!(
            vararg_recorded_type(vararg_recorded_type(array)),
            vararg_recorded_type(array)
        );
    }

    #[test]
    fn a_primitive_vararg_array_is_recorded_unchanged() {
        // `vararg xs: Int` is an `IntArray` — no type argument, so nothing to project.
        let ints = Ty::obj("kotlin/IntArray");
        assert_eq!(vararg_recorded_type(ints), ints);
    }

    #[test]
    fn only_a_reference_array_forces_a_recorded_descriptor() {
        // `kotlin/Array`'s descriptor depends on its argument, so it must be recorded — through a
        // `?` as well. Everything a name-keyed table maps needs nothing.
        assert!(descriptor_needs_recording(Ty::obj_args(
            "kotlin/Array",
            &[Ty::String]
        )));
        assert!(descriptor_needs_recording(Ty::nullable(Ty::obj_args(
            "kotlin/Array",
            &[Ty::String]
        ))));
        assert!(!descriptor_needs_recording(Ty::obj("kotlin/IntArray")));
        assert!(!descriptor_needs_recording(Ty::Int));
        assert!(!descriptor_needs_recording(Ty::obj_args(
            "kotlin/collections/List",
            &[Ty::obj_args("kotlin/Array", &[Ty::String])]
        )));
    }

    #[test]
    fn string_table_types_merge_only_plain_record_runs() {
        let plain = Pb::new();
        let mut predefined = Pb::new();
        predefined.field_varint(2, 8);
        let mut operation = Pb::new();
        operation.field_varint(3, 2);

        let encoded = serialize_string_table_types(
            &[
                plain.clone(),
                plain,
                predefined,
                operation.clone(),
                operation,
            ],
            &[],
        );

        assert_eq!(
            encoded.as_bytes(),
            &[
                0x0a, 0x02, 0x08, 0x02, // two plain records
                0x0a, 0x02, 0x10, 0x08, // predefined index
                0x0a, 0x02, 0x18, 0x02, // operation
                0x0a, 0x02, 0x18, 0x02, // identical operation stays separate
            ]
        );
    }
}
