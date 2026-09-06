//! JVM realization of shared mutable capture cells.
//!
//! Checked FIR and common IR keep the captured element's Kotlin type. A sparse identity edge marks
//! declaration slots that physically carry the shared cell; this pass realizes class fields and
//! constructor parameters as `kotlin.jvm.internal.Ref` holders without making that JVM choice part
//! of frontend semantics.

use std::collections::HashMap;

use crate::ir::{ClassId, IrExpr, IrFile};
use crate::types::Ty;

/// The JVM holder class and its `element` field descriptor for one captured element type.
pub(super) fn holder_class(elem: &Ty) -> (&'static str, &'static str) {
    if elem.is_nullable() {
        return ("kotlin/jvm/internal/Ref$ObjectRef", "Ljava/lang/Object;");
    }
    match elem.non_null() {
        Ty::Int | Ty::UInt => ("kotlin/jvm/internal/Ref$IntRef", "I"),
        Ty::Long | Ty::ULong => ("kotlin/jvm/internal/Ref$LongRef", "J"),
        Ty::Float => ("kotlin/jvm/internal/Ref$FloatRef", "F"),
        Ty::Double => ("kotlin/jvm/internal/Ref$DoubleRef", "D"),
        Ty::Boolean => ("kotlin/jvm/internal/Ref$BooleanRef", "Z"),
        Ty::Char => ("kotlin/jvm/internal/Ref$CharRef", "C"),
        Ty::Byte | Ty::UByte => ("kotlin/jvm/internal/Ref$ByteRef", "B"),
        Ty::Short | Ty::UShort => ("kotlin/jvm/internal/Ref$ShortRef", "S"),
        Ty::Obj(name, _) if name.matches("kotlin/Int") || name.matches("kotlin/UInt") => {
            ("kotlin/jvm/internal/Ref$IntRef", "I")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Long") || name.matches("kotlin/ULong") => {
            ("kotlin/jvm/internal/Ref$LongRef", "J")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Float") => {
            ("kotlin/jvm/internal/Ref$FloatRef", "F")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Double") => {
            ("kotlin/jvm/internal/Ref$DoubleRef", "D")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Boolean") => {
            ("kotlin/jvm/internal/Ref$BooleanRef", "Z")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Char") => ("kotlin/jvm/internal/Ref$CharRef", "C"),
        Ty::Obj(name, _) if name.matches("kotlin/Byte") || name.matches("kotlin/UByte") => {
            ("kotlin/jvm/internal/Ref$ByteRef", "B")
        }
        Ty::Obj(name, _) if name.matches("kotlin/Short") || name.matches("kotlin/UShort") => {
            ("kotlin/jvm/internal/Ref$ShortRef", "S")
        }
        _ => ("kotlin/jvm/internal/Ref$ObjectRef", "Ljava/lang/Object;"),
    }
}

pub(super) fn holder_ty(elem: &Ty) -> Ty {
    Ty::obj(holder_class(elem).0)
}

/// Replace only backend declaration slots. The marker map remains logical so metadata, diagnostics,
/// and non-JVM backends never observe `Ref$*Ref` as the captured source value's type.
pub(super) fn lower_class_capture_slots(ir: &mut IrFile) {
    let mut physical = HashMap::<ClassId, Vec<(usize, Ty)>>::new();
    for (&(class, field), _) in &ir.shared_class_capture_fields {
        let field = field as usize;
        let element = ir.classes[class as usize].fields[field].ty;
        physical
            .entry(class)
            .or_default()
            .push((field, holder_ty(&element)));
    }
    for slots in physical.values_mut() {
        slots.sort_unstable_by_key(|(field, _)| *field);
    }

    for (&class, slots) in &physical {
        let declaration = &mut ir.classes[class as usize];
        for &(field, holder) in slots {
            declaration.fields[field].ty = holder;
            let argument = &mut declaration.ctor_args[field];
            argument.ty = holder;
            argument.check = None;
            for constructor in &mut declaration.secondary_ctors {
                constructor.prefix_params[field] = holder;
            }
        }
    }

    let class_names = physical
        .iter()
        .map(|(&class, slots)| (ir.classes[class as usize].fq_name_id(), slots.clone()))
        .collect::<HashMap<_, _>>();
    for expression in &mut ir.exprs {
        let IrExpr::New {
            internal,
            ctor_params: Some(parameters),
            ..
        } = expression
        else {
            continue;
        };
        let Some(slots) = class_names.get(internal) else {
            continue;
        };
        for &(field, holder) in slots {
            parameters[field] = holder;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrClass, IrCtorArg, IrField};

    #[test]
    fn realizes_only_marked_class_capture_slots() {
        let mut ir = IrFile::default();
        let mut class = IrClass::synthetic(crate::types::type_name("Captured"));
        class.fields = vec![
            IrField::new("shared".to_string(), Ty::String),
            IrField::new("plain".to_string(), Ty::String),
        ];
        class.ctor_args = vec![
            IrCtorArg {
                name: Some("shared".to_string()),
                ty: Ty::String,
                declared_ty: None,
                is_field: true,
                field_index: None,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: Some("shared".to_string()),
            },
            IrCtorArg {
                name: Some("plain".to_string()),
                ty: Ty::String,
                declared_ty: None,
                is_field: true,
                field_index: None,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: Some("plain".to_string()),
            },
        ];
        let class = ir.add_class(class);
        ir.shared_class_capture_fields
            .insert((class, 0), Ty::String);

        lower_class_capture_slots(&mut ir);

        assert_eq!(
            ir.classes[class as usize].fields[0].ty,
            Ty::obj("kotlin/jvm/internal/Ref$ObjectRef")
        );
        assert_eq!(ir.classes[class as usize].fields[1].ty, Ty::String);
        assert_eq!(
            ir.classes[class as usize].ctor_args[0].ty,
            Ty::obj("kotlin/jvm/internal/Ref$ObjectRef")
        );
        assert_eq!(ir.classes[class as usize].ctor_args[1].ty, Ty::String);
        assert_eq!(
            ir.shared_class_capture_fields.get(&(class, 0)),
            Some(&Ty::String)
        );
    }

    #[test]
    fn nullable_primitive_uses_object_holder() {
        assert_eq!(
            holder_class(&Ty::nullable(Ty::Int)).0,
            "kotlin/jvm/internal/Ref$ObjectRef"
        );
    }
}
