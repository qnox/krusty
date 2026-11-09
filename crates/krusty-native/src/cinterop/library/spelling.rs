//! A struct's C spelling for its `@CStruct` annotation: an anonymous declaration of the same
//! layout, which a C compiler can read to pass the struct by value.
//!
//! Each field is spelled with a type of the same size and alignment — a pointer as `void*`, an
//! enum as its underlying integer type, `_Bool` as `char`, a typedef as what it names — and an
//! `aligned` attribute where its offset is not the one the type alone would give. A struct whose
//! fields are not all at their type's alignment is `packed`, and one aligned beyond its fields
//! says so. A struct with a bitfield or a flexible array member has no spelling.

use super::super::layout::{enum_underlying, LayoutEngine};
use super::super::model::{
    float_spelling, int_spelling, Declarations, IntRank, RecordId, RecordKind, Signedness, Type,
    TypeId,
};

/// The spelling of the defined struct or union `id`; `None` when it cannot be spelled.
pub(super) fn record_spelling(
    declarations: &Declarations,
    layouts: &mut LayoutEngine,
    id: RecordId,
) -> Option<String> {
    let record = declarations.record(id);
    let fields = record.fields.as_ref()?;
    let layout = layouts.record(declarations, id).ok()?;
    if record.kind == RecordKind::Union {
        let mut out = String::from("union { ");
        for field in fields {
            if field.bit_width.is_some() {
                return None;
            }
            let declaration = match &field.name {
                Some(name) => var(declarations, layouts, field.ty, name)?,
                None => anonymous_member(declarations, layouts, field.ty)?,
            };
            out.push_str(&declaration);
            out.push_str("; ");
        }
        out.push('}');
        return Some(out);
    }
    let mut type_layouts = Vec::with_capacity(fields.len());
    for field in fields {
        type_layouts.push(layouts.layout(declarations, field.ty).ok());
    }
    let packed = fields
        .iter()
        .zip(&layout.field_offsets)
        .zip(&type_layouts)
        .any(|((field, offset), type_layout)| {
            field.bit_width.is_none()
                && field.name.is_some()
                && type_layout.is_some_and(|type_layout| (offset / 8) % type_layout.align != 0)
        });
    let mut out = String::from("struct  { ");
    let mut end = 0u64;
    let mut natural_align = 1u64;
    for ((field, &offset_bits), type_layout) in
        fields.iter().zip(&layout.field_offsets).zip(&type_layouts)
    {
        if field.bit_width.is_some() {
            return None;
        }
        let offset = offset_bits / 8;
        let type_layout = (*type_layout)?;
        let declaration = match &field.name {
            None => {
                end = offset + type_layout.size;
                anonymous_member(declarations, layouts, field.ty)?
            }
            Some(name) => {
                let default_align = if packed { 1 } else { type_layout.align };
                natural_align = natural_align.max(default_align);
                let align = [default_align, 1, 2, 4, 8, 16, 32]
                    .into_iter()
                    .find(|&align| end.next_multiple_of(align) == offset)?;
                end = offset + type_layout.size;
                let mut declaration = var(declarations, layouts, field.ty, name)?;
                if align != default_align {
                    declaration.push_str(&format!(" __attribute__((aligned({align})))"));
                }
                declaration
            }
        };
        out.push_str(&declaration);
        out.push_str("; ");
    }
    out.push('}');
    if packed {
        out.push_str(" __attribute__((packed))");
    }
    if layout.align > natural_align {
        out.push_str(&format!(" __attribute__((aligned({})))", layout.align));
    }
    Some(out)
}

fn anonymous_member(
    declarations: &Declarations,
    layouts: &mut LayoutEngine,
    ty: TypeId,
) -> Option<String> {
    match declarations.ty(declarations.canonical(ty)) {
        Type::Record(inner) => record_spelling(declarations, layouts, *inner),
        _ => None,
    }
}

/// `ty name` with a type of the same layout as `ty`.
fn var(
    declarations: &Declarations,
    layouts: &mut LayoutEngine,
    ty: TypeId,
    name: &str,
) -> Option<String> {
    Some(match declarations.ty(ty) {
        Type::Qualified { base, .. } => return var(declarations, layouts, *base, name),
        Type::Typedef(id) => return var(declarations, layouts, declarations.typedef(*id).ty, name),
        Type::Int {
            rank: IntRank::Bool,
            ..
        }
        | Type::Int {
            rank: IntRank::Char,
            signedness: Signedness::Plain,
        } => format!("char {name}"),
        Type::Int { rank, signedness } => format!("{} {name}", int_spelling(*rank, *signedness)),
        Type::Float(kind) => format!("{} {name}", float_spelling(*kind)),
        Type::Enum(id) => {
            let (rank, signedness) = enum_underlying(declarations.enumeration(*id))?;
            format!("{} {name}", int_spelling(rank, signedness))
        }
        Type::Pointer(_) => format!("void* {name}"),
        Type::Array {
            element,
            length: Some(length),
        } => {
            return var(
                declarations,
                layouts,
                *element,
                &format!("{name}[{length}]"),
            )
        }
        Type::Record(id) => format!("{} {name}", record_spelling(declarations, layouts, *id)?),
        _ => return None,
    })
}
