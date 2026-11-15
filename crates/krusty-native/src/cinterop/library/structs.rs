//! A C struct or union as Kotlin/Native binds it: a `CStructVar` class whose properties read and
//! write the fields in place, and a companion that records the size and alignment.
//!
//! The fields of an anonymous struct or union member are the outer record's own, at their offset
//! in it. Ordinary fields come first, in declaration order, then the bitfields. A field whose type
//! has no Kotlin representation is left out.

use super::super::layout::LayoutEngine;
use super::super::model::{RecordId, Type, TypeId};
use super::kotlin_type::{cinterop_class, native_ptr};
use super::mirror::{Mapper, Mirror};
use super::spelling::record_spelling;
use super::stub::{flags, Annotation, Class, Constructor, Parameter, Property, Value};

struct Member<'d> {
    name: &'d str,
    ty: TypeId,
    /// In bytes, or in bits for a bitfield.
    offset: u64,
    width: Option<u64>,
}

/// The struct's class and its companion; `None` when the struct cannot be laid out.
pub(super) fn record_classes(
    mapper: &Mapper<'_>,
    layouts: &mut LayoutEngine,
    id: RecordId,
) -> Option<Vec<Class>> {
    let declarations = mapper.declarations;
    let layout = layouts.record(declarations, id).ok()?;
    let mut fields = Vec::new();
    let mut bitfields = Vec::new();
    members(mapper, layouts, id, 0, &mut fields, &mut bitfields)?;
    let name = mapper.record_class(id);
    let mut class = Class::new(name, Some(flags::CLASS));
    class.companion = Some("Companion");
    class.supertypes = vec![cinterop_class("CStructVar")];
    class.nested = vec!["Companion"];
    class.constructors = vec![raw_pointer_constructor()];
    for field in fields.iter().chain(&bitfields) {
        if let Some(property) = field_property(mapper, field) {
            class.properties.push(property);
        }
    }
    if let Some(spelling) = record_spelling(declarations, layouts, id) {
        class.annotations.push(Annotation::new(
            "kotlinx/cinterop/internal/CStruct",
            vec![("spelling", Value::String(spelling))],
        ));
    }
    class.annotations.push(Annotation::experimental());

    let mut companion = Class::new(name.nested_child("Companion"), Some(flags::COMPANION));
    companion.supertypes = vec![cinterop_class("CStructVar.Type")];
    companion.constructors = vec![private_constructor()];
    companion.annotations = vec![
        Annotation::new(
            "kotlinx/cinterop/internal/CStruct.VarType",
            vec![
                ("size", Value::long(layout.size)),
                ("align", Value::int(layout.align)),
            ],
        ),
        Annotation::deprecated("Use sizeOf<T>() or alignOf<T>() instead."),
    ];
    Some(vec![class, companion])
}

/// `constructor(rawPtr: NativePtr)`, every variable class's.
pub(super) fn raw_pointer_constructor() -> Constructor {
    Constructor {
        flags: None,
        parameters: vec![Parameter::new("rawPtr", native_ptr())],
    }
}

pub(super) fn private_constructor() -> Constructor {
    Constructor {
        flags: Some(flags::PRIVATE_CONSTRUCTOR),
        parameters: Vec::new(),
    }
}

/// The named members of `id` starting at `base` bits into the outermost record, an anonymous
/// member's in its place.
fn members<'d>(
    mapper: &Mapper<'d>,
    layouts: &mut LayoutEngine,
    id: RecordId,
    base: u64,
    fields: &mut Vec<Member<'d>>,
    bitfields: &mut Vec<Member<'d>>,
) -> Option<()> {
    let declarations = mapper.declarations;
    let layout = layouts.record(declarations, id).ok()?;
    let record_fields = declarations.record(id).fields.as_ref()?;
    for (field, &offset) in record_fields.iter().zip(&layout.field_offsets) {
        let offset = base + offset;
        match (&field.name, field.bit_width) {
            (Some(name), Some(width)) => bitfields.push(Member {
                name,
                ty: field.ty,
                offset,
                width: Some(width),
            }),
            (None, Some(_)) => {}
            (Some(name), None) => fields.push(Member {
                name,
                ty: field.ty,
                offset: offset / 8,
                width: None,
            }),
            (None, None) => {
                if let Type::Record(inner) = declarations.ty(declarations.canonical(field.ty)) {
                    members(mapper, layouts, *inner, offset, fields, bitfields)?;
                }
            }
        }
    }
    Some(())
}

fn field_property(mapper: &Mapper<'_>, member: &Member<'_>) -> Option<Property> {
    let declarations = mapper.declarations;
    let mirror = mapper.mirror(member.ty).ok()?;
    let at = |annotation: &str, arguments: Vec<(&'static str, Value)>| {
        vec![Annotation::new(
            &format!("kotlinx/cinterop/internal/CStruct.{annotation}"),
            arguments,
        )]
    };
    let property = |ty, var: bool, accessor: Vec<Annotation>| Property {
        name: member.name.to_string(),
        flags: if var {
            flags::MEMBER_VAR
        } else {
            flags::MEMBER_VAL
        },
        ty,
        getter_flags: flags::ANNOTATED_ACCESSOR,
        setter_flags: var.then_some(flags::ANNOTATED_ACCESSOR),
        annotations: Vec::new(),
        getter_annotations: accessor.clone(),
        setter_annotations: if var { accessor } else { Vec::new() },
        constant: None,
    };
    if let Some(width) = member.width {
        let Mirror::ByValue { .. } = mirror else {
            return None;
        };
        let accessor = at(
            "BitField",
            vec![
                ("offset", Value::long(member.offset)),
                ("size", Value::int(width)),
            ],
        );
        return Some(property(mirror.argument(), true, accessor));
    }
    let offset = vec![("offset", Value::long(member.offset))];
    if let Type::Array { .. } = declarations.ty(declarations.canonical(member.ty)) {
        let Mirror::ByValue { value, .. } = mirror else {
            return None;
        };
        return Some(property(value, false, at("ArrayMemberAt", offset)));
    }
    Some(match mirror {
        Mirror::ByValue { .. } => property(mirror.argument(), true, at("MemberAt", offset)),
        Mirror::ByRef { pointed } => property(pointed, false, at("MemberAt", offset)),
    })
}
