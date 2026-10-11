//! Object layout and virtual-dispatch tables for the classes of one IR file, as Native lays them
//! out in memory.
//!
//! Which fields precede a class's own, and which slot each member dispatches through, are the
//! shared class tables' answers (`crate::backend::class_tables`); this module adds what is Native's
//! own: the byte offset of every field, the descriptor's reference offsets, and what realizes each
//! slot in the runtime — `kotlin.Any`'s members by runtime symbol, and the members of annotation
//! instances and value-class boxes, which Native synthesizes.
//!
//! **Layout** is the classic single-inheritance one: the object header, then the superclass's
//! fields as a prefix in the superclass's order, then this class's fields in declaration order,
//! each aligned to its size, and the whole rounded up to 8. A subclass's first field follows the
//! superclass's last one directly, not the superclass's rounded size. The same offsets feed both
//! the loads and stores the generator emits and the `reference_offsets` table in the class's
//! descriptor, so the program and the collector cannot disagree about where a reference is.
//!
//! **Refusals.** [`check_supported`] declines, by name, the two class kinds this model does not lay
//! out — an annotation's implementation class and a callable reference's class — and the shared
//! tables decline a superclass declared in another file and an override of a method declared
//! outside it, so the file declines with a diagnostic instead of emitting something unverified.

use super::value_classes::NativeValueClasses;
use std::collections::HashMap;

use crate::backend::class_tables::{self, ClassTable, Representation};
use crate::ir::{ClassId, FunId, IrClass, IrFile};
use crate::types::{Ty, TypeName};

pub(super) use crate::backend::class_tables::{
    function_key, local_property_target, AnyMember, SlotKey, FUNCTION_SLOT,
};

/// The construct a lowering declined, phrased for a diagnostic.
pub(super) type Unsupported = String;

/// How a Kotlin type is carried in memory, named by the runtime's `kt_*` typedef for the scalar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CKind {
    /// Kotlin `Unit` in return position: no value.
    Void,
    /// A machine scalar, named by the runtime's `kt_*` typedef.
    Scalar(&'static str),
    /// A `KRef`: every reference, including a boxed `Int?` and an erased type parameter.
    Ref,
}

impl CKind {
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

/// The carrier for a Kotlin type as this target carries it: a value class is its underlying value
/// wherever the representation policy projects it (see `native::value_classes`).
pub(super) fn c_kind(values: &NativeValueClasses, ty: Ty) -> CKind {
    machine_kind(values.project(ty))
}

/// How a value of `ty` is REPRESENTED: its carrier, and the value class it is carried unboxed
/// for, if any. Two signatures agree only where both do — a value class over `Any` and `Any` share
/// a carrier, and still differ, because one is the value and the other a box of it.
pub(super) fn representation(
    values: &NativeValueClasses,
    ty: Ty,
) -> (CKind, Option<crate::types::TypeName>) {
    (c_kind(values, ty), values.unboxed(ty))
}

/// The carrier for a type already projected. A nullable primitive is deliberately NOT a scalar:
/// `Int?` has to represent `null`, so it boxes, exactly as it does on the JVM. A type parameter
/// erases to a reference, as it does there too.
fn machine_kind(ty: Ty) -> CKind {
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

/// `kotlin.Enum`'s own storage, which every enum class carries ahead of its own fields: the
/// constant's NAME and its position among the constants. Kotlin reads both through `name` and
/// `ordinal`, and `toString` answers with the name. The enum's superclass is not a class in this
/// file — it is the language's own — so these offsets are the one place the layout of that base is
/// written down.
pub(super) const ENUM_NAME_OFFSET: u32 = HEADER_SIZE;
pub(super) const ENUM_ORDINAL_OFFSET: u32 = HEADER_SIZE + 8;
pub(super) const ENUM_FIELDS_END: u32 = HEADER_SIZE + 12;

/// The field a value class's box holds its value in: the exact backing-field coordinate common IR
/// recorded on its sole stored property. Never chosen by position or recovered from the property's
/// spelling; a declaration with no coordinate or more than one declines.
pub(super) fn value_field(ir: &IrFile, class: ClassId) -> Result<u32, Unsupported> {
    let declaration = &ir.classes[class as usize];
    let name = declaration.fq_name();
    let mut coordinates = declaration
        .properties
        .iter()
        .filter_map(|property| property.backing_field);
    let field = coordinates
        .next()
        .ok_or_else(|| format!("a value class whose property has no recorded field (`{name}`)"))?;
    if coordinates.next().is_some() {
        return Err(format!(
            "a single-field value class with more than one recorded property field (`{name}`)"
        ));
    }
    declaration
        .fields
        .get(field as usize)
        .map(|_| field)
        .ok_or_else(|| format!("a value class whose property field is absent (`{name}`)"))
}

/// The type a field is STORED at. A value class's box holds its value at the underlying type its
/// checked declaration states, which a generic one's IR field does not spell: `value class W<T :
/// Int>(val v: T)` holds an `Int` in a field the IR types `T`. Every other field is stored at its
/// own physical type.
pub(super) fn field_storage_ty(
    values: &NativeValueClasses,
    ir: &IrFile,
    class: ClassId,
    index: u32,
) -> Result<Ty, Unsupported> {
    let declaration = &ir.classes[class as usize];
    if values.is_value_class(declaration.fq_name) && value_field(ir, class)? == index {
        return values.underlying(declaration.fq_name).ok_or_else(|| {
            format!(
                "a value class with no checked underlying type (`{}`)",
                declaration.fq_name()
            )
        });
    }
    Ok(super::captures::physical_ty(
        ir,
        class,
        index,
        declaration.fields[index as usize].ty,
    ))
}

/// Whether a class is an enum: it extends `kotlin.Enum`, which no file declares.
pub(super) fn is_enum(class: &IrClass) -> bool {
    class.superclass == crate::types::wk::kotlin_enum()
}

/// A base class no file declares, whose layout the RUNTIME owns.
///
/// `kotlin.Enum` is the same idea spelled out above, and this is the rest of the family: a source
/// class may extend `kotlin.Throwable` or any of the exceptions Kotlin declares under it, and the
/// runtime already carries a `KType` and a storage layout for each — that is what makes `catch (e:
/// Exception)` take such a subclass without anything further, since matching a clause walks the
/// `base` chain those descriptors already form.
#[derive(Clone, Copy)]
pub(super) struct ExternalBase {
    /// The runtime's `KType` for this class, by data symbol.
    pub descriptor: &'static str,
    /// The first byte after the base's own storage — where a subclass's fields begin.
    pub fields_end: u32,
    /// Reference fields the base contributes, as offsets the collector traces.
    pub reference_offsets: &'static [u32],
    /// What the base puts in `kotlin.Any`'s three slots, by runtime symbol.
    pub any_slots: [&'static str; ANY_SLOTS as usize],
}

/// `kotlin.Throwable`'s own storage: the header, then `message` and `cause`. It has to agree with
/// `struct KThrowable` in the runtime, which is what a subclass's own fields are laid out after.
const THROWABLE_MESSAGE_OFFSET: u32 = HEADER_SIZE;
pub(super) const THROWABLE_CAUSE_OFFSET: u32 = HEADER_SIZE + 8;
const THROWABLE_FIELDS_END: u32 = HEADER_SIZE + 16;
const THROWABLE_REFERENCE_OFFSETS: &[u32] = &[THROWABLE_MESSAGE_OFFSET, THROWABLE_CAUSE_OFFSET];

/// The base `superclass` names, when it is one the runtime owns rather than one this file declares.
///
/// Which classes those are, and under which of the two providers' spellings they arrive, is
/// [`crate::native::intrinsics::throwable_descriptor`]'s to know — this adds only the LAYOUT, which
/// is a fact about emitting a subclass rather than about naming the base.
pub(super) fn external_base(superclass: TypeName) -> Option<ExternalBase> {
    // `kotlin.Number` carries NO state — every member it declares is an abstract conversion — so a
    // subclass of it is laid out exactly as a subclass of `kotlin.Any` is, and it contributes
    // `Any`'s own three slots. A caller reaching one of those conversions through a `Number`
    // receiver is a separate question, answered where every runtime-known member is: the file knows
    // the classes of its own that could stand behind the type, and a conversion takes no arguments.
    if super::intrinsics::is_number_base(superclass) {
        return Some(ExternalBase {
            descriptor: "kt_type_number",
            fields_end: HEADER_SIZE,
            reference_offsets: &[],
            any_slots: ["kt_any_equals", "kt_any_hash_code", "kt_any_to_string"],
        });
    }
    // Each of these wears `KThrowable`'s layout and `Throwable`'s own `toString`; only the
    // descriptor — and so the position in the `catch`-matching chain — differs.
    let descriptor = super::intrinsics::throwable_descriptor(superclass)?;
    Some(ExternalBase {
        descriptor,
        fields_end: THROWABLE_FIELDS_END,
        reference_offsets: THROWABLE_REFERENCE_OFFSETS,
        any_slots: [
            "kt_any_equals",
            "kt_any_hash_code",
            "kt_throwable_to_string",
        ],
    })
}

/// Where a subclass of an external base stores what the base's constructor was given. Only
/// `Throwable`'s single `message` is realized; a base with more than one is not in the table above.
pub(super) const EXTERNAL_BASE_FIELD_OFFSET: u32 = THROWABLE_MESSAGE_OFFSET;

/// The size of an object header: one pointer to the type.
pub(super) const HEADER_SIZE: u32 = 8;

/// Where one of a class's OWN fields lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FieldLayout {
    pub offset: u32,
    pub kind: CKind,
}

/// What a vtable slot holds: a shared table's slot as Native realizes it, plus the members Native
/// synthesizes itself. The variants shared with [`class_tables::Slot`] mean what they mean there.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Slot {
    /// One of `kotlin.Any`'s defaults, by runtime symbol.
    Runtime(&'static str),
    /// An emitted method.
    Function(FunId),
    /// An abstract member: the runtime's loud failure, never a jump through NULL.
    Abstract,
    /// A synthesized getter for a property with a backing field and no source getter.
    FieldGetter { class: ClassId, field: u32 },
    /// A synthesized setter, likewise.
    FieldSetter { class: ClassId, field: u32 },
    /// See [`class_tables::Slot::Bridge`].
    Bridge {
        declared: FunId,
        target_slot: Option<u32>,
        target: FunId,
    },
    /// See [`class_tables::Slot::FunctionBridge`]; the runtime names the number it stands in
    /// `KT_SLOT_INVOKE`.
    FunctionBridge {
        arity: usize,
        target_slot: u32,
        target: FunId,
    },
    /// `kotlin.Any`'s three for an ANNOTATION instance. Kotlin defines all three over the
    /// annotation's members, and an ARRAY member is compared, hashed and rendered by CONTENT —
    /// which is what separates it from a data class, where an array member is compared by
    /// identity. `hashCode` is the contract sum of `(127 * name.hashCode()) xor value.hashCode()`,
    /// and a program can read it: the corpus computes the same sum in Kotlin and compares.
    AnnotationMember { class: ClassId, member: AnyMember },
    /// `kotlin.Any`'s three for a VALUE CLASS's box, answered by the value it holds: two boxes of
    /// one value class are equal when their values are, and `V(x=1)` is how one renders.
    ValueMember { class: ClassId, member: AnyMember },
    /// A value class's own member, reached through its BOX. The member itself takes the value as
    /// `this` — that is what a value class is — while a vtable is read off an object, so this
    /// entry takes the box, reads the value out of it, and calls the member with that.
    ValueBridge { class: ClassId, function: FunId },
    /// See [`class_tables::Slot::AccessorBridge`].
    AccessorBridge {
        declared: Ty,
        implemented: Ty,
        receiver: Option<(Ty, Ty)>,
        setter: bool,
        target_slot: u32,
    },
}

impl Slot {
    /// A shared table's slot as Native realizes it, where `kotlin.Any`'s members are those of the
    /// base `root` — the class at the top of the hierarchy in this file — extends.
    fn realized(slot: class_tables::Slot, root: &IrClass) -> Self {
        match slot {
            class_tables::Slot::AnyMember(member) => Self::Runtime(any_member_symbol(root, member)),
            class_tables::Slot::Function(function) => Self::Function(function),
            class_tables::Slot::Abstract => Self::Abstract,
            class_tables::Slot::FieldGetter { class, field } => Self::FieldGetter { class, field },
            class_tables::Slot::FieldSetter { class, field } => Self::FieldSetter { class, field },
            class_tables::Slot::Bridge {
                declared,
                target_slot,
                target,
            } => Self::Bridge {
                declared,
                target_slot,
                target,
            },
            class_tables::Slot::FunctionBridge {
                arity,
                target_slot,
                target,
            } => Self::FunctionBridge {
                arity,
                target_slot,
                target,
            },
            class_tables::Slot::AccessorBridge {
                declared,
                implemented,
                receiver,
                setter,
                target_slot,
            } => Self::AccessorBridge {
                declared,
                implemented,
                receiver,
                setter,
                target_slot,
            },
        }
    }
}

/// The runtime symbol realizing `kotlin.Any`'s `member` for objects whose hierarchy in this file
/// starts at `root`.
///
/// A base the runtime owns fills `kotlin.Any`'s three its own way — a `Throwable` renders as
/// `qualified.Name: message` rather than by identity — and Kotlin's `Enum.toString()` is the
/// constant's name, which the runtime answers because the storage it reads belongs to
/// `kotlin.Enum`, a base no file declares.
fn any_member_symbol(root: &IrClass, member: AnyMember) -> &'static str {
    if member == AnyMember::ToString && is_enum(root) {
        return "kt_enum_to_string";
    }
    let defaults = match external_base(root.superclass) {
        Some(base) => base.any_slots,
        None => ["kt_any_equals", "kt_any_hash_code", "kt_any_to_string"],
    };
    defaults[member.slot() as usize]
}

/// How Native carries values, as the shared tables ask it.
struct NativeRepresentation<'v>(&'v NativeValueClasses);

impl Representation for NativeRepresentation<'_> {
    fn same(&self, a: Ty, b: Ty) -> bool {
        representation(self.0, a) == representation(self.0, b)
    }

    fn is_reference(&self, ty: Ty) -> bool {
        c_kind(self.0, ty) == CKind::Ref
    }

    fn owns_base(&self, superclass: TypeName) -> bool {
        superclass == crate::types::wk::kotlin_enum() || external_base(superclass).is_some()
    }
}

#[derive(Clone, Debug)]
pub(super) struct ClassLayout {
    /// The superclass in this file, or `None` for a class extending `kotlin.Any`.
    pub superclass: Option<ClassId>,
    /// Parallel to `IrClass::fields`.
    pub fields: Vec<FieldLayout>,
    /// The first byte after the last field, before any rounding — where a subclass's own fields
    /// begin, exactly as C packs the members of a struct that spells the superclass's fields out
    /// as a prefix.
    pub fields_end: u32,
    /// Bytes including the header, rounded up to 8.
    pub instance_size: u32,
    /// Every reference field, inherited ones first — what the collector traces.
    pub reference_offsets: Vec<u32>,
    pub vtable: Vec<Slot>,
    pub slots: HashMap<SlotKey, u32>,
}

/// The layouts of every class in a file, indexed by `ClassId`, plus the order in which a
/// superclass precedes its subclasses.
#[derive(Debug)]
pub(super) struct ClassModel {
    pub layouts: Vec<ClassLayout>,
    pub order: Vec<ClassId>,
    /// Every interface each class implements, transitively — what its descriptor carries so `is`
    /// can answer for a type that is not on the single-inheritance chain. Indexed by `ClassId`.
    pub interfaces: Vec<Vec<ClassId>>,
}

impl ClassModel {
    pub(super) fn layout(&self, class: ClassId) -> &ClassLayout {
        &self.layouts[class as usize]
    }

    /// The slot a member dispatches through, looked up on the class the call names.
    pub(super) fn slot(&self, class: ClassId, key: &SlotKey) -> Option<u32> {
        self.layout(class).slots.get(key).copied()
    }
}

/// Reject, by name, a class this step does not lower.
/// Reject, by name, a class this step does not lower.
pub(super) fn check_supported(class: &IrClass) -> Result<(), Unsupported> {
    let name = class.fq_name();
    let construct = if class.annotation_impl_of.is_some() {
        "an annotation implementation class"
    } else if class.prop_ref.is_some() || class.func_ref.is_some() {
        "a callable reference"
    } else {
        return Ok(());
    };
    Err(format!("{construct} (`{name}`)"))
}

/// How many slots `kotlin.Any` occupies at the front of every vtable.
const ANY_SLOTS: u32 = class_tables::ANY_SLOTS;

/// Build the layout and vtable of every class in `ir`, superclasses first.
pub(super) fn build(ir: &IrFile, values: &NativeValueClasses) -> Result<ClassModel, Unsupported> {
    for class in &ir.classes {
        check_supported(class)?;
    }
    let tables = class_tables::build(&NativeRepresentation(values), ir)?;
    let mut layouts: Vec<Option<ClassLayout>> = vec![None; ir.classes.len()];
    for &id in &tables.order {
        let table = tables.table(id);
        let layout = {
            let parent = table.superclass.map(|parent| {
                layouts[parent as usize]
                    .as_ref()
                    .expect("superclasses are laid out first")
            });
            layout_class(values, ir, id, table, parent)?
        };
        layouts[id as usize] = Some(layout);
    }
    let mut layouts: Vec<ClassLayout> = layouts
        .into_iter()
        .map(|layout| layout.expect("every class was laid out"))
        .collect();
    // A value class's own members take the VALUE as `this`, and a vtable is read off its box, so
    // every entry naming one of them reaches it through a bridge that unboxes first.
    for (id, class) in ir.classes.iter().enumerate() {
        if !values.is_value_class(class.fq_name) {
            continue;
        }
        for slot in &mut layouts[id].vtable {
            if let Slot::Function(function) = *slot {
                if class.methods.contains(&function) {
                    *slot = Slot::ValueBridge {
                        class: id as ClassId,
                        function,
                    };
                }
            }
        }
    }
    Ok(ClassModel {
        layouts,
        order: tables.order,
        interfaces: tables.interfaces,
    })
}

fn round_up(value: u32, alignment: u32) -> u32 {
    value.div_ceil(alignment) * alignment
}

/// The class at the top of `id`'s hierarchy in this file.
fn root_class(ir: &IrFile, mut id: ClassId) -> &IrClass {
    while let Some(parent) = ir.class_id_by_name(ir.classes[id as usize].superclass) {
        id = parent;
    }
    &ir.classes[id as usize]
}

/// Lay out class `id` in memory over its shared `table`, whose superclass in this file (already
/// laid out) is `parent`.
fn layout_class(
    values: &NativeValueClasses,
    ir: &IrFile,
    id: ClassId,
    table: &ClassTable,
    parent: Option<&ClassLayout>,
) -> Result<ClassLayout, Unsupported> {
    let class = &ir.classes[id as usize];

    // ---- fields ----
    let external = external_base(class.superclass);
    let mut end = parent.map_or_else(
        || {
            if is_enum(class) {
                ENUM_FIELDS_END
            } else {
                external.map_or(HEADER_SIZE, |base| base.fields_end)
            }
        },
        |parent| parent.fields_end,
    );
    let mut reference_offsets = parent.map_or_else(
        || {
            // The constant's name is a reference the collector traces like any other, and so is
            // a `Throwable`'s message: what the base stores is traced through the subclass.
            if is_enum(class) {
                vec![ENUM_NAME_OFFSET]
            } else {
                external.map_or_else(Vec::new, |base| base.reference_offsets.to_vec())
            }
        },
        |parent| parent.reference_offsets.clone(),
    );
    let mut fields = Vec::with_capacity(class.fields.len());
    for (index, field) in class.fields.iter().enumerate() {
        // A field carrying a mutable local this class captured holds the shared cell, not a copy
        // of the value the source declared — so it is a reference whatever the declaration says.
        let kind = c_kind(values, field_storage_ty(values, ir, id, index as u32)?);
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
    // ---- vtable ----
    let root = root_class(ir, id);
    let mut vtable: Vec<Slot> = table
        .vtable
        .iter()
        .map(|slot| Slot::realized(slot.clone(), root))
        .collect();

    // An annotation INSTANCE is a value whose three `kotlin.Any` members Kotlin defines over its
    // members rather than by identity. A declaration that carries none of them is still one — the
    // language gives no way to write them — so there is nothing to preserve.
    if class.is_annotation {
        for (slot, member) in [
            (0, AnyMember::Equals),
            (1, AnyMember::HashCode),
            (2, AnyMember::ToString),
        ] {
            vtable[slot] = Slot::AnnotationMember { class: id, member };
        }
    }
    // A value class's box answers `kotlin.Any`'s three by the value it holds. One the class
    // declares itself keeps its own, reached through the unboxing bridge `build` installs.
    if values.is_value_class(class.fq_name) {
        value_field(ir, id)?;
        for (slot, member) in [
            (0, AnyMember::Equals),
            (1, AnyMember::HashCode),
            (2, AnyMember::ToString),
        ] {
            if matches!(vtable[slot], Slot::Runtime(_)) {
                vtable[slot] = Slot::ValueMember { class: id, member };
            }
        }
    }
    Ok(ClassLayout {
        superclass: table.superclass,
        fields,
        fields_end: end,
        instance_size,
        reference_offsets,
        vtable,
        slots: table.slots.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::class_tables::fixtures::{
        add_method, class, field, function, record_override, record_semantic_override,
        stored_property,
    };
    use crate::fir::{ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
    use crate::ir::IrProperty;

    fn build(ir: &IrFile) -> Result<ClassModel, Unsupported> {
        super::build(ir, &NativeValueClasses::default())
    }

    #[test]
    fn a_value_box_uses_the_recorded_property_field_coordinate() {
        let mut ir = IrFile::default();
        let mut value = IrClass::synthetic(crate::types::type_name("app/Token"));
        value.is_value = true;
        value.ctor_param_count = 1;
        // The decoy has the property's spelling and occupies field zero. Only the common-IR
        // coordinate distinguishes the actual storage field from both guesses.
        value.fields = vec![field("payload", Ty::String), field("storage", Ty::Int)];
        value.properties = vec![stored_property("payload", Ty::Int, 1)];
        let class = ir.add_class(value);

        assert_eq!(value_field(&ir, class), Ok(1));
    }

    #[test]
    fn every_vtable_begins_with_kotlin_any_in_the_fixed_order() {
        let mut ir = IrFile::default();
        let point = class(&mut ir, "Point", "kotlin/Any", 0);
        let model = build(&ir).expect("layout");
        let layout = model.layout(point);
        assert_eq!(
            layout.vtable,
            vec![
                Slot::Runtime("kt_any_equals"),
                Slot::Runtime("kt_any_hash_code"),
                Slot::Runtime("kt_any_to_string"),
                // The function slot, reserved in every table so no member ever stands there.
                Slot::Abstract,
            ]
        );
        assert_eq!(layout.instance_size, HEADER_SIZE);
    }

    #[test]
    fn a_new_method_appends_and_an_override_replaces_the_slot() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        let b = class(&mut ir, "B", "A", 1);
        let a_name = add_method(&mut ir, a, function("name", "A", vec![], Ty::String, false));
        let a_other = add_method(&mut ir, a, function("other", "A", vec![], Ty::Int, false));
        let b_name = add_method(&mut ir, b, function("name", "B", vec![], Ty::String, false));
        let b_extra = add_method(&mut ir, b, function("extra", "B", vec![], Ty::Int, false));
        record_override(&mut ir, b, b_name, a_name);

        let model = build(&ir).expect("layout");
        assert_eq!(model.slot(a, &SlotKey::Function(a_name)), Some(4));
        assert_eq!(model.slot(a, &SlotKey::Function(a_other)), Some(5));
        assert_eq!(model.layout(a).vtable[4], Slot::Function(a_name));

        let b_layout = model.layout(b);
        assert_eq!(
            b_layout.vtable[4],
            Slot::Function(b_name),
            "the override takes the slot its base assigned"
        );
        assert_eq!(
            b_layout.vtable[5],
            Slot::Function(a_other),
            "inherited unchanged"
        );
        assert_eq!(
            b_layout.vtable[6],
            Slot::Function(b_extra),
            "new members append"
        );
        assert_eq!(
            model.slot(b, &SlotKey::Function(a_name)),
            Some(4),
            "a call naming the base method finds the same slot on the subclass"
        );
        assert_eq!(model.slot(b, &SlotKey::Function(b_name)), Some(4));
    }

    #[test]
    fn a_three_level_chain_keeps_slot_numbers_stable() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        let b = class(&mut ir, "B", "A", 1);
        let c = class(&mut ir, "C", "B", 2);
        let a_f = add_method(&mut ir, a, function("f", "A", vec![], Ty::Int, true));
        let b_g = add_method(&mut ir, b, function("g", "B", vec![], Ty::Int, false));
        let c_f = add_method(&mut ir, c, function("f", "C", vec![], Ty::Int, false));
        record_override(&mut ir, c, c_f, a_f);

        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(a).vtable[4], Slot::Abstract);
        assert_eq!(model.layout(b).vtable[4], Slot::Abstract);
        assert_eq!(model.layout(b).vtable[5], Slot::Function(b_g));
        assert_eq!(
            model.layout(c).vtable[4],
            Slot::Function(c_f),
            "an override two levels down replaces the slot the root declared"
        );
        assert_eq!(model.layout(c).vtable[5], Slot::Function(b_g));
        assert_eq!(model.layout(c).vtable.len(), 6);
        assert_eq!(model.order, vec![a, b, c]);
    }

    #[test]
    fn kotlin_any_slots_follow_exact_semantic_roles() {
        let mut ir = IrFile::default();
        let p = class(&mut ir, "P", "kotlin/Any", 0);
        let display = add_method(
            &mut ir,
            p,
            function("display", "P", vec![], Ty::String, false),
        );
        record_semantic_override(
            &mut ir,
            p,
            display,
            crate::types::SemanticCallRole::KotlinAnyToString,
        );
        // A familiar spelling without the selected declaration role is an unrelated member.
        let overload = add_method(
            &mut ir,
            p,
            function("toString", "P", vec![], Ty::String, false),
        );
        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(p).vtable[2], Slot::Function(display));
        assert_eq!(model.layout(p).vtable[4], Slot::Function(overload));
    }

    #[test]
    fn fields_follow_the_superclass_prefix_and_align_to_their_size() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        ir.classes[a as usize].fields = vec![field("flag", Ty::Boolean), field("count", Ty::Int)];
        let b = class(&mut ir, "B", "A", 1);
        ir.classes[b as usize].fields = vec![
            field("next", Ty::nullable(Ty::obj("B"))),
            field("small", Ty::Short),
            field("wide", Ty::Long),
            field("text", Ty::String),
        ];

        let model = build(&ir).expect("layout");
        let a_layout = model.layout(a);
        assert_eq!(a_layout.fields[0].offset, 8);
        assert_eq!(
            a_layout.fields[1].offset, 12,
            "an Int after a Boolean aligns to 4"
        );
        assert_eq!(a_layout.instance_size, 16, "rounded up to 8");
        assert!(a_layout.reference_offsets.is_empty());

        let b_layout = model.layout(b);
        assert_eq!(
            b_layout.fields[0].offset, 16,
            "own fields start after the superclass"
        );
        assert_eq!(b_layout.fields[1].offset, 24);
        assert_eq!(
            b_layout.fields[2].offset, 32,
            "a Long after a Short aligns to 8"
        );
        assert_eq!(b_layout.fields[3].offset, 40);
        assert_eq!(b_layout.instance_size, 48);
        assert_eq!(
            b_layout.reference_offsets,
            vec![16, 40],
            "exactly the reference fields, in layout order — what the collector traces"
        );
    }

    #[test]
    fn an_inherited_reference_field_stays_in_the_subclass_trace_list() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        ir.classes[a as usize].fields = vec![field("label", Ty::String)];
        let b = class(&mut ir, "B", "A", 1);
        ir.classes[b as usize].fields = vec![field("n", Ty::Int)];
        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(b).reference_offsets, vec![8]);
        assert_eq!(model.layout(b).fields[0].offset, 16);
        assert_eq!(model.layout(b).instance_size, 24);
    }

    #[test]
    fn a_subclass_field_packs_after_the_superclass_field_not_after_its_rounded_size() {
        // `A { a: Int }` ends at 12 and is 16 bytes; `B : A { b: Int }` puts `b` at 12, where C
        // puts the next member of the flattened struct, not at 16. The generated `_Static_assert`
        // is what caught the difference.
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        ir.classes[a as usize].fields = vec![field("a", Ty::Int)];
        let b = class(&mut ir, "B", "A", 1);
        ir.classes[b as usize].fields = vec![field("b", Ty::Int)];
        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(a).instance_size, 16);
        assert_eq!(model.layout(b).fields[0].offset, 12);
        assert_eq!(model.layout(b).instance_size, 16);
    }

    #[test]
    fn a_superclass_from_another_file_is_declined() {
        let mut ir = IrFile::default();
        class(&mut ir, "B", "other/A", 1);
        let error = build(&ir).expect_err("declined");
        assert!(
            error.contains("superclass declared outside this file"),
            "{error}"
        );
    }

    #[test]
    fn an_override_that_changes_representation_takes_a_bridge_and_a_slot_of_its_own() {
        // `A.f(Any?)` takes a reference and `B.f(Int)` an unboxed integer, so `A`'s slot cannot
        // hold `B`'s body. It holds a bridge that forwards to the slot `B`'s own body took, which
        // is what lets a call through `B` read `B`'s signature and pay for no conversion.
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        let b = class(&mut ir, "B", "A", 1);
        let erased = Ty::nullable(Ty::obj("kotlin/Any"));
        let a_f = add_method(
            &mut ir,
            a,
            function("f", "A", vec![erased], Ty::Unit, false),
        );
        let b_f = add_method(
            &mut ir,
            b,
            function("f", "B", vec![Ty::Int], Ty::Unit, false),
        );
        record_override(&mut ir, b, b_f, a_f);
        let model = build(&ir).expect("a representation change is bridged");
        let base_slot = model.layouts[a as usize].slots[&SlotKey::Function(a_f)];
        let own_slot = model.layouts[b as usize].slots[&SlotKey::Function(b_f)];
        assert_ne!(
            base_slot, own_slot,
            "the override keeps a slot of its own, with its own signature"
        );
        assert_eq!(
            model.layouts[b as usize].vtable[base_slot as usize],
            Slot::Bridge {
                declared: a_f,
                target_slot: Some(own_slot),
                target: b_f,
            },
            "the base's slot holds a bridge forwarding to the override's slot"
        );
        assert_eq!(
            model.layouts[b as usize].vtable[own_slot as usize],
            Slot::Function(b_f)
        );
    }

    #[test]
    fn an_open_property_without_a_source_getter_gets_a_synthesized_slot() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        ir.classes[a as usize].fields = vec![field("v", Ty::Int)];
        ir.classes[a as usize].properties = vec![IrProperty {
            name: "v".to_string(),
            context_params: Vec::new(),
            source_order: 0,
            decl_line: 1,
            ty: Ty::Int,
            type_params: Vec::new(),
            visibility: crate::types::Visibility::Public,
            return_value_status: Default::default(),
            annotations: Box::new([]),
            initializer: None,
            storage_ty: None,
            backing_field: Some(0),
            is_var: false,
            is_open: true,
            modifiers: Default::default(),
            delegate_field: None,
            has_constant_initializer: false,
            is_private: false,
            setter_visibility: crate::types::Visibility::Public,
            getter: None,
            setter: None,
            getter_jvm_name: None,
            setter_jvm_name: None,
            needs_access_bridge: false,
            accessor_annotations: Default::default(),
        }];
        let property = crate::fir::PropertyId::from_raw(0);
        ir.local_property_layouts.insert(
            property,
            crate::ir::IrLocalPropertyLayout::Member {
                class: a,
                owner: crate::types::type_name("A"),
                backing_field: Some(0),
                getter: None,
                setter: None,
                interface: false,
                name: "v".to_string(),
                ty: Ty::Int,
                mutable: false,
                private: false,
                context_parameters: Vec::new(),
                property: 0,
            },
        );
        let model = build(&ir).expect("layout");
        assert_eq!(
            model.layout(a).vtable[4],
            Slot::FieldGetter { class: a, field: 0 }
        );
        assert_eq!(
            model.slot(
                a,
                &SlotKey::Getter(ResolvedPropertyOverrideTarget::Module(property))
            ),
            Some(4)
        );
    }

    #[test]
    fn out_of_scope_classes_are_declined_by_name() {
        let mut ir = IrFile::default();
        let id = class(&mut ir, "D", "kotlin/Any", 0);
        // A data class, an interface, a value class, an `inner` class, an ANNOTATION class and now
        // a VARARG constructor parameter are deliberately absent from this list: each lowers like
        // the thing it is. A vararg parameter is PHYSICALLY an array, and the IR records it as one
        // — so a constructor taking it takes a reference, like any other array parameter, and
        // there was nothing for the refusal to protect.
        ir.classes[id as usize]
            .ctor_args
            .push(crate::ir::IrCtorArg {
                name: Some("rest".to_string()),
                context_kind: crate::types::ContextParameterKind::None,
                ty: Ty::obj_args("kotlin/Array", &[Ty::Int]),
                declared_ty: None,
                is_field: false,
                field_index: None,
                has_default: false,
                is_vararg: true,
                type_param: None,
                check: None,
                anonymous_super_forward: None,
                capture: None,
                provenance: crate::ir::IrCtorParameterProvenance::Value,
                capture_identity: None,
            });
        build(&ir).expect("a vararg constructor parameter is an array parameter");

        ir.classes[id as usize].annotation_impl_of = Some(TypeName::from("D"));
        assert!(build(&ir)
            .expect_err("declined")
            .contains("an annotation implementation class"));
    }

    #[test]
    fn an_annotation_class_is_laid_out_and_its_implementation_class_is_not() {
        // The declaration is metadata and lowers like the class it is; what this target cannot
        // give an annotation is a VALUE, and the refusal for that lives at the construction rather
        // than here. The implementation class is the JVM backend's own synthesis and reaches this
        // model only by mistake, so it is named separately.
        let mut ir = IrFile::default();
        let id = class(&mut ir, "D", "kotlin/Any", 0);
        ir.classes[id as usize].is_annotation = true;
        build(&ir).expect("an annotation declaration lays out like any other class");

        ir.classes[id as usize].annotation_impl_of = Some(TypeName::from("D"));
        assert!(build(&ir)
            .expect_err("declined")
            .contains("an annotation implementation class"));
    }

    /// Record that `implementation` is a function classifier's `invoke`, as the frontend does: an
    /// edge to an external declaration whose provider publishes the exact classifier role.
    fn record_invoke(ir: &mut IrFile, class: ClassId, implementation: FunId, arity: usize) {
        use crate::fir::{CallableId, ExternalCallableId};
        let owner = ir.classes[class as usize].fq_name_id();
        let function_owner = crate::types::type_name(&format!("fixture/Callable{arity}"));
        ir.function_overrides
            .entry(owner)
            .or_default()
            .push(crate::ir::IrFunctionOverride {
                implementation: ResolvedFunctionOverrideTarget::Module(CallableId::from_raw(
                    2000 + implementation,
                )),
                implementation_function: Some(implementation),
                implementation_owner: owner,
                overridden: ResolvedFunctionOverrideTarget::External(ExternalCallableId::from_raw(
                    1,
                )),
                overridden_owner: function_owner,
                overridden_semantic_role: Some(
                    crate::types::SemanticCallRole::KotlinFunctionInvoke,
                ),
                collection_barrier: None,
                overridden_is_interface: true,
                name: "invoke".to_string(),
                declared_parameters: Vec::new(),
                declared_result: Ty::Unit,
                applied_parameters: Vec::new(),
                applied_result: Ty::Unit,
                implementation_parameters: Vec::new(),
                implementation_parameter_identities: Vec::new(),
                overridden_parameter_identities: Vec::new(),
                implementation_result: Ty::Unit,
                suspend: false,
                has_kotlin_superclass_override: false,
                depth: 1,
            });
    }

    #[test]
    fn a_function_class_never_takes_the_slot_of_a_member_it_inherits() {
        // `open class A { open fun foo(): String }` and `class B : A(), () -> String`: `invoke`
        // belongs at the function slot, and `A.foo` must still be where every call through `A`
        // looks for it.
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        let b = class(&mut ir, "B", "A", 1);
        let foo = add_method(&mut ir, a, function("foo", "A", vec![], Ty::String, false));
        let invoke = add_method(
            &mut ir,
            b,
            function("invoke", "B", vec![], Ty::String, false),
        );
        record_invoke(&mut ir, b, invoke, 0);

        let model = super::build(&ir, &NativeValueClasses::default()).expect("layout");
        let foo_slot = model.slot(b, &SlotKey::Function(foo)).expect("foo's slot");
        assert_ne!(foo_slot, FUNCTION_SLOT);
        assert_eq!(
            model.layout(b).vtable[foo_slot as usize],
            Slot::Function(foo)
        );
        assert_eq!(
            model.layout(b).vtable[FUNCTION_SLOT as usize],
            Slot::Function(invoke)
        );
    }

    #[test]
    fn an_overload_or_a_private_base_member_is_not_overridden_by_name() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        let b = class(&mut ir, "B", "A", 1);
        // `open fun take(x: Any)` in A and `fun take(x: String)` in B share a machine signature,
        // but B's is an overload.
        let a_take = add_method(
            &mut ir,
            a,
            function(
                "take",
                "A",
                vec![Ty::nullable(Ty::obj("kotlin/Any"))],
                Ty::Int,
                false,
            ),
        );
        ir.open_methods.insert(a_take);
        let b_take = add_method(
            &mut ir,
            b,
            function("take", "B", vec![Ty::String], Ty::Int, false),
        );
        // `private fun hidden()` in A is invisible to B, whose `hidden()` is a new member.
        let a_hidden = add_method(&mut ir, a, function("hidden", "A", vec![], Ty::Int, false));
        ir.open_methods.insert(a_hidden);
        ir.set_method_visibility(a_hidden, crate::types::Visibility::Private);
        let b_hidden = add_method(&mut ir, b, function("hidden", "B", vec![], Ty::Int, false));

        let model = build(&ir).expect("layout");
        for (base, own) in [(a_take, b_take), (a_hidden, b_hidden)] {
            let base_slot = model.slot(b, &SlotKey::Function(base)).expect("base slot");
            assert_eq!(
                model.layout(b).vtable[base_slot as usize],
                Slot::Function(base)
            );
            assert_ne!(model.slot(b, &SlotKey::Function(own)), Some(base_slot));
        }
    }

    #[test]
    fn an_any_like_spelling_does_not_replace_a_slot_without_the_exact_role() {
        let mut ir = IrFile::default();
        let p = class(&mut ir, "P", "kotlin/Any", 0);
        add_method(
            &mut ir,
            p,
            function(
                "equals",
                "P",
                vec![Ty::nullable(Ty::obj("fixture/Value"))],
                Ty::Boolean,
                false,
            ),
        );
        let q = class(&mut ir, "Q", "kotlin/Any", 0);
        let equals = add_method(
            &mut ir,
            q,
            function(
                "same",
                "Q",
                vec![Ty::nullable(Ty::obj("fixture/Value"))],
                Ty::Boolean,
                false,
            ),
        );
        record_semantic_override(
            &mut ir,
            q,
            equals,
            crate::types::SemanticCallRole::KotlinAnyEquals,
        );
        let model = build(&ir).expect("layout");
        assert_eq!(
            model.layout(p).vtable[0],
            Slot::Runtime("kt_any_equals"),
            "spelling alone is not an override identity"
        );
        assert_eq!(model.layout(q).vtable[0], Slot::Function(equals));
    }

    #[test]
    fn synthesized_data_class_members_follow_their_exact_recorded_roles() {
        let mut ir = IrFile::default();
        let record = class(&mut ir, "Record", "kotlin/Any", 0);
        let owner = ir.classes[record as usize].fq_name_id();
        let generated = add_method(
            &mut ir,
            record,
            function(
                "physically-renamed",
                "Record",
                vec![Ty::nullable(Ty::obj("kotlin/Any"))],
                Ty::Boolean,
                false,
            ),
        );
        ir.record_data_class_member(owner, crate::ir::IrDataClassMemberRole::Equals, generated);
        let unrelated = add_method(
            &mut ir,
            record,
            function(
                "equals",
                "Record",
                vec![Ty::nullable(Ty::obj("fixture/Other"))],
                Ty::Boolean,
                false,
            ),
        );

        let model = build(&ir).expect("layout");

        assert_eq!(model.layout(record).vtable[0], Slot::Function(generated));
        assert_ne!(
            model.slot(record, &SlotKey::Function(unrelated)),
            Some(0),
            "a familiar spelling without the recorded role remains an ordinary overload"
        );
    }
}
