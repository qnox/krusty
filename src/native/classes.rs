//! Object layout and virtual-dispatch tables for the classes of one IR file.
//!
//! Everything here is a pure function over an [`IrFile`]; nothing prints C. The emitter asks
//! "where is this field" and this module answers from the IR's own declarations.
//!
//! **Layout** is the classic single-inheritance one: the object header, then the superclass's
//! fields as a prefix in the superclass's order, then this class's fields in declaration order,
//! each aligned to its C size, and the whole rounded up to 8. Because the layout is computed here
//! and not by the C compiler, the emitter asserts every offset with `_Static_assert` in the
//! generated C — a disagreement is a compile error instead of a collector that follows the wrong
//! word.
//!
//! **Refusals.** Constructs this step does not lower — inheritance of any kind (there is no
//! virtual dispatch yet, so a call through a base-typed value could not reach an override),
//! interfaces, data classes, `inner` classes, enums, secondary constructors, constructor defaults
//! — are reported by name from [`check_supported`] so the file declines with a diagnostic instead
//! of emitting something unverified.

use crate::ir::{ClassId, IrClass, IrFile};
use crate::types::Ty;

/// The construct a lowering declined, phrased for a diagnostic.
pub(super) type Unsupported = String;

/// How a Kotlin type is carried in C.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CKind {
    /// `void` — Kotlin `Unit` in return position.
    Void,
    /// A machine scalar, spelled as one of the runtime's `kt_*` typedefs.
    Scalar(&'static str),
    /// A `KRef`: every reference, including a boxed `Int?` and an erased type parameter.
    Ref,
}

impl CKind {
    pub(super) fn spelling(self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Scalar(name) => name,
            Self::Ref => "KRef",
        }
    }

    /// The carrier's size in bytes, which is also its alignment on every supported target.
    pub(super) fn size(self) -> u32 {
        match self {
            Self::Void => 0,
            Self::Scalar("kt_boolean" | "kt_byte") => 1,
            Self::Scalar("kt_short" | "kt_char") => 2,
            Self::Scalar("kt_int" | "kt_float") => 4,
            Self::Scalar(_) | Self::Ref => 8,
        }
    }
}

/// The C carrier for a Kotlin type. A nullable primitive is deliberately NOT a scalar: `Int?` has
/// to represent `null`, so it boxes, exactly as it does on the JVM. A type parameter erases to a
/// reference, as it does there too.
pub(super) fn c_kind(ty: Ty) -> CKind {
    match ty {
        Ty::Unit => CKind::Void,
        Ty::Boolean => CKind::Scalar("kt_boolean"),
        Ty::Byte => CKind::Scalar("kt_byte"),
        Ty::Short => CKind::Scalar("kt_short"),
        Ty::Int => CKind::Scalar("kt_int"),
        Ty::Long => CKind::Scalar("kt_long"),
        Ty::Char => CKind::Scalar("kt_char"),
        Ty::Float => CKind::Scalar("kt_float"),
        Ty::Double => CKind::Scalar("kt_double"),
        _ => CKind::Ref,
    }
}

/// The size of an object header: one pointer to the type.
pub(super) const HEADER_SIZE: u32 = 8;

/// Where one of a class's OWN fields lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FieldLayout {
    pub offset: u32,
    pub kind: CKind,
}

#[derive(Clone, Debug)]
pub(super) struct ClassLayout {
    /// The superclass in this file, or `None` for a class extending `kotlin.Any`.
    pub superclass: Option<ClassId>,
    /// Parallel to `IrClass::fields`.
    pub fields: Vec<FieldLayout>,
    /// Bytes including the header, rounded up to 8.
    pub instance_size: u32,
    /// Every reference field, inherited ones first — what the collector traces.
    pub reference_offsets: Vec<u32>,
}

/// The layouts of every class in a file, indexed by `ClassId`, plus the order in which a
/// superclass precedes its subclasses.
#[derive(Debug)]
pub(super) struct ClassModel {
    pub layouts: Vec<ClassLayout>,
    pub order: Vec<ClassId>,
}

impl ClassModel {
    pub(super) fn layout(&self, class: ClassId) -> &ClassLayout {
        &self.layouts[class as usize]
    }
}

/// Reject, by name, a class this step does not lower.
pub(super) fn check_supported(ir: &IrFile, class: &IrClass) -> Result<(), Unsupported> {
    let name = class.fq_name();
    let overrides_any = class.methods.iter().any(|&fid| {
        let function = &ir.functions[fid as usize];
        matches!(
            (function.name.as_str(), function.params.len()),
            ("toString", 0) | ("hashCode", 0) | ("equals", 1)
        )
    });
    let construct = if class.is_interface {
        "an interface"
    } else if !class.superclass.matches("kotlin/Any") {
        "a superclass"
    } else if class.is_open || class.is_abstract || class.is_sealed {
        "an open class"
    } else if overrides_any {
        "an override of a `kotlin.Any` member"
    } else if class.properties.iter().any(|property| property.is_open) {
        "an open property"
    } else if class
        .methods
        .iter()
        .any(|&fid| ir.functions[fid as usize].body.is_none())
    {
        "an abstract method"
    } else if !class.interfaces.is_empty()
        || class.supertypes.iter().any(|supertype| {
            supertype
                .obj_internal()
                .and_then(|internal| ir.class_id_by_name(internal))
                .is_some_and(|id| ir.classes[id as usize].is_interface)
        })
    {
        "an interface supertype"
    } else if class.is_data {
        "a data class"
    } else if class.is_inner_class || class.constructor_prefix_count > 0 {
        "an inner class"
    } else if !class.enum_entries.is_empty() || class.enum_entry_of.is_some() {
        "an enum class"
    } else if !class.secondary_ctors.is_empty() {
        "a secondary constructor"
    } else if ir
        .class_ctor_defaults_name(class.fq_name_id())
        .is_some_and(|defaults| defaults.iter().any(Option::is_some))
        || class.ctor_args.iter().any(|argument| argument.has_default)
    {
        "a constructor default argument"
    } else if class.is_value {
        "a value class"
    } else if class.is_annotation || class.annotation_impl_of.is_some() {
        "an annotation class"
    } else if class.is_anonymous_object
        || class.enclosing_function.is_some()
        || class.is_local_class
    {
        "a local or anonymous class"
    } else if class.prop_ref.is_some() || class.func_ref.is_some() {
        "a callable reference"
    } else if !class.pre_super_param_fields.is_empty() {
        "a field stored before the superclass constructor"
    } else if class.ctor_args.iter().any(|argument| argument.is_vararg) {
        "a vararg constructor parameter"
    } else {
        return Ok(());
    };
    Err(format!("{construct} (`{name}`)"))
}

/// Build the layout and vtable of every class in `ir`, superclasses first.
pub(super) fn build(ir: &IrFile) -> Result<ClassModel, Unsupported> {
    for class in &ir.classes {
        check_supported(ir, class)?;
    }
    let order = hierarchy_order(ir)?;

    let mut layouts: Vec<Option<ClassLayout>> = vec![None; ir.classes.len()];
    for &id in &order {
        let class = &ir.classes[id as usize];
        let superclass = ir.class_id_by_name(class.superclass);
        let layout = {
            let parent = superclass.map(|parent| {
                layouts[parent as usize]
                    .as_ref()
                    .expect("superclasses are laid out first")
            });
            layout_class(ir, id, superclass, parent)?
        };
        layouts[id as usize] = Some(layout);
    }
    Ok(ClassModel {
        layouts: layouts
            .into_iter()
            .map(|layout| layout.expect("every class was laid out"))
            .collect(),
        order,
    })
}

/// Classes in IR order. Kept as an explicit order so a superclass can precede its subclasses once
/// inheritance lands; today every class extends `kotlin.Any` directly.
fn hierarchy_order(ir: &IrFile) -> Result<Vec<ClassId>, Unsupported> {
    Ok((0..ir.classes.len() as ClassId).collect())
}

fn round_up(value: u32, alignment: u32) -> u32 {
    (value + alignment - 1) / alignment * alignment
}

fn layout_class(
    ir: &IrFile,
    id: ClassId,
    superclass: Option<ClassId>,
    parent: Option<&ClassLayout>,
) -> Result<ClassLayout, Unsupported> {
    let class = &ir.classes[id as usize];

    // ---- fields ----
    let mut end = parent.map_or(HEADER_SIZE, |parent| parent.instance_size);
    let mut reference_offsets =
        parent.map_or_else(Vec::new, |parent| parent.reference_offsets.clone());
    let mut fields = Vec::with_capacity(class.fields.len());
    for field in &class.fields {
        let kind = c_kind(field.ty);
        if kind == CKind::Void {
            return Err(format!(
                "a `Unit`-typed field (`{}.{}`)",
                class.fq_name(),
                field.name
            ));
        }
        let offset = round_up(end, kind.size());
        if kind == CKind::Ref {
            reference_offsets.push(offset);
        }
        fields.push(FieldLayout { offset, kind });
        end = offset + kind.size();
    }
    let instance_size = round_up(end, 8);

    Ok(ClassLayout {
        superclass,
        fields,
        instance_size,
        reference_offsets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrField, IrfFlags};

    fn field(name: &str, ty: Ty) -> IrField {
        IrField {
            name: name.to_string(),
            ty,
            type_param: None,
            default: None,
            flags: IrfFlags::default(),
        }
    }

    fn class(ir: &mut IrFile, name: &str) -> ClassId {
        let class = IrClass::synthetic(crate::types::type_name(name));
        let id = ir.classes.len() as ClassId;
        ir.classes.push(class);
        id
    }

    #[test]
    fn a_class_with_no_fields_is_just_a_header() {
        let mut ir = IrFile::default();
        let point = class(&mut ir, "Point");
        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(point).instance_size, HEADER_SIZE);
        assert!(model.layout(point).reference_offsets.is_empty());
    }

    #[test]
    fn fields_follow_the_header_and_align_to_their_size() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A");
        ir.classes[a as usize].fields = vec![
            field("flag", Ty::Boolean),
            field("count", Ty::Int),
            field("next", Ty::nullable(Ty::obj("A"))),
            field("small", Ty::Short),
            field("wide", Ty::Long),
            field("text", Ty::String),
        ];

        let model = build(&ir).expect("layout");
        let layout = model.layout(a);
        assert_eq!(layout.fields[0].offset, 8);
        assert_eq!(
            layout.fields[1].offset, 12,
            "an Int after a Boolean aligns to 4"
        );
        assert_eq!(layout.fields[2].offset, 16, "a reference aligns to 8");
        assert_eq!(layout.fields[3].offset, 24);
        assert_eq!(
            layout.fields[4].offset, 32,
            "a Long after a Short aligns to 8"
        );
        assert_eq!(layout.fields[5].offset, 40);
        assert_eq!(layout.instance_size, 48, "rounded up to 8");
        assert_eq!(
            layout.reference_offsets,
            vec![16, 40],
            "exactly the reference fields, in layout order — what the collector traces"
        );
    }

    #[test]
    fn a_trailing_scalar_rounds_the_instance_up_to_eight() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A");
        ir.classes[a as usize].fields = vec![field("n", Ty::Int)];
        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(a).fields[0].offset, 8);
        assert_eq!(model.layout(a).instance_size, 16);
    }

    #[test]
    fn out_of_scope_classes_are_declined_by_name() {
        let mut ir = IrFile::default();
        let id = class(&mut ir, "D");
        ir.classes[id as usize].is_data = true;
        assert!(build(&ir).expect_err("declined").contains("a data class"));
        ir.classes[id as usize].is_data = false;
        ir.classes[id as usize].is_interface = true;
        assert!(build(&ir).expect_err("declined").contains("an interface"));
        ir.classes[id as usize].is_interface = false;
        ir.classes[id as usize].is_inner_class = true;
        assert!(build(&ir).expect_err("declined").contains("an inner class"));
        ir.classes[id as usize].is_inner_class = false;
        ir.classes[id as usize].superclass = crate::types::type_name("Base");
        assert!(build(&ir).expect_err("declined").contains("a superclass"));
        ir.classes[id as usize].superclass = crate::types::type_name("kotlin/Any");
        ir.classes[id as usize].is_open = true;
        assert!(build(&ir).expect_err("declined").contains("an open class"));
    }
}
