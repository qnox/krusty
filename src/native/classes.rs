//! Object layout and virtual-dispatch tables for the classes of one IR file.
//!
//! Everything here is a pure function over an [`IrFile`]; nothing emits code. The code generator
//! asks two questions of the result — "where is this field" and "which vtable slot is this member"
//! — and this module answers both from the IR's own hierarchy and override tables
//! (`IrFile::classifier_hierarchies`, `function_overrides`, `property_overrides`) rather than by
//! matching names.
//!
//! **Layout** is the classic single-inheritance one: the object header, then the superclass's
//! fields as a prefix in the superclass's order, then this class's fields in declaration order,
//! each aligned to its size, and the whole rounded up to 8. A subclass's first field follows the
//! superclass's last one directly, not the superclass's rounded size. The same offsets feed both
//! the loads and stores the generator emits and the `reference_offsets` table in the class's
//! descriptor, so the program and the collector cannot disagree about where a reference is.
//!
//! **Vtables** are the classic single-inheritance ones too: a class's table is its superclass's
//! with overridden slots replaced and newly declared members appended. Slot numbers are assigned
//! at the declaring class and stay valid down the hierarchy, which is what lets a call through an
//! `A`-typed value reach `B`'s override with one load. Every table begins with `kotlin.Any`'s
//! three slots — `equals`, `hashCode`, `toString`, in that order — which the runtime defines.
//!
//! Property accessors are members too. A property that is `open` (or overrides one) dispatches
//! through a getter (and, for a `var`, a setter) slot; when the source declares no accessor body,
//! the slot is a synthesized field access, so `x.v` through an `A`-typed `x` reads `B`'s `v` when
//! `B` overrides it. A property that is neither open nor an override is a direct field access.
//!
//! **Refusals.** Constructs not lowered yet — interfaces, data classes, `inner` classes, enums,
//! secondary constructors, constructor defaults, a superclass declared in another file, an override
//! that changes a parameter's machine representation (which would need a bridge) — are reported by
//! name from [`check_supported`] so the file declines with a diagnostic instead of emitting
//! something unverified.

use std::collections::{HashMap, HashSet};

use crate::fir::{ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
use crate::ir::{ClassId, FunId, IrClass, IrFile, IrFunction};
use crate::types::{Ty, TypeName};

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

/// The carrier for a Kotlin type. A nullable primitive is deliberately NOT a scalar: `Int?` has to
/// represent `null`, so it boxes, exactly as it does on the JVM. A type parameter erases to a
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

/// `kotlin.Enum`'s own storage, which every enum class carries ahead of its own fields: the
/// constant's NAME and its position among the constants. Kotlin reads both through `name` and
/// `ordinal`, and `toString` answers with the name. The enum's superclass is not a class in this
/// file — it is the language's own — so these offsets are the one place the layout of that base is
/// written down.
pub(super) const ENUM_NAME_OFFSET: u32 = HEADER_SIZE;
pub(super) const ENUM_ORDINAL_OFFSET: u32 = HEADER_SIZE + 8;
pub(super) const ENUM_FIELDS_END: u32 = HEADER_SIZE + 12;

/// Whether a class is an enum: it extends `kotlin.Enum`, which no file declares.
pub(super) fn is_enum(class: &IrClass) -> bool {
    class.superclass.matches("kotlin/Enum")
}

/// The size of an object header: one pointer to the type.
pub(super) const HEADER_SIZE: u32 = 8;

/// Where one of a class's OWN fields lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FieldLayout {
    pub offset: u32,
    pub kind: CKind,
}

/// What a vtable slot holds.
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
    /// A `value class`'s `equals`, `hashCode` or `toString`, answered by its underlying value —
    /// which is what makes it a value class rather than a one-field class.
    ValueMember { class: ClassId, member: ValueMember },
}

/// Which of `kotlin.Any`'s three a synthesized value-class member answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ValueMember {
    Equals,
    HashCode,
    ToString,
}

/// The identity of a virtual member, independent of which class's implementation fills it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum SlotKey {
    /// `kotlin.Any`'s slots, by number.
    Any(u32),
    /// A method, by its implementation in some class; an override registers under its own id too,
    /// so a further override finds the slot through either.
    Function(FunId),
    /// A property's getter, by the declaring class and the property name.
    Getter(ClassId, String),
    Setter(ClassId, String),
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

/// How many slots `kotlin.Any` occupies at the front of every vtable.
const ANY_SLOTS: u32 = 3;

/// `kotlin.Any`'s vtable, the prefix of every class's.
fn any_vtable() -> (Vec<Slot>, HashMap<SlotKey, u32>) {
    let vtable = vec![
        Slot::Runtime("kt_any_equals"),
        Slot::Runtime("kt_any_hash_code"),
        Slot::Runtime("kt_any_to_string"),
    ];
    let slots = (0..3).map(|slot| (SlotKey::Any(slot), slot)).collect();
    (vtable, slots)
}

/// The `kotlin.Any` slot a method overrides, decided by name and arity — the frontend does not
/// record an override of a dependency member in `function_overrides`, and Kotlin admits no other
/// member with these names and arities in a class.
fn any_slot(function: &IrFunction) -> Option<u32> {
    match (function.name.as_str(), function.params.len()) {
        ("equals", 1) => Some(0),
        ("hashCode", 0) => Some(1),
        ("toString", 0) => Some(2),
        _ => None,
    }
}

/// The expected carriers of `kotlin.Any`'s slots, so a same-named member with another signature
/// is not mistaken for an override.
fn any_slot_signature(slot: u32) -> (&'static [CKind], CKind) {
    match slot {
        0 => (&[CKind::Ref], CKind::Scalar("kt_boolean")),
        1 => (&[], CKind::Scalar("kt_int")),
        _ => (&[], CKind::Ref),
    }
}

/// Reject, by name, a class this step does not lower.
pub(super) fn check_supported(ir: &IrFile, class: &IrClass) -> Result<(), Unsupported> {
    let name = class.fq_name();
    let construct = if ir
        .class_ctor_defaults_name(class.fq_name_id())
        .is_some_and(|defaults| defaults.iter().any(Option::is_some))
        || class.ctor_args.iter().any(|argument| argument.has_default)
    {
        "a constructor default argument"
    } else if class.is_annotation || class.annotation_impl_of.is_some() {
        "an annotation class"
    } else if class.is_anonymous_object
        || class.enclosing_function.is_some()
        || class.is_local_class
    {
        "a local or anonymous class"
    } else if class.prop_ref.is_some() || class.func_ref.is_some() {
        "a callable reference"
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
    let mut layouts: Vec<ClassLayout> = layouts
        .into_iter()
        .map(|layout| layout.expect("every class was laid out"))
        .collect();
    let interfaces = interface_closure(ir);
    place_interface_slots(ir, &order, &mut layouts, &interfaces)?;
    Ok(ClassModel {
        layouts,
        order,
        interfaces,
    })
}

/// Every interface each class implements, transitively: its own, its superclass's, and the bases
/// of both. An interface's entry holds the interfaces it extends, not itself.
fn interface_closure(ir: &IrFile) -> Vec<Vec<ClassId>> {
    let direct = |id: ClassId| -> Vec<ClassId> {
        let class = &ir.classes[id as usize];
        class
            .interfaces
            .iter()
            .chain(
                class
                    .supertypes
                    .iter()
                    .copied()
                    .filter_map(Ty::obj_internal),
            )
            .filter_map(|name| ir.class_id_by_name(name))
            .filter(|&candidate| ir.classes[candidate as usize].is_interface)
            .collect()
    };
    let mut closures: Vec<Vec<ClassId>> = vec![Vec::new(); ir.classes.len()];
    // A fixed point rather than an order-dependent single pass: a class may precede an interface
    // it implements in IR order, and the closure is small enough that iterating to stability costs
    // nothing.
    loop {
        let mut changed = false;
        for id in 0..ir.classes.len() as ClassId {
            let mut collected: Vec<ClassId> = closures[id as usize].clone();
            let add = |collected: &mut Vec<ClassId>, candidate: ClassId| {
                if !collected.contains(&candidate) {
                    collected.push(candidate);
                }
            };
            for candidate in direct(id) {
                add(&mut collected, candidate);
                for inherited in closures[candidate as usize].clone() {
                    add(&mut collected, inherited);
                }
            }
            if let Some(parent) = ir.class_id_by_name(ir.classes[id as usize].superclass) {
                for inherited in closures[parent as usize].clone() {
                    add(&mut collected, inherited);
                }
            }
            if collected.len() != closures[id as usize].len() {
                closures[id as usize] = collected;
                changed = true;
            }
        }
        if !changed {
            return closures;
        }
    }
}

/// Give every interface member a slot number that is the same in EVERY class implementing it.
///
/// A class's own methods are numbered as they are declared, so two classes number their methods
/// differently — which is fine for a call through a class-typed receiver, because the call site
/// knows the class. A call through an INTERFACE-typed receiver does not: it knows only the
/// interface, so the number it uses has to mean the same thing in every implementation. So the
/// interface members share one numbering, placed above every class's own slots: each class's
/// vtable is its own slots, padded to `interface_base`, then one entry per interface member in the
/// program. A class fills only the entries of interfaces it implements; the rest are the abstract
/// trap, which nothing can reach because nothing can name them through a type this class has.
///
/// The cost is a vtable as long as the program's interface surface rather than the class's own,
/// and it is paid per class. That is the trade Kotlin's own targets make differently — the JVM has
/// `invokeinterface` and an itable search — and it is the right one while a compilation unit is a
/// file: dispatch stays a single indexed load, exactly like a class method's.
fn place_interface_slots(
    ir: &IrFile,
    order: &[ClassId],
    layouts: &mut [ClassLayout],
    interfaces: &[Vec<ClassId>],
) -> Result<(), Unsupported> {
    let is_interface = |id: ClassId| ir.classes[id as usize].is_interface;
    // Relative numbering, assigned interface by interface so an extending interface inherits the
    // slots of the one it extends.
    let mut members: Vec<InterfaceMember> = Vec::new();
    let mut relative: Vec<HashMap<SlotKey, u32>> = vec![HashMap::new(); ir.classes.len()];
    for &id in order.iter().filter(|&&id| is_interface(id)) {
        let mut own: HashMap<SlotKey, u32> = HashMap::new();
        for &base in &interfaces[id as usize] {
            for (key, slot) in &relative[base as usize] {
                own.insert(key.clone(), *slot);
            }
        }
        let layout = &layouts[id as usize];
        // The provisional layout numbered this interface's members as if they were a class's; the
        // ORDER it used is the declaration order, which is all that is wanted here.
        let mut provisional: Vec<(&SlotKey, &u32)> = layout.slots.iter().collect();
        provisional.sort_by_key(|(_, slot)| **slot);
        for (key, slot) in provisional {
            // A member occupying one of `kotlin.Any`'s slots is already numbered, and numbered the
            // same way for every object there is: a `fun interface` that overrides `toString`
            // wants slot 2, which is where the runtime's own rendering looks. Numbering it again in
            // the interface region would leave the slot every caller actually uses empty. Both
            // spellings of such a member — `Any(2)` and the method's own key — are skipped here and
            // carried through below.
            if *slot < ANY_SLOTS {
                continue;
            }
            let inherited = interfaces[id as usize]
                .iter()
                .find_map(|&base| relative[base as usize].get(key).copied());
            let entry = layout.vtable[*slot as usize].clone();
            match inherited {
                Some(slot) => {
                    own.insert(key.clone(), slot);
                    // A class implementing `Both : Named` registers what it overrides under the
                    // key the override names, and that can be either interface's spelling of the
                    // same member — a property's key carries its declaring class. So the member
                    // records the extending interface's spelling too, and the class is looked up
                    // by any of them.
                    if let Some(alias) = alias_key(key, id) {
                        let aliases = &mut members[slot as usize].aliases;
                        if !aliases.contains(&alias) {
                            aliases.push(alias);
                        }
                    }
                    // An interface redeclaring an inherited member with a body replaces what the
                    // base supplied, for classes that implement neither themselves.
                    if !matches!(entry, Slot::Abstract) {
                        members[slot as usize].default = entry;
                    }
                }
                None => {
                    let assigned = members.len() as u32;
                    members.push(InterfaceMember {
                        interface: id,
                        key: key.clone(),
                        aliases: Vec::new(),
                        default: entry,
                    });
                    own.insert(key.clone(), assigned);
                }
            }
        }
        relative[id as usize] = own;
    }

    let base = layouts
        .iter()
        .enumerate()
        .filter(|(id, _)| !is_interface(*id as ClassId))
        .map(|(_, layout)| layout.vtable.len() as u32)
        .max()
        .unwrap_or(0)
        .max(ANY_SLOTS);
    for id in 0..layouts.len() as ClassId {
        if is_interface(id) {
            // An interface has no instances of its own, so this table is never the table of an
            // object the program constructs. It is still worth building: it is what an implementor
            // that supplies nothing of its own would have — every member this interface DEFINES a
            // body for, and the abstract trap elsewhere — and the object a lambda becomes when it
            // is converted to this interface starts from exactly that and fills in the one member
            // the lambda is.
            let own = relative[id as usize].clone();
            let mut slots: HashMap<SlotKey, u32> = own
                .iter()
                .map(|(key, slot)| (key.clone(), base + slot))
                .collect();
            let mut table = layouts[id as usize].vtable.clone();
            for (key, slot) in &layouts[id as usize].slots {
                if *slot < ANY_SLOTS {
                    slots.insert(key.clone(), *slot);
                }
            }
            let mut defaults = vec![Slot::Abstract; base as usize + members.len()];
            defaults[..ANY_SLOTS as usize].clone_from_slice(&any_vtable().0);
            // An `Any` member the interface itself overrides keeps `Any`'s slot, which is where
            // every caller looks for it — including the runtime's own `toString`.
            for (slot, entry) in defaults.iter_mut().take(ANY_SLOTS as usize).enumerate() {
                if let Some(own) = table.get(slot) {
                    *entry = own.clone();
                }
            }
            for (member_slot, member) in members.iter().enumerate() {
                let mine = own
                    .get(&member.key)
                    .is_some_and(|slot| *slot == member_slot as u32);
                if !mine {
                    continue;
                }
                defaults[base as usize + member_slot] = member.default.clone();
            }
            table = defaults;
            layouts[id as usize].slots = slots;
            layouts[id as usize].vtable = table;
            continue;
        }
        let mut vtable = std::mem::take(&mut layouts[id as usize].vtable);
        vtable.resize(base as usize, Slot::Abstract);
        for member in &members {
            let implemented = interfaces[id as usize].contains(&member.interface);
            let entry = if !implemented {
                Slot::Abstract
            } else {
                match std::iter::once(&member.key)
                    .chain(&member.aliases)
                    .find_map(|key| layouts[id as usize].slots.get(key))
                {
                    // The class (or an ancestor) implements the member: its own slot already holds
                    // the most derived implementation, whatever further overrides did to it.
                    Some(&slot) => vtable[slot as usize].clone(),
                    None => member.default.clone(),
                }
            };
            // A CONCRETE class reaching the abstract trap for an interface it DOES implement
            // means the implementation exists in the source and this model failed to find it —
            // Kotlin would not have compiled the class otherwise. Declining says so at compile
            // time; emitting the trap would say it at run time, as a program that aborts where it
            // should print an answer. A slot of an interface the class does not implement is
            // padding, and unreachable: nothing can name it through a type this class has.
            if implemented
                && matches!(entry, Slot::Abstract)
                && !ir.classes[id as usize].is_abstract
                && !ir.classes[id as usize].is_interface
            {
                return Err(format!(
                    "an interface member with no implementation found (`{}` in `{}`)",
                    member_name(ir, member),
                    ir.classes[id as usize].fq_name()
                ));
            }
            vtable.push(entry);
        }
        layouts[id as usize].vtable = vtable;
    }
    Ok(())
}

/// How a diagnostic names an interface member.
fn member_name(ir: &IrFile, member: &InterfaceMember) -> String {
    let interface = ir.classes[member.interface as usize].fq_name();
    let name = match &member.key {
        SlotKey::Function(fid) => ir.functions[*fid as usize].name.clone(),
        SlotKey::Getter(_, name) => format!("get {name}"),
        SlotKey::Setter(_, name) => format!("set {name}"),
        SlotKey::Any(slot) => format!("kotlin.Any slot {slot}"),
    };
    format!("{interface}.{name}")
}

/// The same member spelled as a member of `interface`, for a key that carries its declaring class.
fn alias_key(key: &SlotKey, interface: ClassId) -> Option<SlotKey> {
    match key {
        SlotKey::Getter(_, name) => Some(SlotKey::Getter(interface, name.clone())),
        SlotKey::Setter(_, name) => Some(SlotKey::Setter(interface, name.clone())),
        SlotKey::Function(_) | SlotKey::Any(_) => None,
    }
}

/// One member of one interface, in the program-wide numbering.
struct InterfaceMember {
    interface: ClassId,
    key: SlotKey,
    /// The same member as spelled through each interface that inherits it.
    aliases: Vec<SlotKey>,
    /// What a class that implements the interface without supplying this member gets: the
    /// interface's own body where it has one, and otherwise the abstract trap.
    default: Slot,
}

/// Classes sorted so that everything a class is laid out FROM precedes it: its superclass, whose
/// fields and vtable it extends, and — for an interface — the interfaces it extends, whose member
/// numbering it inherits. A superclass that is not in this file (other than `kotlin.Any`) is
/// declined.
///
/// This is a topological sort rather than a sort by hierarchy depth. Depth is not a valid key
/// here: two classes can sit at the same recorded depth with one extending the other, once
/// interfaces contribute their own depths to the same number, and laying out a subclass before its
/// superclass reads a layout that does not exist yet.
fn hierarchy_order(ir: &IrFile) -> Result<Vec<ClassId>, Unsupported> {
    for class in &ir.classes {
        if !class.superclass.matches("kotlin/Any")
            && !class.superclass.matches("kotlin/Enum")
            && ir.class_id_by_name(class.superclass).is_none()
        {
            return Err(format!(
                "a superclass declared outside this file (`{}` extends `{}`)",
                class.fq_name(),
                class.superclass.render()
            ));
        }
    }
    let requires = |id: ClassId| -> Vec<ClassId> {
        let class = &ir.classes[id as usize];
        let mut needed: Vec<ClassId> = ir.class_id_by_name(class.superclass).into_iter().collect();
        if class.is_interface {
            needed.extend(
                class
                    .interfaces
                    .iter()
                    .chain(
                        class
                            .supertypes
                            .iter()
                            .copied()
                            .filter_map(Ty::obj_internal),
                    )
                    .filter_map(|name| ir.class_id_by_name(name)),
            );
        }
        needed.retain(|&needed| needed != id);
        needed
    };
    let mut placed = vec![false; ir.classes.len()];
    let mut order = Vec::with_capacity(ir.classes.len());
    // IR order within a level, so emission stays deterministic for a given source.
    while order.len() < ir.classes.len() {
        let mut progressed = false;
        for id in 0..ir.classes.len() as ClassId {
            if placed[id as usize] || !requires(id).iter().all(|&need| placed[need as usize]) {
                continue;
            }
            placed[id as usize] = true;
            order.push(id);
            progressed = true;
        }
        if !progressed {
            // A cycle among supertypes: not expressible in Kotlin, so this is a defect in the
            // hierarchy the frontend handed over rather than something to lower.
            return Err("a cycle among supertypes".to_string());
        }
    }
    Ok(order)
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
    let mut end = parent.map_or_else(
        || {
            if is_enum(class) {
                ENUM_FIELDS_END
            } else {
                HEADER_SIZE
            }
        },
        |parent| parent.fields_end,
    );
    let mut reference_offsets = parent.map_or_else(
        || {
            // The constant's name is a reference the collector traces like any other.
            if is_enum(class) {
                vec![ENUM_NAME_OFFSET]
            } else {
                Vec::new()
            }
        },
        |parent| parent.reference_offsets.clone(),
    );
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

    // ---- vtable ----
    let (mut vtable, mut slots) = parent.map_or_else(any_vtable, |parent| {
        (parent.vtable.clone(), parent.slots.clone())
    });

    // Which methods are property accessors, so their slots are keyed by the property. A property
    // whose accessor has no BODY — an abstract `val` in an interface — carries no accessor id, and
    // its accessor reaches the method list as an ordinary method; matching the declared accessor
    // name is what ties the two back together, so the interface and the class implementing it
    // agree on one key for the member.
    let mut accessor_keys: HashMap<FunId, SlotKey> = HashMap::new();
    for property in &class.properties {
        let named = |accessor: &str, arity: usize| {
            class.methods.iter().copied().find(|&fid| {
                let function = &ir.functions[fid as usize];
                function.name == accessor && function.params.len() == arity
            })
        };
        let getter = property
            .getter
            .or_else(|| named(&crate::names::property_getter_name(&property.name), 0));
        let setter = property
            .setter
            .or_else(|| named(&crate::names::property_setter_name(&property.name), 1));
        if let Some(getter) = getter {
            accessor_keys.insert(getter, SlotKey::Getter(id, property.name.clone()));
        }
        if let Some(setter) = setter {
            accessor_keys.insert(setter, SlotKey::Setter(id, property.name.clone()));
        }
    }

    let overridden_functions = overridden_functions(ir, class)?;
    let overridden_properties = overridden_properties(ir, id, class)?;

    for &fid in &class.methods {
        let function = &ir.functions[fid as usize];
        if function.dispatch_receiver.is_none() || function.is_static {
            continue;
        }
        let entry = if function.body.is_some() {
            Slot::Function(fid)
        } else {
            Slot::Abstract
        };
        let own_key = accessor_keys
            .get(&fid)
            .cloned()
            .unwrap_or(SlotKey::Function(fid));

        let replaces = match any_slot(function) {
            Some(slot) => {
                let (params, ret) = any_slot_signature(slot);
                let actual: Vec<CKind> = function.params.iter().copied().map(c_kind).collect();
                if actual == params && c_kind(function.ret) == ret {
                    Some(slot)
                } else {
                    None
                }
            }
            None => None,
        };
        // What this method overrides, split by what each target owns: a class base owns a slot in
        // this vtable to replace, while an interface base owns a number in the program-wide
        // interface region, which is pointed at this method's slot once that slot is known.
        let mut interface_keys = Vec::new();
        let mut class_replaces = None;
        for &overridden in overridden_functions.get(&fid).into_iter().flatten() {
            check_same_representation(ir, function, &ir.functions[overridden as usize])?;
            let owner = ir.functions[overridden as usize]
                .dispatch_receiver
                .and_then(|owner| ir.class_id_by_name(owner))
                .ok_or_else(|| {
                    format!(
                        "an override of a method declared outside this file (`{}`)",
                        function.name
                    )
                })?;
            let key = function_key(ir, owner, overridden);
            if ir.classes[owner as usize].is_interface {
                interface_keys.push(key);
                continue;
            }
            let slot = slots.get(&key).copied().ok_or_else(|| {
                format!("an override with no slot to replace (`{}`)", function.name)
            })?;
            class_replaces = Some(slot);
        }
        // A member EXTENSION's override is not in the override tables — the frontend records none
        // for one — so an override of it would take a new slot and a call through the base's type
        // would reach the base's body. The IR still says which it is: a method the source declares
        // WITHOUT `override` is listed in `fresh_method_decls`, so one that is absent from that
        // list and matches an inherited member by name and machine signature is that member's
        // override. This is name matching, and it is sound only because the IR has already
        // answered the question name matching cannot: whether this declaration is an override.
        let inherited_replaces = match class_replaces {
            Some(slot) => Some(slot),
            None if !ir.fresh_method_decls.contains(&fid) => {
                inherited_slot(ir, superclass, function, &slots)
            }
            None => None,
        };
        let replaces = match replaces {
            Some(slot) => Some(slot),
            None => match inherited_replaces {
                Some(slot) => Some(slot),
                None => match &own_key {
                    SlotKey::Getter(_, name) | SlotKey::Setter(_, name) => {
                        let setter = matches!(own_key, SlotKey::Setter(..));
                        overridden_property_slot(
                            ir,
                            &overridden_properties,
                            &slots,
                            name,
                            setter,
                            &class.fq_name(),
                        )?
                    }
                    _ => None,
                },
            },
        };
        let slot = match replaces {
            Some(slot) => {
                vtable[slot as usize] = entry;
                slot
            }
            None => {
                vtable.push(entry);
                (vtable.len() - 1) as u32
            }
        };
        slots.insert(own_key, slot);
        for key in interface_keys {
            slots.insert(key, slot);
        }
    }

    // Open or overriding properties with no source accessor still dispatch: synthesize the
    // field access as a slot.
    for property in &class.properties {
        let overrides = overridden_properties.contains_key(&property.name);
        if !property.is_open && !overrides {
            continue;
        }
        let accessors = [
            (false, property.getter.is_none()),
            (true, property.is_var && property.setter.is_none()),
        ];
        for (setter, needed) in accessors {
            if !needed {
                continue;
            }
            let entry = match property.backing_field {
                Some(field) if setter => Slot::FieldSetter { class: id, field },
                Some(field) => Slot::FieldGetter { class: id, field },
                None => Slot::Abstract,
            };
            let key = if setter {
                SlotKey::Setter(id, property.name.clone())
            } else {
                SlotKey::Getter(id, property.name.clone())
            };
            let replaces = overridden_property_slot(
                ir,
                &overridden_properties,
                &slots,
                &property.name,
                setter,
                &class.fq_name(),
            )?;
            let slot = match replaces {
                Some(slot) => {
                    vtable[slot as usize] = entry;
                    slot
                }
                None => {
                    vtable.push(entry);
                    (vtable.len() - 1) as u32
                }
            };
            slots.insert(key, slot);
            for (owner, overridden_name) in overridden_properties
                .get(&property.name)
                .into_iter()
                .flatten()
            {
                if !ir.classes[*owner as usize].is_interface {
                    continue;
                }
                let key = if setter {
                    SlotKey::Setter(*owner, overridden_name.clone())
                } else {
                    SlotKey::Getter(*owner, overridden_name.clone())
                };
                slots.insert(key, slot);
            }
        }
    }

    register_inherited_interface_members(ir, class, &mut slots)?;

    // A `value class` is not a one-field class. Kotlin answers `equals`, `hashCode` and `toString`
    // by the value it wraps — `IC(1) == IC(1)` is true, and `IC(1).toString()` is `IC(n=1)` —
    // where an ordinary class answers all three by identity. The JVM reaches that by erasing the
    // class to its underlying value entirely; here the object stays, and the three members are
    // synthesized instead. A value class that DECLARES one of them keeps its own: the loop above
    // has already replaced that slot, and only `kotlin.Any`'s own default is overwritten here.
    // Kotlin's `Enum.toString()` is the constant's name, where `kotlin.Any`'s is its identity. The
    // runtime answers it, because the storage it reads belongs to `kotlin.Enum` — a base no file
    // declares — rather than to this class.
    if is_enum(class) && matches!(vtable[2], Slot::Runtime(_)) {
        vtable[2] = Slot::Runtime("kt_enum_to_string");
    }
    if class.is_value {
        if class.fields.len() != 1 {
            return Err(format!(
                "a value class with {} fields (`{}`)",
                class.fields.len(),
                class.fq_name()
            ));
        }
        if matches!(
            c_kind(class.fields[0].ty),
            CKind::Scalar("kt_float" | "kt_double")
        ) {
            // Its `toString` would have to render the value, which the runtime cannot do.
            return Err(format!(
                "a value class wrapping a floating-point value (`{}`)",
                class.fq_name()
            ));
        }
        for (slot, member) in [
            (0, ValueMember::Equals),
            (1, ValueMember::HashCode),
            (2, ValueMember::ToString),
        ] {
            if matches!(vtable[slot], Slot::Runtime(_)) {
                vtable[slot] = Slot::ValueMember { class: id, member };
            }
        }
    }

    Ok(ClassLayout {
        superclass,
        fields,
        fields_end: end,
        instance_size,
        reference_offsets,
        vtable,
        slots,
    })
}

/// Point an interface's member numbers at implementations this class INHERITS rather than declares.
///
/// Kotlin calls this a fake override: `class B : A(), I` satisfies `I.foo` with `A.foo`, and `A`
/// knows nothing about `I`. `A.foo` already occupies a slot in this class's table — inherited with
/// the rest of `A`'s — so what is missing is only the interface's number pointing at that slot,
/// which nothing in the loop over the class's OWN members could have added. The frontend records
/// the edge on the class that brings the two together, which is this one.
fn register_inherited_interface_members(
    ir: &IrFile,
    class: &IrClass,
    slots: &mut HashMap<SlotKey, u32>,
) -> Result<(), Unsupported> {
    let module_function = |target: &ResolvedFunctionOverrideTarget| match target {
        ResolvedFunctionOverrideTarget::Module(callable) => {
            ir.checked_callable_functions.get(callable).copied()
        }
        ResolvedFunctionOverrideTarget::External(_) => None,
    };
    for edge in ir
        .function_overrides
        .get(&class.fq_name_id())
        .into_iter()
        .flatten()
    {
        if !edge.overridden_is_interface {
            continue;
        }
        let implementation = edge
            .implementation_function
            .or_else(|| module_function(&edge.implementation));
        let (Some(implementation), Some(overridden)) =
            (implementation, module_function(&edge.overridden))
        else {
            continue;
        };
        let (Some(owner), Some(interface)) = (
            ir.class_id_by_name(edge.implementation_owner),
            ir.class_id_by_name(edge.overridden_owner),
        ) else {
            continue;
        };
        let Some(&slot) = slots.get(&function_key(ir, owner, implementation)) else {
            continue;
        };
        // The inherited method has to be CALLABLE through the interface's signature. `class F5 :
        // F3, D4()` where `D4.foo(): Int` is what `D1.foo(): Any` gets is the case that says why:
        // one returns an unboxed machine integer, the other a reference, and pointing the
        // interface's slot at it would have a caller read an integer as a pointer. The JVM emits a
        // bridge for exactly this; until one is emitted here, the file is declined.
        check_same_representation(
            ir,
            &ir.functions[implementation as usize],
            &ir.functions[overridden as usize],
        )?;
        slots
            .entry(function_key(ir, interface, overridden))
            .or_insert(slot);
    }
    for edge in ir
        .property_overrides
        .get(&class.fq_name_id())
        .into_iter()
        .flatten()
    {
        if !edge.overridden_is_interface {
            continue;
        }
        let property = |target: &ResolvedPropertyOverrideTarget| match target {
            ResolvedPropertyOverrideTarget::Module(id) => ir.checked_properties.get(id),
            ResolvedPropertyOverrideTarget::External(_) => None,
        };
        let (Some(implementation), Some(overridden)) =
            (property(&edge.implementation), property(&edge.overridden))
        else {
            continue;
        };
        let (Some(owner), Some(interface)) = (implementation.class, overridden.class) else {
            continue;
        };
        for setter in [false, true] {
            let (from, to) = if setter {
                (
                    SlotKey::Setter(owner, implementation.name.clone()),
                    SlotKey::Setter(interface, overridden.name.clone()),
                )
            } else {
                (
                    SlotKey::Getter(owner, implementation.name.clone()),
                    SlotKey::Getter(interface, overridden.name.clone()),
                )
            };
            if let Some(&slot) = slots.get(&from) {
                slots.entry(to).or_insert(slot);
            }
        }
    }
    Ok(())
}

/// The slot an inherited member of the same name and machine signature occupies, searched up the
/// superclass chain. Used only for a method the IR has already identified as an override.
fn inherited_slot(
    ir: &IrFile,
    superclass: Option<ClassId>,
    function: &IrFunction,
    slots: &HashMap<SlotKey, u32>,
) -> Option<u32> {
    let signature = |candidate: &IrFunction| {
        (
            candidate
                .params
                .iter()
                .copied()
                .map(c_kind)
                .collect::<Vec<_>>(),
            c_kind(candidate.ret),
        )
    };
    let mine = signature(function);
    let mut at = superclass;
    while let Some(class) = at {
        for &candidate in &ir.classes[class as usize].methods {
            let other = &ir.functions[candidate as usize];
            if other.name != function.name || signature(other) != mine {
                continue;
            }
            if let Some(&slot) = slots.get(&function_key(ir, class, candidate)) {
                return Some(slot);
            }
        }
        at = ir.class_id_by_name(ir.classes[class as usize].superclass);
    }
    None
}

/// The slot key of a method of `owner`: a property accessor is keyed by its property.
pub(super) fn function_key(ir: &IrFile, owner: ClassId, fid: FunId) -> SlotKey {
    let class = &ir.classes[owner as usize];
    for property in &class.properties {
        if property.getter == Some(fid) {
            return SlotKey::Getter(owner, property.name.clone());
        }
        if property.setter == Some(fid) {
            return SlotKey::Setter(owner, property.name.clone());
        }
    }
    SlotKey::Function(fid)
}

/// The slot an overriding property accessor replaces, found through the overridden property's
/// declaring class.
fn overridden_property_slot(
    ir: &IrFile,
    overridden: &HashMap<String, Vec<(ClassId, String)>>,
    slots: &HashMap<SlotKey, u32>,
    name: &str,
    setter: bool,
    class_name: &str,
) -> Result<Option<u32>, Unsupported> {
    // Only a CLASS base owns a slot to replace. An interface base owns a number in the program-wide
    // interface region instead, and that is pointed at this property afterwards rather than
    // replaced here.
    let Some((owner, overridden_name)) = overridden.get(name).and_then(|targets| {
        targets
            .iter()
            .find(|(owner, _)| !ir.classes[*owner as usize].is_interface)
    }) else {
        return Ok(None);
    };
    let key = if setter {
        SlotKey::Setter(*owner, overridden_name.clone())
    } else {
        SlotKey::Getter(*owner, overridden_name.clone())
    };
    // A `val` overridden by a `var` adds a setter the base never had: a new slot, not an override.
    if setter && !slots.contains_key(&key) {
        return Ok(None);
    }
    slots.get(&key).copied().map(Some).ok_or_else(|| {
        format!("a property override with no slot to replace (`{class_name}.{name}`)")
    })
}

/// Implementation method → the method it overrides, for the methods `class` declares. Both ends
/// must be functions of this file.
fn overridden_functions(
    ir: &IrFile,
    class: &IrClass,
) -> Result<HashMap<FunId, Vec<FunId>>, Unsupported> {
    let mut map: HashMap<FunId, Vec<FunId>> = HashMap::new();
    let Some(overrides) = ir.function_overrides.get(&class.fq_name_id()) else {
        return Ok(map);
    };
    for edge in overrides {
        let implementation = match (&edge.implementation, edge.implementation_function) {
            (_, Some(fid)) => Some(fid),
            (ResolvedFunctionOverrideTarget::Module(callable), None) => {
                ir.checked_callable_functions.get(callable).copied()
            }
            (ResolvedFunctionOverrideTarget::External(_), None) => None,
        };
        let Some(implementation) = implementation else {
            continue;
        };
        let function = &ir.functions[implementation as usize];
        match &edge.overridden {
            ResolvedFunctionOverrideTarget::Module(callable) => {
                match ir.checked_callable_functions.get(callable) {
                    Some(&overridden) => {
                        // EVERY target, not just the nearest: one method can override its
                        // superclass's and an interface's at once, and the two want different
                        // things — one slot replaced, one interface number pointed here.
                        let targets = map.entry(implementation).or_default();
                        if !targets.contains(&overridden) {
                            targets.push(overridden);
                        }
                    }
                    None => {
                        return Err(format!(
                            "an override of a method declared in another file (`{}.{}`)",
                            class.fq_name(),
                            function.name
                        ))
                    }
                }
            }
            ResolvedFunctionOverrideTarget::External(_) => {
                if any_slot(function).is_none() {
                    return Err(format!(
                        "an override of a dependency method (`{}.{}`)",
                        class.fq_name(),
                        function.name
                    ));
                }
            }
        }
    }
    Ok(map)
}

/// Property name → the (class, name) of the property it overrides, for `class`'s properties.
fn overridden_properties(
    ir: &IrFile,
    id: ClassId,
    class: &IrClass,
) -> Result<HashMap<String, Vec<(ClassId, String)>>, Unsupported> {
    let mut map: HashMap<String, Vec<(ClassId, String)>> = HashMap::new();
    let Some(overrides) = ir.property_overrides.get(&class.fq_name_id()) else {
        return Ok(map);
    };
    for edge in overrides {
        let ResolvedPropertyOverrideTarget::Module(implementation) = &edge.implementation else {
            continue;
        };
        let Some(property) = ir.checked_properties.get(implementation) else {
            continue;
        };
        if property.class != Some(id) {
            continue;
        }
        let overridden = match &edge.overridden {
            ResolvedPropertyOverrideTarget::Module(overridden) => ir
                .checked_properties
                .get(overridden)
                .and_then(|base| Some((base.class?, base.name.clone()))),
            ResolvedPropertyOverrideTarget::External(_) => None,
        };
        let Some(overridden) = overridden else {
            return Err(format!(
                "an override of a property declared outside this file (`{}.{}`)",
                class.fq_name(),
                edge.name
            ));
        };
        // Every target: a property can override a superclass's and an interface's at once, and
        // the direct base's edge (depth 1) is the one whose slot is replaced, so it goes first.
        let targets = map.entry(property.name.clone()).or_default();
        if !targets.contains(&overridden) {
            if edge.depth == 1 {
                targets.insert(0, overridden);
            } else {
                targets.push(overridden);
            }
        }
    }
    Ok(map)
}

/// An override must be callable through the overridden slot's signature. Kotlin permits a
/// covariant return (`A` → `B`, both references) and generic specialization (`T` → `Int`); the
/// second changes the machine representation and would need a bridge method, which does not
/// exist here yet.
fn check_same_representation(
    ir: &IrFile,
    implementation: &IrFunction,
    overridden: &IrFunction,
) -> Result<(), Unsupported> {
    let same = implementation.params.len() == overridden.params.len()
        && implementation
            .params
            .iter()
            .zip(&overridden.params)
            .all(|(a, b)| c_kind(*a) == c_kind(*b))
        && c_kind(implementation.ret) == c_kind(overridden.ret);
    if same {
        return Ok(());
    }
    let owner = implementation
        .dispatch_receiver
        .map_or_else(String::new, TypeName::render);
    let _ = ir;
    Err(format!(
        "an override that changes a parameter's or the result's representation (`{owner}.{}`; \
         a bridge method is needed)",
        implementation.name
    ))
}

/// A symbol-safe spelling of a Kotlin name: every non-alphanumeric character becomes `_`.
pub(super) fn c_identifier(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    out
}

/// Kotlin overloads share a name and C has no overloading, so a repeat gets a suffix. The suffixed
/// spelling is itself checked for collisions: a file declaring both `greet` and `greet__1` would
/// otherwise hand two functions the same symbol, and the linker would silently pick one of them.
/// Classes draw from the same pool as functions because their descriptors and constructors are
/// ordinary C objects too: a class `X` and a top-level function `type_X` must not both become
/// `kt_type_X`.
pub(super) struct Symbols {
    /// The sanitized, unique base name of each class, from which `struct kt_class_<base>`,
    /// `kt_type_<base>`, `kt_<base>__init` and the struct members `<base>_f<i>` derive.
    pub classes: Vec<String>,
    /// C symbol per IR function index.
    pub functions: Vec<String>,
}

fn class_symbol_family(base: &str) -> [String; 6] {
    [
        format!("kt_type_{base}"),
        format!("kt_vtable_{base}"),
        format!("kt_refs_{base}"),
        format!("kt_{base}__init"),
        format!("kt_singleton_{base}"),
        format!("kt_singleton_{base}_get"),
    ]
}

pub(super) fn symbols(ir: &IrFile, reserved: HashSet<String>) -> Symbols {
    // The runtime's own symbols are taken before any of the program's are handed out. A Kotlin
    // `fun cast(value: Any)` would otherwise be named `kt_cast`, which the runtime already defines,
    // and the link fails with a duplicate symbol that says nothing about the Kotlin name behind it.
    let mut taken: HashSet<String> = reserved;
    let mut unique = |base: String, family: &dyn Fn(&str) -> Vec<String>| -> String {
        let mut candidate = base.clone();
        let mut ordinal = 0;
        loop {
            let names = family(&candidate);
            if names.iter().all(|name| !taken.contains(name)) {
                taken.extend(names);
                return candidate;
            }
            ordinal += 1;
            candidate = format!("{base}__{ordinal}");
        }
    };

    // Classes first: a class reserves a whole family of names, and a function only one.
    let classes: Vec<String> = ir
        .classes
        .iter()
        .map(|class| {
            unique(c_identifier(&class.fq_name()), &|base| {
                class_symbol_family(base).to_vec()
            })
        })
        .collect();

    let package = ir
        .package
        .as_deref()
        .map(c_identifier)
        .filter(|package| !package.is_empty());
    let mut functions = vec![String::new(); ir.functions.len()];
    // Methods are named by their class, then everything else by the package.
    for (class, base) in ir.classes.iter().zip(&classes) {
        for &fid in &class.methods {
            let function = &ir.functions[fid as usize];
            let method = format!("kt_{base}_{}", c_identifier(&function.name));
            functions[fid as usize] = unique(method, &|name| vec![name.to_string()]);
        }
    }
    for (index, function) in ir.functions.iter().enumerate() {
        if !functions[index].is_empty() {
            continue;
        }
        let base = match &package {
            Some(package) => format!("kt_{package}_{}", c_identifier(&function.name)),
            None => format!("kt_{}", c_identifier(&function.name)),
        };
        functions[index] = unique(base, &|name| vec![name.to_string()]);
    }
    Symbols { classes, functions }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrField, IrProperty, IrfFlags};

    fn function(name: &str, owner: &str, params: Vec<Ty>, ret: Ty, abstract_: bool) -> IrFunction {
        IrFunction {
            name: name.to_string(),
            params,
            ret,
            body: if abstract_ { None } else { Some(0) },
            is_static: false,
            dispatch_receiver: Some(crate::types::type_name(owner)),
            param_checks: Vec::new(),
        }
    }

    fn field(name: &str, ty: Ty) -> IrField {
        IrField {
            name: name.to_string(),
            ty,
            type_param: None,
            default: None,
            flags: IrfFlags::default(),
        }
    }

    fn class(ir: &mut IrFile, name: &str, superclass: &str, depth: u32) -> ClassId {
        let mut class = IrClass::synthetic(crate::types::type_name(name));
        class.superclass = crate::types::type_name(superclass);
        class.is_open = true;
        let id = ir.classes.len() as ClassId;
        ir.classes.push(class);
        let mut applied = vec![crate::ir::IrAppliedClassifier {
            classifier: crate::types::type_name(name),
            applied: Ty::obj(name),
            depth: 0,
        }];
        if depth > 0 {
            applied.push(crate::ir::IrAppliedClassifier {
                classifier: crate::types::type_name(superclass),
                applied: Ty::obj(superclass),
                depth,
            });
        }
        ir.classifier_hierarchies
            .insert(crate::types::type_name(name), applied);
        id
    }

    fn add_method(ir: &mut IrFile, class: ClassId, function: IrFunction) -> FunId {
        let fid = ir.functions.len() as FunId;
        ir.functions.push(function);
        ir.classes[class as usize].methods.push(fid);
        fid
    }

    fn record_override(ir: &mut IrFile, class: ClassId, implementation: FunId, overridden: FunId) {
        // The frontend records overrides by callable identity; the test uses the function index
        // as that identity, which is what `checked_callable_functions` maps back.
        use crate::fir::CallableId;
        let implementation_id = CallableId::from_raw(1000 + implementation);
        let overridden_id = CallableId::from_raw(1000 + overridden);
        ir.checked_callable_functions
            .insert(implementation_id, implementation);
        ir.checked_callable_functions
            .insert(overridden_id, overridden);
        let name = ir.functions[implementation as usize].name.clone();
        let owner = ir.classes[class as usize].fq_name_id();
        ir.function_overrides
            .entry(owner)
            .or_default()
            .push(crate::ir::IrFunctionOverride {
                implementation: ResolvedFunctionOverrideTarget::Module(implementation_id),
                implementation_function: None,
                implementation_owner: owner,
                overridden: ResolvedFunctionOverrideTarget::Module(overridden_id),
                overridden_owner: ir.functions[overridden as usize]
                    .dispatch_receiver
                    .expect("instance method"),
                overridden_is_interface: false,
                name,
                declared_parameters: Vec::new(),
                declared_result: Ty::Unit,
                applied_parameters: Vec::new(),
                applied_result: Ty::Unit,
                implementation_parameters: Vec::new(),
                implementation_result: Ty::Unit,
                suspend: false,
                depth: 1,
            });
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
        assert_eq!(model.slot(a, &SlotKey::Function(a_name)), Some(3));
        assert_eq!(model.slot(a, &SlotKey::Function(a_other)), Some(4));
        assert_eq!(model.layout(a).vtable[3], Slot::Function(a_name));

        let b_layout = model.layout(b);
        assert_eq!(
            b_layout.vtable[3],
            Slot::Function(b_name),
            "the override takes the slot its base assigned"
        );
        assert_eq!(
            b_layout.vtable[4],
            Slot::Function(a_other),
            "inherited unchanged"
        );
        assert_eq!(
            b_layout.vtable[5],
            Slot::Function(b_extra),
            "new members append"
        );
        assert_eq!(
            model.slot(b, &SlotKey::Function(a_name)),
            Some(3),
            "a call naming the base method finds the same slot on the subclass"
        );
        assert_eq!(model.slot(b, &SlotKey::Function(b_name)), Some(3));
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
        assert_eq!(model.layout(a).vtable[3], Slot::Abstract);
        assert_eq!(model.layout(b).vtable[3], Slot::Abstract);
        assert_eq!(model.layout(b).vtable[4], Slot::Function(b_g));
        assert_eq!(
            model.layout(c).vtable[3],
            Slot::Function(c_f),
            "an override two levels down replaces the slot the root declared"
        );
        assert_eq!(model.layout(c).vtable[4], Slot::Function(b_g));
        assert_eq!(model.layout(c).vtable.len(), 5);
        assert_eq!(model.order, vec![a, b, c]);
    }

    #[test]
    fn kotlin_any_members_are_matched_by_name_and_signature() {
        let mut ir = IrFile::default();
        let p = class(&mut ir, "P", "kotlin/Any", 0);
        let to_string = add_method(
            &mut ir,
            p,
            function("toString", "P", vec![], Ty::String, false),
        );
        // Same name, different arity: an overload, not an override.
        let overload = add_method(
            &mut ir,
            p,
            function("toString", "P", vec![Ty::Int], Ty::String, false),
        );
        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(p).vtable[2], Slot::Function(to_string));
        assert_eq!(model.layout(p).vtable[3], Slot::Function(overload));
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
    fn an_override_that_changes_representation_is_declined() {
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
        let error = build(&ir).expect_err("a KRef slot cannot hold a kt_int implementation");
        assert!(error.contains("representation"), "{error}");
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
            visibility: crate::types::Visibility::Public,
            annotations: Box::new([]),
            initializer: None,
            storage_ty: None,
            backing_field: Some(0),
            is_var: false,
            is_open: true,
            is_private: false,
            setter_is_private: false,
            getter: None,
            setter: None,
            getter_jvm_name: None,
            setter_jvm_name: None,
            needs_access_bridge: false,
        }];
        let model = build(&ir).expect("layout");
        assert_eq!(
            model.layout(a).vtable[3],
            Slot::FieldGetter { class: a, field: 0 }
        );
        assert_eq!(model.slot(a, &SlotKey::Getter(a, "v".to_string())), Some(3));
    }

    #[test]
    fn out_of_scope_classes_are_declined_by_name() {
        let mut ir = IrFile::default();
        let id = class(&mut ir, "D", "kotlin/Any", 0);
        // A data class, an interface, a value class and an `inner` class are deliberately absent
        // from this list: each lowers like the class it is now. What remains are constructs with
        // no realization at all yet.
        ir.classes[id as usize].is_annotation = true;
        assert!(build(&ir)
            .expect_err("declined")
            .contains("an annotation class"));
    }
}
