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

/// `kotlin.Throwable`'s own storage: the header, then `message`.
const THROWABLE_MESSAGE_OFFSET: u32 = HEADER_SIZE;
const THROWABLE_FIELDS_END: u32 = HEADER_SIZE + 8;
const THROWABLE_REFERENCE_OFFSETS: &[u32] = &[THROWABLE_MESSAGE_OFFSET];

/// The base `superclass` names, when it is one the runtime owns rather than one this file declares.
///
/// Which classes those are, and under which of the two providers' spellings they arrive, is
/// [`crate::native::intrinsics::throwable_descriptor`]'s to know — this adds only the LAYOUT, which
/// is a fact about emitting a subclass rather than about naming the base.
pub(super) fn external_base(superclass: TypeName) -> Option<ExternalBase> {
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
    /// An override reachable through a base whose signature has a different REPRESENTATION:
    /// `A<T : Number>.foo(): T` erases its result to a reference, and `Z : A<Int>` overriding it
    /// returns an unboxed machine integer. The base's slot cannot hold that body — a caller reading
    /// the slot through `A` would read an integer as a pointer — so it holds this instead: a small
    /// function with the BASE's signature that converts each operand and forwards.
    ///
    /// It forwards by DISPATCH rather than by calling the override directly, and that is the whole
    /// reason it is a slot number here and not a function id. A further subclass replaces `target`
    /// with its own body, and a bridge that had named the override would keep running the wrong
    /// one; re-dispatching through the receiver's own vtable reaches whatever that receiver
    /// actually is.
    Bridge {
        /// The base method whose signature this entry wears.
        declared: FunId,
        /// The slot to forward through, and the method whose carriers that slot expects.
        ///
        /// `None` where there is no slot to forward through: an INTERFACE's own bridge stands in
        /// the default an implementor inherits, and the slot it would name is the interface's
        /// table index rather than that implementor's. A default is reached only where nothing
        /// overrides the member, so calling `target` outright is the same answer — and it is the
        /// same thing a plain `Slot::Function` default already does.
        target_slot: Option<u32>,
        target: FunId,
    },
    /// The same thing for a property ACCESSOR reached through an INTERFACE that declares it with
    /// a different representation: `interface C<T> { var size: T }` erases its accessors to a
    /// reference while `class B : C<Int>, A()` inherits an unboxed machine integer. The
    /// interface's program-wide number holds this rather than an alias of the class's own slot,
    /// which would have a caller read that integer as a pointer.
    ///
    /// It carries TYPES where [`Slot::Bridge`] carries function ids, because neither end need be
    /// a source accessor: either may be a synthesized field access, which has no declaration to
    /// name. It forwards by DISPATCH for the same reason the method bridge does.
    /// `kotlin.Any`'s three for an ANNOTATION instance. Kotlin defines all three over the
    /// annotation's members, and an ARRAY member is compared, hashed and rendered by CONTENT —
    /// which is what separates it from a data class, where an array member is compared by
    /// identity. `hashCode` is the contract sum of `(127 * name.hashCode()) xor value.hashCode()`,
    /// and a program can read it: the corpus computes the same sum in Kotlin and compares.
    AnnotationMember { class: ClassId, member: ValueMember },
    AccessorBridge {
        /// The property type as the INTERFACE declares it: the carrier this entry wears.
        declared: Ty,
        /// The property type as the implementation has it: the carrier `target_slot` expects.
        implemented: Ty,
        /// Whether this stands in for the setter — which takes the value and answers nothing —
        /// rather than the getter.
        setter: bool,
        target_slot: u32,
    },
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
    /// Interface members this class implements through a declaration of a DIFFERENT
    /// representation, by the interface's own spelling of the member. The interface's
    /// program-wide number is filled with the bridge recorded here instead of an alias of this
    /// class's slot — see [`Slot::AccessorBridge`].
    pub interface_bridges: HashMap<SlotKey, Slot>,
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

/// Build the layout and vtable of every class in `ir`, superclasses first.
pub(super) fn build(ir: &IrFile) -> Result<ClassModel, Unsupported> {
    for class in &ir.classes {
        check_supported(class)?;
    }
    let order = hierarchy_order(ir)?;
    // Before the layouts, because a class laying itself out needs to know which interfaces it
    // implements: an accessor it supplies with no override edge to name it — interface delegation
    // synthesizes exactly those — is matched against the members those interfaces declare.
    let interfaces = interface_closure(ir);

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
            layout_class(ir, id, superclass, parent, &interfaces[id as usize])?
        };
        layouts[id as usize] = Some(layout);
    }
    let mut layouts: Vec<ClassLayout> = layouts
        .into_iter()
        .map(|layout| layout.expect("every class was laid out"))
        .collect();
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
        // The provisional layout numbered this interface's members as if they were a class's, and
        // ONE of its entries can be named by several keys: `interface Base2 : Base` redeclaring
        // `test` registers both its own spelling and `Base`'s at the same slot. They are one
        // member, so they are numbered together — numbering them one by one would give `Base2`'s
        // spelling a second number, which every implementor that registered `Base`'s would then
        // miss, silently taking the interface's default over its own implementation. Grouping by
        // slot also makes the numbering independent of the map's iteration order, which sorting by
        // slot alone did not: two keys sharing a slot compared equal.
        let mut groups: std::collections::BTreeMap<u32, Vec<SlotKey>> =
            std::collections::BTreeMap::new();
        for (key, slot) in &layout.slots {
            // A member occupying one of `kotlin.Any`'s slots is already numbered, and numbered the
            // same way for every object there is: a `fun interface` that overrides `toString`
            // wants slot 2, which is where the runtime's own rendering looks. Numbering it again in
            // the interface region would leave the slot every caller actually uses empty. Both
            // spellings of such a member — `Any(2)` and the method's own key — are skipped here and
            // carried through below.
            if *slot < ANY_SLOTS {
                continue;
            }
            groups.entry(*slot).or_default().push(key.clone());
        }
        for (slot, mut keys) in groups {
            keys.sort_by_key(key_order);
            let entry = layout.vtable[slot as usize].clone();
            // What each key is already numbered as, through some base. A key with none is a
            // spelling this interface introduced for a member a sibling key already names, so it
            // joins that number instead of taking one of its own. `interface D : A, B` overriding a
            // member BOTH declare is why each key keeps its own number when it has one: `A.f` and
            // `B.f` are two members of the program that one entry happens to fill, and an
            // implementor has to fill both numbers.
            let inherited: Vec<Option<u32>> = keys
                .iter()
                .map(|key| {
                    interfaces[id as usize]
                        .iter()
                        .find_map(|&base| relative[base as usize].get(key).copied())
                })
                .collect();
            // A key with no number of its own joins one that HAS one — but only a number that
            // carries the entry as it stands. A number needing a bridge wears the base's
            // signature, and a key that does not need one is callable with the entry's: sharing
            // the two would make a caller through this interface pass what the bridge converts.
            // `interface Z1 : A<String>, B<String, Int>` overriding `foo(String, Int)` is that
            // case — `A`'s number takes the body, `B`'s takes a bridge, and `Z1`'s own spelling
            // has to join `A`'s.
            let plain = |key: &SlotKey| !layout.interface_bridges.contains_key(key);
            let joined = match keys
                .iter()
                .zip(&inherited)
                .find_map(|(key, number)| number.filter(|_| plain(key)))
            {
                Some(number) => number,
                None => {
                    let assigned = members.len() as u32;
                    members.push(InterfaceMember {
                        interface: id,
                        key: keys
                            .iter()
                            .find(|key| plain(key))
                            .or_else(|| keys.first())
                            .expect("a group has a key")
                            .clone(),
                        aliases: Vec::new(),
                        // Filled by the loop below, which knows which key this number wears.
                        defaults: Vec::new(),
                    });
                    assigned
                }
            };
            let numbered: Vec<u32> = inherited
                .iter()
                .map(|number| number.unwrap_or(joined))
                .collect();
            for (key, &number) in keys.iter().zip(&numbered) {
                // An interface redeclaring an inherited member with a body replaces what the base
                // supplied, for classes that implement neither themselves.
                //
                // Per KEY, because one entry can fill several numbers and need a bridge in only
                // some of them: `interface Z1 : A<String>, B<String, Int>` overriding
                // `foo(String, Int)` is callable through `A`'s number as it stands — `A.foo(T,
                // Int)` already carries the second operand as a machine integer — and through
                // `B`'s number only after conversion, since `B.foo(T, U)` carries both as
                // references. Reading the entry for both numbers put the raw body where `B`'s
                // caller passes a boxed integer.
                let supplied = layout
                    .interface_bridges
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| entry.clone());
                if !matches!(supplied, Slot::Abstract) {
                    let member = &mut members[number as usize];
                    match member.defaults.iter_mut().find(|(who, _)| *who == id) {
                        Some((_, existing)) => *existing = supplied,
                        None => member.defaults.push((id, supplied)),
                    }
                }
                // A class implementing `Both : Named` registers what it overrides under the key the
                // override names, and that can be ANY interface's spelling of the same member — a
                // property's key carries its declaring class, and a method's carries the declaring
                // interface's own `FunId`. So the member records every spelling that shares its
                // number, and the class is looked up by all of them.
                for (alias, _) in keys
                    .iter()
                    .zip(&numbered)
                    .filter(|(_, &other)| other == number)
                {
                    let member = &mut members[number as usize];
                    if *alias != member.key && !member.aliases.contains(alias) {
                        member.aliases.push(alias.clone());
                    }
                }
                own.insert(key.clone(), number);
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
                defaults[base as usize + member_slot] = supplied(member, id, interfaces);
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
            let spellings = || std::iter::once(&member.key).chain(&member.aliases);
            let entry = if !implemented {
                Slot::Abstract
            } else if let Some(bridge) =
                spellings().find_map(|key| layouts[id as usize].interface_bridges.get(key))
            {
                // The class implements the member with a different REPRESENTATION than the
                // interface declares, so this number cannot alias its slot — it wears the
                // interface's carrier and converts.
                bridge.clone()
            } else {
                match spellings().find_map(|key| layouts[id as usize].slots.get(key)) {
                    // The class (or an ancestor) implements the member: its own slot already holds
                    // the most derived implementation, whatever further overrides did to it.
                    Some(&slot) => vtable[slot as usize].clone(),
                    None => supplied(member, id, interfaces),
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

/// A total order over slot keys, so an interface's members are numbered the same way on every
/// run — the map they come from has no order of its own.
fn key_order(key: &SlotKey) -> (u8, u32, String) {
    match key {
        SlotKey::Any(slot) => (0, *slot, String::new()),
        SlotKey::Function(fid) => (1, *fid, String::new()),
        SlotKey::Getter(class, name) => (2, *class, name.clone()),
        SlotKey::Setter(class, name) => (3, *class, name.clone()),
    }
}

/// One member of one interface, in the program-wide numbering.
struct InterfaceMember {
    interface: ClassId,
    key: SlotKey,
    /// The same member as spelled through each interface that inherits it.
    aliases: Vec<SlotKey>,
    /// What a class that implements the interface without supplying this member gets, PER
    /// interface that supplies one — the interface's own body where it has one.
    ///
    /// Several interfaces can supply one number: `interface Z1 : A, B` and `interface Z2 : B, A`
    /// each override the member `A` and `B` both declare, and both are numbered onto `A`'s entry.
    /// One recorded answer meant the last interface laid out won, and a class implementing the
    /// other answered with a body it does not have. [`supplied`] chooses against the
    /// implementor's own hierarchy instead.
    defaults: Vec<(ClassId, Slot)>,
}

/// What an implementor inherits for one interface member: the body of the MOST DERIVED interface
/// in its own hierarchy that supplies one, or the abstract trap when none does.
///
/// "Most derived" is the supplier that no other candidate extends. Kotlin guarantees there is one:
/// a class inheriting two unrelated bodies for the same member does not compile without an
/// override of its own, and that override is what this is consulted instead of.
fn supplied(member: &InterfaceMember, class: ClassId, interfaces: &[Vec<ClassId>]) -> Slot {
    let reaches =
        |from: ClassId, to: ClassId| from == to || interfaces[from as usize].contains(&to);
    let candidates: Vec<&(ClassId, Slot)> = member
        .defaults
        .iter()
        .filter(|(supplier, _)| reaches(class, *supplier))
        .collect();
    candidates
        .iter()
        .find(|(supplier, _)| {
            !candidates
                .iter()
                .any(|(other, _)| other != supplier && reaches(*other, *supplier))
        })
        .map(|(_, slot)| slot.clone())
        .unwrap_or(Slot::Abstract)
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
            && external_base(class.superclass).is_none()
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
    interfaces: &[ClassId],
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
        let kind = c_kind(super::captures::physical_ty(ir, id, index as u32, field.ty));
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
    // Bridges against an INTERFACE's member numbers, which are placed program-wide rather than in
    // this table. Filled below wherever an implementation and the interface's declaration disagree
    // about representation, and read where the interface region is laid out.
    // Inherited, because a subclass's vtable inherits the slot the bridge forwards through and
    // would otherwise take the raw implementation at the interface's number.
    let mut interface_bridges: HashMap<SlotKey, Slot> = parent
        .map(|parent| parent.interface_bridges.clone())
        .unwrap_or_default();
    let (mut vtable, mut slots) = match (parent, external) {
        (Some(parent), _) => (parent.vtable.clone(), parent.slots.clone()),
        // A base the runtime owns fills `kotlin.Any`'s three slots its own way — a `Throwable`
        // renders as `qualified.Name: message` rather than by identity — and a subclass inherits
        // that table before putting anything of its own in it.
        (None, Some(base)) => {
            let (_, slots) = any_vtable();
            (base.any_slots.map(Slot::Runtime).to_vec(), slots)
        }
        (None, None) => any_vtable(),
    };

    // Which methods are property accessors, so their slots are keyed by the property. A property
    // whose accessor has no BODY — an abstract `val` in an interface — carries no accessor id, and
    // its accessor reaches the method list as an ordinary method; matching the declared accessor
    // name is what ties the two back together, so the interface and the class implementing it
    // agree on one key for the member.
    //
    // Only where the property has no STORAGE of its own. A property with a field is read through
    // that field — the accessor for it is synthesized below — so a method that happens to spell
    // the accessor's name is a method and not that accessor. `class Bottom(val data: Int) : Top {
    // override fun getData(): Int = data }` declares both, which is legal Kotlin and not even
    // unusual; keying the method as the property's getter took it out of the method numbering
    // entirely, so the base's slot kept the base's body and a call through the base jumped into
    // whatever stood there.
    let mut accessor_keys: HashMap<FunId, SlotKey> = HashMap::new();
    for property in &class.properties {
        if let Some(getter) = property.getter {
            accessor_keys.insert(getter, SlotKey::Getter(id, property.name.clone()));
        }
        if let Some(setter) = property.setter {
            accessor_keys.insert(setter, SlotKey::Setter(id, property.name.clone()));
        }
    }
    // The name fallback, through the same reading every CALL makes, so the two cannot disagree.
    for &fid in &class.methods {
        if let Some(key) = accessor_key_by_name(ir, id, fid) {
            accessor_keys.entry(fid).or_insert(key);
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
            // `invoke` on a class implementing a FUNCTION TYPE takes the one other fixed slot this
            // target has: the runtime names it (`KT_SLOT_INVOKE`) and every caller through a
            // function type reads it, a lambda's body included.
            None => ir
                .function_overrides
                .get(&class.fq_name_id())
                .into_iter()
                .flatten()
                .filter(|edge| {
                    matches!(
                        edge.implementation,
                        ResolvedFunctionOverrideTarget::External(_)
                    ) || edge.implementation_function == Some(fid)
                })
                .find(|edge| {
                    edge.implementation_function == Some(fid)
                        || ir
                            .checked_callable_functions
                            .get(match &edge.implementation {
                                ResolvedFunctionOverrideTarget::Module(callable) => callable,
                                ResolvedFunctionOverrideTarget::External(_) => return false,
                            })
                            == Some(&fid)
                })
                .and_then(|edge| external_invoke_slot(ir, edge, function)),
        };
        // What this method overrides, split by what each target owns: a class base owns a slot in
        // this vtable to replace, while an interface base owns a number in the program-wide
        // interface region, which is pointed at this method's slot once that slot is known.
        // The interface bases this method satisfies, each with the declaration a BRIDGE against
        // that number would wear — `None` where the representations already agree.
        let mut interface_keys: Vec<(SlotKey, Option<FunId>)> = Vec::new();
        let mut class_replaces = None;
        // A base whose signature has a different REPRESENTATION keeps its own slot and gets a
        // bridge placed in it once this method's slot is known. A class base can always take one;
        // an interface base's number is program-wide rather than this vtable's, so one there is
        // still declined.
        let mut bridged: Vec<(u32, FunId)> = Vec::new();
        // The same, for a property accessor whose base declares a different representation. It is
        // kept apart because neither end need be a source accessor, so the entry carries the two
        // property TYPES rather than two declaration ids.
        let mut accessor_bridged: Vec<(u32, Ty, Ty, bool)> = Vec::new();
        for &overridden in overridden_functions.get(&fid).into_iter().flatten() {
            let matches = same_representation(function, &ir.functions[overridden as usize]);
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
                // The interface's number is placed program-wide rather than in this vtable, so
                // there is no entry HERE to put a bridge in. The bridge is recorded against the
                // interface's own key instead, and read where that number is filled — which is
                // the arrangement a property accessor's interface bridge already uses.
                interface_keys.push((key, (!matches).then_some(overridden)));
                continue;
            }
            let slot = slots.get(&key).copied().ok_or_else(|| {
                format!("an override with no slot to replace (`{}`)", function.name)
            })?;
            if matches {
                class_replaces = Some(slot);
            } else {
                bridged.push((slot, overridden));
            }
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
            // A property ACCESSOR is recorded as a fresh declaration whether or not its property
            // overrides one — common lowering pushes every accessor into `fresh_method_decls`
            // without reading the modifier — so for one the table above cannot be believed. What
            // can be: an OPEN, non-private base member with the same name and machine signature
            // that this class redeclares. Kotlin rejects a fresh redeclaration of such a member
            // ("hides member of supertype and needs `override`"), so matching it is reading the
            // language's own rule rather than guessing. For a method the table is right and this
            // never fires; for an accessor of `override val Foo.bar` it is what points the base's
            // slot at the override instead of giving it one of its own.
            None => inherited_open_slot(ir, superclass, function, &slots),
        };
        let replaces = match replaces {
            Some(slot) => Some(slot),
            None => match inherited_replaces {
                Some(slot) => Some(slot),
                None => match &own_key {
                    SlotKey::Getter(_, name) | SlotKey::Setter(_, name) => {
                        let setter = matches!(own_key, SlotKey::Setter(..));
                        match overridden_property_slot(
                            ir,
                            &overridden_properties,
                            &slots,
                            name,
                            setter,
                            &class.fq_name(),
                        )? {
                            // The base declares the property with a different REPRESENTATION, so
                            // its slot cannot hold this accessor. This one takes a slot of its own
                            // and the base's gets a bridge, exactly as a method's does.
                            Some((slot, declared))
                                if c_kind(declared) != c_kind(own_property_ty(class, name)) =>
                            {
                                accessor_bridged.push((
                                    slot,
                                    declared,
                                    own_property_ty(class, name),
                                    setter,
                                ));
                                None
                            }
                            Some((slot, _)) => Some(slot),
                            // No edge names a base for this accessor. The base's own accessor may
                            // be SYNTHESIZED — `override lateinit var x` has no method to match
                            // by signature — so the inherited slot is found by the property's
                            // NAME instead. `class E : B(), C by D()` is the case: the delegation
                            // supplies `x` again with no edge of its own, and kotlinc answers
                            // with the delegate (KT-70417), which is what taking the base's slot
                            // makes true here.
                            None => inherited_accessor_slot(ir, superclass, name, setter, &slots),
                        }
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
        for (key, bridge) in interface_keys {
            // The number wears the INTERFACE's signature and converts; the slot map still points
            // at this method, because everything else that reads the map wants the
            // implementation. Which of the two a caller gets is decided where the number is
            // filled, and it prefers the bridge.
            if let Some(declared) = bridge {
                interface_bridges
                    .entry(key.clone())
                    .or_insert(Slot::Bridge {
                        declared,
                        // An INTERFACE's entry is a default an implementor inherits, and this
                        // slot is the interface's own table index — not that implementor's.
                        target_slot: (!class.is_interface).then_some(slot),
                        target: fid,
                    });
            }
            slots.insert(key, slot);
        }
        // The base keeps its own slot and its own signature; what changes is only what stands in
        // it. Forwarding by DISPATCH rather than to this method by id is what keeps a further
        // subclass's override reachable through the same base.
        for (base_slot, declared) in bridged {
            vtable[base_slot as usize] = Slot::Bridge {
                declared,
                target_slot: Some(slot),
                target: fid,
            };
        }
        for (base_slot, declared, implemented, setter) in accessor_bridged {
            vtable[base_slot as usize] = Slot::AccessorBridge {
                declared,
                implemented,
                setter,
                target_slot: slot,
            };
        }
    }

    // Open or overriding properties with no source accessor still dispatch: synthesize the
    // field access as a slot.
    for property in &class.properties {
        let overrides = overridden_properties.contains_key(&property.name);
        // A property a SUBCLASS hands to an interface is dispatched through as well, whether or
        // not it is `open` here: `class B : C, A<Int>()` implements `C.size` with the `size` that
        // `A` declares plainly, and the interface's number has to reach it. Kotlin needs no
        // `open` for that — B overrides nothing — so the declaration alone cannot say it.
        if !property.is_open && !overrides && !implements_an_interface(ir, id, &property.name) {
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
            // A base declaring a different REPRESENTATION keeps its slot and takes a bridge, as
            // above: a synthesized field access is exactly as unusable through the base's carrier
            // as a source accessor would be.
            let bridged = match replaces {
                Some((slot, declared)) if c_kind(declared) != c_kind(property.ty) => {
                    Some((slot, declared))
                }
                _ => None,
            };
            let slot = match replaces.filter(|_| bridged.is_none()) {
                Some((slot, _)) => {
                    vtable[slot as usize] = entry;
                    slot
                }
                None => {
                    vtable.push(entry);
                    (vtable.len() - 1) as u32
                }
            };
            if let Some((base_slot, declared)) = bridged {
                vtable[base_slot as usize] = Slot::AccessorBridge {
                    declared,
                    implemented: property.ty,
                    setter,
                    target_slot: slot,
                };
            }
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

    register_inherited_interface_members(ir, class, &mut slots, &mut interface_bridges)?;
    register_accessors_without_an_edge(
        ir,
        id,
        class,
        interfaces,
        &mut slots,
        &mut interface_bridges,
    );

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
    // An annotation INSTANCE is a value whose three `kotlin.Any` members Kotlin defines over its
    // members rather than by identity. A declaration that carries none of them is still one — the
    // language gives no way to write them — so there is nothing to preserve here as a value class
    // has to.
    if class.is_annotation {
        for (slot, member) in [
            (0, ValueMember::Equals),
            (1, ValueMember::HashCode),
            (2, ValueMember::ToString),
        ] {
            vtable[slot] = Slot::AnnotationMember { class: id, member };
        }
    }
    if class.is_value {
        if class.fields.len() != 1 {
            return Err(format!(
                "a value class with {} fields (`{}`)",
                class.fields.len(),
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
        interface_bridges,
    })
}

/// Point an interface's property numbers at accessors this class supplies with NO override edge to
/// name them.
///
/// Interface delegation is the shape: `class Q(a: A) : A by a` synthesizes `Q`'s own `x` and its
/// accessors, and nothing records that they implement `A.x` — there is no source declaration to
/// carry the edge, so the edge tables say nothing and `A`'s number found no implementation.
///
/// Kotlin has already decided they do implement it: a class does not compile with an interface
/// property left unimplemented, and it cannot declare a second property of that name beside the
/// inherited one. So an interface in this class's hierarchy declaring the same name IS the member
/// these accessors fill — which is why matching by name is reading the language's rule rather than
/// guessing, the same ground `inherited_open_slot` stands on.
///
/// `or_insert` throughout: a source `override val` has an edge, and that edge's answer wins.
fn register_accessors_without_an_edge(
    ir: &IrFile,
    id: ClassId,
    class: &IrClass,
    interfaces: &[ClassId],
    slots: &mut HashMap<SlotKey, u32>,
    interface_bridges: &mut HashMap<SlotKey, Slot>,
) {
    for property in &class.properties {
        for setter in [false, true] {
            let spell = |owner: ClassId| {
                if setter {
                    SlotKey::Setter(owner, property.name.clone())
                } else {
                    SlotKey::Getter(owner, property.name.clone())
                }
            };
            let Some(&slot) = slots.get(&spell(id)) else {
                continue;
            };
            for &interface in interfaces {
                let Some(declared) = ir.classes[interface as usize]
                    .properties
                    .iter()
                    .find(|candidate| candidate.name == property.name)
                else {
                    continue;
                };
                // A `val` in the interface has no setter number to fill.
                if setter && !declared.is_var {
                    continue;
                }
                let key = spell(interface);
                // The same representation question the edge-carrying paths ask: an interface
                // declaring `val x: T` erases its accessor to a reference while the class supplies
                // an unboxed machine integer, and the number takes a bridge rather than an alias.
                if c_kind(declared.ty) != c_kind(property.ty) {
                    interface_bridges
                        .entry(key.clone())
                        .or_insert(Slot::AccessorBridge {
                            declared: declared.ty,
                            implemented: property.ty,
                            setter,
                            target_slot: slot,
                        });
                }
                // Overwrites rather than `or_insert`: what a class SUPPLIES wins over what it
                // inherited, and the entry already there came from the superclass's layout.
                // `class E : B(), C by D()` is that case — `B` implements `A.x` and the
                // delegation to `D` supplies it again, and kotlinc answers with the delegate
                // (KT-70417).
                slots.insert(key, slot);
            }
        }
    }
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
    interface_bridges: &mut HashMap<SlotKey, Slot>,
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
        // interface's number straight at it would have a caller read an integer as a pointer. The
        // number takes a bridge wearing the interface's carrier instead — the same answer the
        // property path below gives, and the same one the JVM gives by emitting a bridge method.
        let key = function_key(ir, interface, overridden);
        if !same_representation(
            &ir.functions[implementation as usize],
            &ir.functions[overridden as usize],
        ) {
            interface_bridges
                .entry(key.clone())
                .or_insert(Slot::Bridge {
                    declared: overridden,
                    target_slot: Some(slot),
                    target: implementation,
                });
        }
        slots.entry(key).or_insert(slot);
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
        // The same representation question the METHOD path above asks, for the same reason.
        // `interface C<T> { var size: T }` implemented by `class B : C<Int>, A()` where `A`
        // declares `var size: Int` erases the interface's accessors to a REFERENCE while the
        // inherited ones are an unboxed machine integer. Aliasing the interface's number onto them
        // would have a caller read that integer as a pointer; the number takes a bridge wearing
        // the interface's carrier instead. A property is what `bridges/test7.kt` is about, which
        // is why the method check did not catch it.
        let bridged = c_kind(implementation.ty) != c_kind(overridden.ty);
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
                if bridged {
                    interface_bridges
                        .entry(to.clone())
                        .or_insert(Slot::AccessorBridge {
                            declared: overridden.ty,
                            implemented: implementation.ty,
                            setter,
                            target_slot: slot,
                        });
                }
                slots.entry(to).or_insert(slot);
            }
        }
    }
    Ok(())
}

/// The slot an inherited member of the same name and machine signature occupies, searched up the
/// superclass chain. Used only for a method the IR has already identified as an override.
/// [`inherited_slot`] restricted to a base member a subclass CANNOT redeclare freshly: one that is
/// `open` or `abstract`, and not private (a private member is not inherited, so a subclass may
/// declare the same name without overriding anything).
fn inherited_open_slot(
    ir: &IrFile,
    superclass: Option<ClassId>,
    function: &IrFunction,
    slots: &HashMap<SlotKey, u32>,
) -> Option<u32> {
    let slot = inherited_slot(ir, superclass, function, slots)?;
    let overridable = |candidate: &FunId| {
        ir.open_methods.contains(candidate) && !ir.private_methods.contains(candidate)
    };
    let mut at = superclass;
    while let Some(class) = at {
        for candidate in &ir.classes[class as usize].methods {
            if ir.functions[*candidate as usize].name == function.name
                && slots.get(&function_key(ir, class, *candidate)) == Some(&slot)
            {
                return overridable(candidate).then_some(slot);
            }
        }
        at = ir.class_id_by_name(ir.classes[class as usize].superclass);
    }
    None
}

/// The slot an inherited PROPERTY of the same name occupies, searched up the superclass chain.
///
/// By name, because a base's accessor need not be a method at all: a field-backed property's
/// accessors are synthesized, so there is no signature for [`inherited_slot`] to match. Kotlin
/// rejects a fresh redeclaration of an inherited property ("hides member of supertype and needs
/// `override`"), so a subclass property of that name IS that one — except where the base's is
/// PRIVATE, which is not inherited and which a subclass may shadow freely.
fn inherited_accessor_slot(
    ir: &IrFile,
    superclass: Option<ClassId>,
    name: &str,
    setter: bool,
    slots: &HashMap<SlotKey, u32>,
) -> Option<u32> {
    let mut at = superclass;
    while let Some(class) = at {
        if let Some(property) = ir.classes[class as usize]
            .properties
            .iter()
            .find(|candidate| candidate.name == name)
        {
            if property.is_private {
                return None;
            }
            let key = if setter {
                SlotKey::Setter(class, name.to_string())
            } else {
                SlotKey::Getter(class, name.to_string())
            };
            return slots.get(&key).copied();
        }
        at = ir.class_id_by_name(ir.classes[class as usize].superclass);
    }
    None
}

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

/// Whether any class in this file hands `owner`'s property `name` to an interface it implements.
/// Such a property is reached through the interface's number and so has to dispatch, even where
/// the declaration is neither `open` nor an override — the class that implements the interface
/// declares nothing of its own.
fn implements_an_interface(ir: &IrFile, owner: ClassId, name: &str) -> bool {
    ir.property_overrides.values().flatten().any(|edge| {
        edge.overridden_is_interface
            && match &edge.implementation {
                ResolvedPropertyOverrideTarget::Module(id) => ir
                    .checked_properties
                    .get(id)
                    .is_some_and(|property| property.class == Some(owner) && property.name == name),
                ResolvedPropertyOverrideTarget::External(_) => false,
            }
    })
}

/// The type `class` declares for its own property `name`, which is what its accessors carry.
/// `Ty::Unit` for a name the class does not declare, which no accessor of this class is keyed by.
fn own_property_ty(class: &IrClass, name: &str) -> Ty {
    class
        .properties
        .iter()
        .find(|property| property.name == name)
        .map_or(Ty::Unit, |property| property.ty)
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
    // The same fallback the layout makes, and it has to be the same or a call names one key while
    // the table holds the other: an abstract `val` in an interface carries no accessor id, so its
    // accessor reaches the method list as an ordinary method and is tied back by name. Reading it
    // only in the layout is what left `interface A { val x: Int }`'s accessor with a `Getter` slot
    // and every call to it asking for a `Function` one.
    accessor_key_by_name(ir, owner, fid).unwrap_or(SlotKey::Function(fid))
}

/// The property whose accessor a method IS, matched by the accessor's declared NAME.
///
/// Only for a property with no storage of its own. A property with a field is read through that
/// field — its accessor is synthesized — so a method that happens to spell the accessor's name is
/// a method: `class Bottom(val data: Int) { override fun getData(): Int }` declares both, which is
/// legal Kotlin, and keying the method as the property's getter took it out of the method
/// numbering entirely.
fn accessor_key_by_name(ir: &IrFile, owner: ClassId, fid: FunId) -> Option<SlotKey> {
    let class = &ir.classes[owner as usize];
    let function = &ir.functions[fid as usize];
    for property in &class.properties {
        if class.fields.iter().any(|field| field.name == property.name) {
            continue;
        }
        if function.params.is_empty()
            && function.name == crate::names::property_getter_name(&property.name)
        {
            return Some(SlotKey::Getter(owner, property.name.clone()));
        }
        if function.params.len() == 1
            && function.name == crate::names::property_setter_name(&property.name)
        {
            return Some(SlotKey::Setter(owner, property.name.clone()));
        }
    }
    None
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
) -> Result<Option<(u32, Ty)>, Unsupported> {
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
    // The base's own declared type travels with its slot, because whether the slot can simply be
    // REPLACED depends on it: a base declaring `var size: T` carries a reference where an
    // overriding `var size: Int` carries a machine integer, and replacing then has a caller
    // reading the base's slot read that integer as a pointer.
    let declared = ir.classes[*owner as usize]
        .properties
        .iter()
        .find(|property| property.name == *overridden_name)
        .map(|property| property.ty);
    match (slots.get(&key).copied(), declared) {
        (Some(slot), Some(declared)) => Ok(Some((slot, declared))),
        _ => Err(format!(
            "a property override with no slot to replace (`{class_name}.{name}`)"
        )),
    }
}

/// The fixed slot a dependency override occupies, for the one dependency member that has one.
///
/// `kotlin.Function{N}.invoke` is it: a function value's body sits right after `kotlin.Any`'s
/// three, the runtime names that number itself (`KT_SLOT_INVOKE`), and every caller through a
/// function type reads it. A class implementing a function type puts its `invoke` there for the
/// same reason a lambda does.
///
/// Only when every operand and the result are REFERENCES. A caller through the function type
/// passes and reads references, and an `invoke(x: Int): Int` carries machine integers — that one
/// needs a bridge and declines instead of being pointed at.
fn external_invoke_slot(
    ir: &IrFile,
    edge: &crate::ir::IrFunctionOverride,
    function: &IrFunction,
) -> Option<u32> {
    let _ = ir;
    if edge.name != "invoke" {
        return None;
    }
    if !super::intrinsics::is_function_type_name(edge.overridden_owner) {
        return None;
    }
    let reference = |ty: Ty| c_kind(ty) == CKind::Ref;
    (function.params.iter().copied().all(reference) && reference(function.ret)).then_some(3)
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
                // `invoke` is the one dependency member this target already gives a FIXED slot:
                // a function value's body sits right after `kotlin.Any`'s three, and the runtime
                // names that number itself (`KT_SLOT_INVOKE`). A class that implements a function
                // type puts its `invoke` there for the same reason a lambda does — every caller
                // through the function type reads that slot.
                if any_slot(function).is_none()
                    && external_invoke_slot(ir, edge, function).is_none()
                {
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

/// Whether an override is callable through the overridden slot's signature as it stands.
///
/// Kotlin permits a covariant return (`A` → `B`, both references), which changes nothing here, and
/// generic specialization (`T` → `Int`), which changes the machine representation. The second needs
/// a [`Slot::Bridge`] in the base's slot; this is the question that says which.
fn same_representation(implementation: &IrFunction, overridden: &IrFunction) -> bool {
    implementation.params.len() == overridden.params.len()
        && implementation
            .params
            .iter()
            .zip(&overridden.params)
            .all(|(a, b)| c_kind(*a) == c_kind(*b))
        && c_kind(implementation.ret) == c_kind(overridden.ret)
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
        // A data class, an interface, a value class, an `inner` class, an ANNOTATION class and now
        // a VARARG constructor parameter are deliberately absent from this list: each lowers like
        // the thing it is. A vararg parameter is PHYSICALLY an array, and the IR records it as one
        // — so a constructor taking it takes a reference, like any other array parameter, and
        // there was nothing for the refusal to protect.
        ir.classes[id as usize]
            .ctor_args
            .push(crate::ir::IrCtorArg {
                name: Some("rest".to_string()),
                ty: Ty::obj_args("kotlin/Array", &[Ty::Int]),
                declared_ty: None,
                is_field: false,
                field_index: None,
                has_default: false,
                is_vararg: true,
                type_param: None,
                check: None,
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
}
