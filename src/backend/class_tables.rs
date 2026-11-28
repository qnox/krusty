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
//! `kotlin.Any`'s three slots — `equals`, `hashCode`, `toString`, in that order. A function value's
//! `invoke` takes no slot number: a class implementing a function type holds an ordinary
//! [`Slot::FunctionBridge`] slot for each, listed in [`ClassTable::function_invokes`], and the
//! target dispatches a call through a function type by the call's physical operand count rather
//! than by a table number, so no module-wide count of function slots exists. Interface members are numbered
//! once for the whole MODULE ([`module_interfaces`]) and held in a separate interface table per
//! class ([`planned_itable`]), so a call through an interface of another file needs only the
//! number: the layout [`build_numbered`] answers.
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
//! **Superclasses of other files.** A class may extend one that another file of the module
//! already laid out ([`LaidOutSuperclass`]), when the module plan numbers both: the plan gives the
//! numbers, and each one this file has no member for is [`Slot::Inherited`], which the target fills
//! from the table that file built.
//!
//! **Refusals.** A superclass declared in another file that laid out no table for it, and an
//! override of a method declared outside the file that the module plan does not number, decline,
//! so the file declines with a diagnostic instead of emitting something unverified.

mod class_vtable;
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures;
mod hierarchy;
mod members;
pub mod module_classes;
pub mod module_interfaces;
pub mod module_slots;
pub mod planned_itable;
mod planned_vtable;
#[cfg(test)]
mod tests;

use crate::fx_hash::HashMap;

use crate::fir::ResolvedPropertyOverrideTarget;
use crate::ir::{ClassId, FunId, IrFile};
use crate::types::{Ty, TypeName};

pub use members::{function_key, local_property_target};

/// The construct a table declined, phrased for a diagnostic.
pub(crate) type Unsupported = String;

/// What a target answers about how it carries values, which is all the tables need to know of it.
pub trait Representation {
    /// Whether a value of type `a` and one of type `b` are carried alike, so a member declared
    /// with one can be implemented with the other and called through the same slot.
    fn same(&self, a: Ty, b: Ty) -> bool;
    /// Whether a class may extend `superclass` although no class of the file declares it: a base
    /// whose storage and `kotlin.Any` members the target's runtime owns.
    fn owns_base(&self, superclass: TypeName) -> bool;
}

/// Which of `kotlin.Any`'s three members a slot answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnyMember {
    Equals,
    HashCode,
    ToString,
}

impl AnyMember {
    /// The three, in slot order.
    pub const ALL: [Self; ANY_SLOTS as usize] = [Self::Equals, Self::HashCode, Self::ToString];

    /// The slot every table holds this member in.
    pub fn slot(self) -> u32 {
        match self {
            Self::Equals => 0,
            Self::HashCode => 1,
            Self::ToString => 2,
        }
    }
}

/// The operands an accessor takes between its dispatch receiver and its value, as the declaration
/// and the implementation of a property override each have them: the context parameters, then a
/// member extension's receiver.
pub(crate) fn accessor_operands(
    declared_contexts: &[Ty],
    implementation_contexts: &[Ty],
    receiver: Option<(Ty, Ty)>,
) -> Vec<(Ty, Ty)> {
    assert_eq!(
        declared_contexts.len(),
        implementation_contexts.len(),
        "an override edge carries the same context parameters on both sides"
    );
    declared_contexts
        .iter()
        .copied()
        .zip(implementation_contexts.iter().copied())
        .chain(receiver)
        .collect()
}

/// What a table slot holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
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
    /// The same stand-in as [`Slot::Bridge`] for a base method no file of the module declares, a
    /// dependency class's (`Base<T>.value(input: T)` overridden by `value(input: Int)` in a
    /// `Derived : Base<Int>`): there is no `FunId` to read the base's signature off, so the entry
    /// carries the carriers the base declares itself. It forwards by DISPATCH for the same reason.
    DeclaredBridge {
        /// The operands, not counting the receiver, and the result as the BASE declares them.
        parameters: Box<[Ty]>,
        result: Ty,
        target_slot: u32,
        target: FunId,
    },
    /// The stand-in for a function type's `invoke`: `invoke` overriding `kotlin.Function{N}.invoke`,
    /// which every function value answers through one signature whatever the override carries.
    ///
    /// A caller through a function type passes and reads references, because that is the one
    /// signature every function value shares. `class A : (Int) -> Int { override fun invoke(p:
    /// Int) = p + 1 }` carries machine integers instead, so its body cannot be what a caller
    /// reaches. This is, and converts, forwarding to the method's own slot. It is recorded in
    /// [`ClassTable::function_invokes`] under the physical operand count.
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
        /// The operands between the dispatch receiver and the value, each as the interface
        /// declares it and as the implementation takes it: the context parameters, then a member
        /// extension's receiver. `GFoo<T>.(T.extVal)` takes a box where `GFoo<S>`'s
        /// implementation takes the value of `S`.
        operands: Vec<(Ty, Ty)>,
        /// Whether this stands in for the setter — which takes the value and answers nothing —
        /// rather than the getter.
        setter: bool,
        target_slot: u32,
    },
    /// What the class's nearest superclass laid out by another file of the module holds at
    /// `slot` ([`ClassTable::laid_out_superclass`]): a member this file neither declares nor
    /// overrides, so it has no function to name. The target reads it off the table that file built.
    /// It names its number because an interface table copies the entry to another index.
    Inherited { slot: u32 },
}

/// The identity of a virtual member, independent of which class's implementation fills it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SlotKey {
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

/// A superclass another file of the module laid out, as a file extending it sees it: what its table
/// holds, by number, and never its IR.
#[derive(Clone, Debug, Default)]
pub struct LaidOutSuperclass {
    /// How many numbers its class table has.
    pub slots: u32,
    /// The module interfaces its instances implement.
    pub interfaces: Vec<TypeName>,
    /// Each interface member number its interface table fills with a member of the class rather
    /// than an interface's default, with the class slot that member stands at, or `None` for a
    /// bridge. A default is not listed: the class extending it finds the same one.
    pub interface_entries: HashMap<u32, Option<u32>>,
    /// The slot of each member its class table dispatches, inherited ones too, by the member's
    /// module-wide identity: where a local class of another file, which no module plan numbers,
    /// finds the member it overrides.
    pub members: HashMap<module_slots::PlanKey, u32>,
    /// The function types its instances implement, by the slot of each one's bridge.
    pub function_invokes: Vec<FunctionInvoke>,
}

/// One class's place in the hierarchy and its dispatch table.
#[derive(Clone, Debug)]
pub struct ClassTable {
    /// The superclass in this file, or `None` for a class extending `kotlin.Any`, a base the
    /// target owns, or a class another file laid out.
    pub superclass: Option<ClassId>,
    /// The nearest superclass outside this file that another file of the module laid out: this
    /// class's own superclass, or the one its superclass in this file inherits from.
    pub laid_out_superclass: Option<TypeName>,
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
    /// Methods of a dependency class that dispatch never reaches, such as its constructor's body
    /// or a final member: the plan numbers only what a subclass can override, so a call of one of
    /// these runs the method itself.
    pub direct_methods: crate::fx_hash::HashSet<FunId>,
    /// The function types this class implements, inherited ones included, each with the slot of
    /// its [`Slot::FunctionBridge`].
    pub function_invokes: Vec<FunctionInvoke>,
}

/// One function type a class implements: the slot of the bridge that answers a call through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FunctionInvoke {
    /// How many physical operands the `invoke` takes, a `suspend` one's continuation included:
    /// what a call through the function type is keyed by.
    pub operands: usize,
    /// The slot of the [`Slot::FunctionBridge`] answering a call with that many operands.
    pub slot: u32,
}

/// How many slots `kotlin.Any` occupies at the front of every table.
pub const ANY_SLOTS: u32 = 3;

/// `kotlin.Any`'s table, the prefix of every class's.
fn any_vtable() -> (Vec<Slot>, HashMap<SlotKey, u32>) {
    let vtable = AnyMember::ALL.map(Slot::AnyMember).to_vec();
    let slots = AnyMember::ALL
        .map(|member| (SlotKey::Any(member.slot()), member.slot()))
        .into_iter()
        .collect();
    (vtable, slots)
}

/// The class tables of every class in `ir`, with the interface members' module-wide numbers in a
/// table of each class's own ([`planned_itable`]) rather than in its class table. An interface's
/// class table keeps only `kotlin.Any`'s slots.
///
/// `laid_out` answers, for a superclass no class of `ir` declares, the table another file of the
/// module built for it, if one did.
pub fn build_numbered(
    representation: &dyn Representation,
    ir: &IrFile,
    plan: &module_slots::ModuleSlotPlan,
    extended: &mut module_interfaces::ExtendedNumbers,
    laid_out: &dyn Fn(TypeName) -> Option<LaidOutSuperclass>,
) -> Result<NumberedClassTables, Unsupported> {
    let superclasses = laid_out_superclasses(ir, laid_out);
    let (mut tables, order, interfaces) = class_tables(representation, ir, plan, &superclasses)?;
    let numbers = plan.interfaces().with_installed(ir, extended)?;
    let interface_tables = planned_itable::interface_tables(
        representation,
        ir,
        &numbers,
        &order,
        &tables,
        &superclasses,
    )?;
    for (id, class) in ir.classes.iter().enumerate() {
        if !class.is_interface {
            continue;
        }
        let table = &mut tables[id];
        table.vtable.truncate(ANY_SLOTS as usize);
        table.slots.retain(|_, slot| *slot < ANY_SLOTS);
        table.function_invokes.clear();
        table.interface_bridges.clear();
    }
    let members = tables
        .iter()
        .map(|table| member_identities(ir, table, &superclasses))
        .collect();
    Ok(NumberedClassTables {
        tables,
        order,
        interfaces,
        interface_tables,
        members,
        numbers,
    })
}

/// The slot of each member `table` dispatches, by module-wide identity: its own members and those
/// of the superclass another file laid out, which this file holds no function for.
fn member_identities(
    ir: &IrFile,
    table: &ClassTable,
    superclasses: &HashMap<TypeName, LaidOutSuperclass>,
) -> HashMap<module_slots::PlanKey, u32> {
    use module_slots::PlanKey;
    let mut members = table
        .laid_out_superclass
        .map(|name| superclasses[&name].members.clone())
        .unwrap_or_default();
    let callables: HashMap<FunId, crate::fir::CallableId> = ir
        .checked_callable_functions
        .iter()
        .map(|(&callable, &function)| (function, callable))
        .collect();
    let dependencies: HashMap<FunId, crate::fir::ExternalCallableId> = ir
        .dependency_methods
        .iter()
        .map(|(&member, &function)| (function, member))
        .collect();
    for (key, &slot) in &table.slots {
        let identity = match *key {
            SlotKey::Any(_) => continue,
            SlotKey::Function(function) => match callables.get(&function) {
                Some(&callable) => PlanKey::Callable(callable),
                // A support unit emits a dependency member, accessors included, as a method.
                None => match dependencies.get(&function) {
                    Some(&member) => PlanKey::DependencyCallable(member),
                    // A method no other file can name, such as a private one.
                    None => continue,
                },
            },
            SlotKey::Getter(property) => PlanKey::accessor(property, false),
            SlotKey::Setter(property) => PlanKey::accessor(property, true),
        };
        members.insert(identity, slot);
    }
    members
}

/// [`build_numbered`]'s answer.
#[derive(Debug)]
pub struct NumberedClassTables {
    pub tables: Vec<ClassTable>,
    pub order: Vec<ClassId>,
    /// Every interface of this file each class implements, transitively. Indexed by `ClassId`.
    pub interfaces: Vec<Vec<ClassId>>,
    pub interface_tables: planned_itable::InterfaceTables,
    /// Each class's [`LaidOutSuperclass::members`]. Indexed by `ClassId`.
    pub members: Vec<HashMap<module_slots::PlanKey, u32>>,
    /// The module's interface numbers, extended with the interfaces this file's IR installed.
    pub numbers: module_interfaces::ModuleInterfaces,
}

impl NumberedClassTables {
    pub fn table(&self, class: ClassId) -> &ClassTable {
        &self.tables[class as usize]
    }

    /// The class slot a member dispatches through, looked up on the class the call names.
    pub fn slot(&self, class: ClassId, key: &SlotKey) -> Option<u32> {
        self.table(class).slots.get(key).copied()
    }
}

type FileTables = (Vec<ClassTable>, Vec<ClassId>, Vec<Vec<ClassId>>);

/// The class a table is built for, and what it extends.
struct TabledClass<'t> {
    id: ClassId,
    /// Its superclass in this file, already tabled as `parent`.
    superclass: Option<ClassId>,
    parent: Option<&'t ClassTable>,
    /// The nearest superclass another file laid out, which `superclass` (if any) extends in turn.
    laid_out: Option<(TypeName, &'t LaidOutSuperclass)>,
}

/// Whether class `id` is declared in executable code: a local or anonymous class, or the copy of an
/// anonymous object an inline expansion specialized for its call. No other file names or extends
/// one, and every file that expands an inline function declaring one realizes it again under the
/// same name, so its descriptor and members stay inside the object.
pub fn is_declared_in_executable_code(ir: &IrFile, id: ClassId) -> bool {
    let class = &ir.classes[id as usize];
    class.is_local_class
        || class.is_anonymous_object
        || ir.local_class_name_provenance.contains_key(&id)
        || ir.specialized_anonymous_classes.contains_key(&id)
}

/// The superclasses of `ir`'s classes that no class of `ir` declares and another file laid out.
fn laid_out_superclasses(
    ir: &IrFile,
    laid_out: &dyn Fn(TypeName) -> Option<LaidOutSuperclass>,
) -> HashMap<TypeName, LaidOutSuperclass> {
    ir.classes
        .iter()
        .map(|class| class.superclass)
        .filter(|&superclass| ir.class_id_by_name(superclass).is_none())
        .filter_map(|superclass| laid_out(superclass).map(|table| (superclass, table)))
        .collect()
}

fn class_tables(
    representation: &dyn Representation,
    ir: &IrFile,
    plan: &module_slots::ModuleSlotPlan,
    superclasses: &HashMap<TypeName, LaidOutSuperclass>,
) -> Result<FileTables, Unsupported> {
    let order = hierarchy::hierarchy_order(representation, ir, superclasses)?;
    // Before the tables because the program-wide interface region uses the complete transitive
    // set after each class has consumed its exact override edges.
    let interfaces = hierarchy::interface_closure(ir);
    let mut tables: Vec<Option<ClassTable>> = vec![None; ir.classes.len()];
    for &id in &order {
        let class = &ir.classes[id as usize];
        let superclass = ir.class_id_by_name(class.superclass);
        let table = {
            let parent = superclass.map(|parent| {
                tables[parent as usize]
                    .as_ref()
                    .expect("superclasses are tabled first")
            });
            let laid_out = match parent {
                Some(parent) => parent
                    .laid_out_superclass
                    .map(|name| (name, &superclasses[&name])),
                None => superclasses
                    .get_key_value(&class.superclass)
                    .map(|(name, table)| (*name, table)),
            };
            match plan.table(class.fq_name) {
                Some(Ok(planned)) => planned_vtable::planned_class_table(
                    representation,
                    ir,
                    TabledClass {
                        id,
                        superclass,
                        parent,
                        laid_out,
                    },
                    planned,
                    plan.interfaces(),
                )?,
                Some(Err(reason)) if laid_out.is_some() && !is_declared_in_executable_code(ir, id) => {
                    return Err(format!(
                        "a class extending one of another file with no module-wide layout (`{}`): {reason}",
                        class.fq_name()
                    ))
                }
                // A class no module plan numbers (a local class no other file names, or a
                // dependency class only a support unit materializes), so no other file numbers its
                // members: it extends the superclass's table by the identities of the members it
                // overrides.
                _ => class_vtable::class_table(
                    representation,
                    ir,
                    TabledClass {
                        id,
                        superclass,
                        parent,
                        laid_out,
                    },
                    plan.interfaces(),
                )?,
            }
        };
        tables[id as usize] = Some(table);
    }
    let tables: Vec<ClassTable> = tables
        .into_iter()
        .map(|table| table.expect("every class was tabled"))
        .collect();
    Ok((tables, order, interfaces))
}
