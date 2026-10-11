//! The field order and virtual-dispatch tables of the classes of one IR file, for every target that
//! lays classes out itself.
//!
//! Everything here is a pure function over an [`IrFile`]; nothing emits code and nothing knows a
//! byte offset or a machine type. A target asks two questions of the result — "which fields come
//! before this class's own" and "which table slot is this member" — and this module answers both
//! from the IR's own hierarchy and override tables (`IrFile::classifier_hierarchies`,
//! `function_overrides`, `property_overrides`) rather than by matching names. What a slot or a
//! field physically IS — a byte offset and a function pointer on Native, a struct field and a typed
//! function reference on WasmGC — is the target's, built on top of these answers.
//!
//! **Fields** follow the classic single-inheritance order: the superclass's fields as a prefix, in
//! the superclass's order, then this class's own in declaration order ([`ClassTable::first_field`]).
//!
//! **Tables** are the classic single-inheritance ones too: a class's table is its superclass's
//! with overridden slots replaced and newly declared members appended. Slot numbers are assigned
//! at the declaring class and stay valid down the hierarchy, which is what lets a call through an
//! `A`-typed value reach `B`'s override with one indexed read. Every table begins with
//! `kotlin.Any`'s three slots — `equals`, `hashCode`, `toString`, in that order — and reserves the
//! next one for a function value's `invoke` ([`FUNCTION_SLOT`]). Interface members are numbered
//! once for the whole file, above every class's own slots ([`InterfaceRegion`]).
//!
//! Property accessors are members too. A property that is `open` (or overrides one) dispatches
//! through a getter (and, for a `var`, a setter) slot; when the source declares no accessor body,
//! the slot is a synthesized field access, so `x.v` through an `A`-typed `x` reads `B`'s `v` when
//! `B` overrides it. A property that is neither open nor an override is a direct field access.
//!
//! **Representation** is the one question the tables ask a target ([`Representation`]): whether an
//! override is carried the way the member it overrides is. Where it is not, the overridden slot
//! keeps its signature and holds a bridge that converts and forwards ([`Slot::Bridge`]).
//!
//! **Refusals.** A superclass declared in another file and an override of a method declared
//! outside it decline, so the file declines with a diagnostic instead of emitting something
//! unverified.

mod class_vtable;
#[cfg(test)]
pub(crate) mod fixtures;
mod hierarchy;
mod interface_slots;
mod members;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use crate::fir::ResolvedPropertyOverrideTarget;
use crate::ir::{ClassId, FunId, IrFile};
use crate::types::{Ty, TypeName};

pub(crate) use members::{function_key, local_property_target};

/// The construct a table declined, phrased for a diagnostic.
pub(crate) type Unsupported = String;

/// What a target answers about how it carries values, which is all the tables need to know of it.
pub(crate) trait Representation {
    /// Whether a value of type `a` and one of type `b` are carried alike, so a member declared
    /// with one can be implemented with the other and called through the same slot.
    fn same(&self, a: Ty, b: Ty) -> bool;
    /// Whether `ty` is carried as a reference: what every operand of a function value's `invoke`
    /// is, so an `invoke` override carrying only references can stand in [`FUNCTION_SLOT`].
    fn is_reference(&self, ty: Ty) -> bool;
    /// Whether a class may extend `superclass` although no class of the file declares it: a base
    /// whose storage and `kotlin.Any` members the target's runtime owns.
    fn owns_base(&self, superclass: TypeName) -> bool;
}

/// Which of `kotlin.Any`'s three members a slot answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AnyMember {
    Equals,
    HashCode,
    ToString,
}

impl AnyMember {
    /// The three, in slot order.
    pub(crate) const ALL: [Self; ANY_SLOTS as usize] =
        [Self::Equals, Self::HashCode, Self::ToString];

    /// The slot every table holds this member in.
    pub(crate) fn slot(self) -> u32 {
        match self {
            Self::Equals => 0,
            Self::HashCode => 1,
            Self::ToString => 2,
        }
    }
}

/// What a table slot holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Slot {
    /// `kotlin.Any`'s own member, which no class of the hierarchy overrides. The target decides
    /// what realizes it: the base the root class extends may own a rendering of its own.
    AnyMember(AnyMember),
    /// A method of the file.
    Function(FunId),
    /// An abstract member: a loud failure if reached, never a jump through nothing.
    Abstract,
    /// A synthesized getter for a property with a backing field and no source getter.
    FieldGetter { class: ClassId, field: u32 },
    /// A synthesized setter, likewise.
    FieldSetter { class: ClassId, field: u32 },
    /// An override reachable through a base whose signature has a different REPRESENTATION:
    /// `A<T : Number>.foo(): T` erases its result to a reference, and `Z : A<Int>` overriding it
    /// returns an unboxed machine integer. The base's slot cannot hold that body — a caller reading
    /// the slot through `A` would read an integer as a reference — so it holds this instead: a small
    /// function with the BASE's signature that converts each operand and forwards.
    ///
    /// It forwards by DISPATCH rather than by calling the override directly, and that is the whole
    /// reason it is a slot number here and not a function id. A further subclass replaces `target`
    /// with its own body, and a bridge that had named the override would keep running the wrong
    /// one; re-dispatching through the receiver's own table reaches whatever that receiver
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
    /// [`FUNCTION_SLOT`]'s own stand-in: `invoke` overriding `kotlin.Function{N}.invoke` where
    /// the override does not carry references throughout.
    ///
    /// A caller through a function type reads that fixed number and passes and reads references
    /// there, because that is the one signature every function value shares. `class A : (Int) ->
    /// Int { override fun invoke(p: Int) = p + 1 }` carries machine integers instead, so its body
    /// cannot stand in that number. This stands there and converts, forwarding to the method's
    /// own slot.
    ///
    /// It carries an ARITY rather than a declaration id, which is what separates it from
    /// [`Slot::Bridge`]: the signature it wears belongs to `kotlin.Function{N}`, which is declared
    /// in no file this target compiles, so there is no `FunId` to read it off. Every operand and
    /// the result are references, and the arity is the whole of the rest.
    ///
    /// It forwards by DISPATCH for the same reason [`Slot::Bridge`] does.
    FunctionBridge {
        /// How many operands `invoke` takes, not counting the receiver.
        arity: usize,
        /// The slot the real `invoke` occupies, and the method whose carriers it expects.
        target_slot: u32,
        target: FunId,
    },
    /// The same thing as [`Slot::Bridge`] for a property ACCESSOR reached through a base that
    /// declares it with a different representation: `interface C<T> { var size: T }` erases its
    /// accessors to a reference while `class B : C<Int>, A()` inherits an unboxed machine integer.
    ///
    /// It carries TYPES where [`Slot::Bridge`] carries function ids, because neither end need be
    /// a source accessor: either may be a synthesized field access, which has no declaration to
    /// name. It forwards by DISPATCH for the same reason the method bridge does.
    AccessorBridge {
        /// The property type as the BASE declares it: the carrier this entry wears.
        declared: Ty,
        /// The property type as the implementation has it: the carrier `target_slot` expects.
        implemented: Ty,
        /// A member extension's receiver, as the interface declares it and as the implementation
        /// takes it: `GFoo<T>.(T.extVal)` takes a box where `GFoo<S>`'s implementation takes the
        /// value of `S`.
        receiver: Option<(Ty, Ty)>,
        /// Whether this stands in for the setter — which takes the value and answers nothing —
        /// rather than the getter.
        setter: bool,
        target_slot: u32,
    },
}

/// The identity of a virtual member, independent of which class's implementation fills it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SlotKey {
    /// `kotlin.Any`'s slots, by number.
    Any(u32),
    /// A method, by its implementation in some class; an override registers under its own id too,
    /// so a further override finds the slot through either.
    Function(FunId),
    /// A property accessor, by the frontend's stable declaration identity. The accessor role is
    /// part of the key because a mutable property owns two independently dispatched members.
    Getter(ResolvedPropertyOverrideTarget),
    Setter(ResolvedPropertyOverrideTarget),
}

/// One class's place in the hierarchy and its dispatch table.
#[derive(Clone, Debug)]
pub(crate) struct ClassTable {
    /// The superclass in this file, or `None` for a class extending `kotlin.Any` or a base the
    /// target owns.
    pub superclass: Option<ClassId>,
    /// How many fields the superclasses in this file declare between them: this class's own
    /// field `i` is field `first_field + i` of the whole object.
    pub first_field: u32,
    pub vtable: Vec<Slot>,
    pub slots: HashMap<SlotKey, u32>,
    /// Interface members this class implements through a declaration of a DIFFERENT
    /// representation, by the interface's own spelling of the member. The interface's
    /// program-wide number is filled with the bridge recorded here instead of an alias of this
    /// class's slot.
    pub interface_bridges: HashMap<SlotKey, Slot>,
}

/// Where the interface members' shared numbering sits in every class's table: from `base`, one
/// slot per member. Every class table is exactly `base + members` long once it is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InterfaceRegion {
    pub base: u32,
    pub members: u32,
}

/// The tables of every class in a file, indexed by `ClassId`, plus the order in which a
/// superclass precedes its subclasses.
#[derive(Debug)]
pub(crate) struct ClassTables {
    pub tables: Vec<ClassTable>,
    pub order: Vec<ClassId>,
    /// Every interface each class implements, transitively. Indexed by `ClassId`.
    pub interfaces: Vec<Vec<ClassId>>,
    pub region: InterfaceRegion,
}

impl ClassTables {
    pub(crate) fn table(&self, class: ClassId) -> &ClassTable {
        &self.tables[class as usize]
    }

    /// The slot a member dispatches through, looked up on the class the call names.
    pub(crate) fn slot(&self, class: ClassId, key: &SlotKey) -> Option<u32> {
        self.table(class).slots.get(key).copied()
    }
}

/// How many slots `kotlin.Any` occupies at the front of every table.
pub(crate) const ANY_SLOTS: u32 = 3;

/// The table number every function value's body occupies: right after `kotlin.Any`'s three.
pub(crate) const FUNCTION_SLOT: u32 = 3;

/// `kotlin.Any`'s table, the prefix of every class's.
fn any_vtable() -> (Vec<Slot>, HashMap<SlotKey, u32>) {
    let vtable = AnyMember::ALL.map(Slot::AnyMember).to_vec();
    let slots = AnyMember::ALL
        .map(|member| (SlotKey::Any(member.slot()), member.slot()))
        .into_iter()
        .collect();
    (vtable, slots)
}

/// Build the table of every class in `ir`, superclasses first.
pub(crate) fn build(
    representation: &dyn Representation,
    ir: &IrFile,
) -> Result<ClassTables, Unsupported> {
    let order = hierarchy::hierarchy_order(representation, ir)?;
    // Before the tables because the program-wide interface region uses the complete transitive
    // set after each class has consumed its exact override edges.
    let interfaces = hierarchy::interface_closure(ir);
    let mut tables: Vec<Option<ClassTable>> = vec![None; ir.classes.len()];
    for &id in &order {
        let superclass = ir.class_id_by_name(ir.classes[id as usize].superclass);
        let table = {
            let parent = superclass.map(|parent| {
                tables[parent as usize]
                    .as_ref()
                    .expect("superclasses are tabled first")
            });
            class_vtable::class_table(representation, ir, id, superclass, parent)?
        };
        tables[id as usize] = Some(table);
    }
    let mut tables: Vec<ClassTable> = tables
        .into_iter()
        .map(|table| table.expect("every class was tabled"))
        .collect();
    let region = interface_slots::place_interface_slots(ir, &order, &mut tables, &interfaces)?;
    Ok(ClassTables {
        tables,
        order,
        interfaces,
        region,
    })
}
