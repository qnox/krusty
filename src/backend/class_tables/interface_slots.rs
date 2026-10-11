//! The program-wide numbering of interface members, above every class's own slots.

use std::collections::HashMap;

use crate::fir::ResolvedPropertyOverrideTarget;
use crate::ir::{ClassId, IrFile};

use super::members::property_name;
use super::{any_vtable, ClassTable, InterfaceRegion, Slot, SlotKey, Unsupported, ANY_SLOTS};

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
pub(super) fn place_interface_slots(
    ir: &IrFile,
    order: &[ClassId],
    layouts: &mut [ClassTable],
    interfaces: &[Vec<ClassId>],
) -> Result<InterfaceRegion, Unsupported> {
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
                // An abstract redeclaration's bridge forwards to no body: like the entry it stands
                // for, it supplies nothing, and an implementor fills the number with its own.
                let supplied = match layout.interface_bridges.get(key) {
                    Some(Slot::Bridge {
                        target,
                        target_slot: None,
                        ..
                    }) if ir.functions[*target as usize].body.is_none() => Slot::Abstract,
                    Some(bridge) => bridge.clone(),
                    None => entry.clone(),
                };
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
            // An ENUM whose every entry has a BODY is not instantiable as itself: each instance
            // is an entry subclass, and the slot is filled there — the check below runs for each
            // of those too, so a genuinely missing implementation is still caught. Kotlin marks
            // such a class abstract for the same reason; common IR does not, so the shape is read
            // here. An entry WITHOUT a body is an instance of the enum class, and then the trap is
            // reachable and this does not apply.
            let entries_carry_it = ir.classes[id as usize].superclass
                == crate::types::wk::kotlin_enum()
                && !ir.classes[id as usize].enum_entries.is_empty()
                && ir.classes[id as usize]
                    .enum_entries
                    .iter()
                    .all(|entry| entry.subclass.is_some());
            if implemented
                && matches!(entry, Slot::Abstract)
                && !ir.classes[id as usize].is_abstract
                && !ir.classes[id as usize].is_interface
                && !entries_carry_it
            {
                crate::trace_compiler!(
                    "class_tables",
                    "interface member unfilled class={} member={} key={:?} aliases={:?} slots={:?} bridges={:?}",
                    ir.classes[id as usize].fq_name(),
                    member_name(ir, member),
                    member.key,
                    member.aliases,
                    layouts[id as usize].slots.keys().collect::<Vec<_>>(),
                    layouts[id as usize].interface_bridges.keys().collect::<Vec<_>>(),
                );
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
    Ok(InterfaceRegion {
        base,
        members: members.len() as u32,
    })
}

/// How a diagnostic names an interface member.
fn member_name(ir: &IrFile, member: &InterfaceMember) -> String {
    let interface = ir.classes[member.interface as usize].fq_name();
    let name = match &member.key {
        SlotKey::Function(fid) => ir.functions[*fid as usize].name.clone(),
        SlotKey::Getter(target) => format!("get {}", property_name(ir, *target)),
        SlotKey::Setter(target) => format!("set {}", property_name(ir, *target)),
        SlotKey::Any(slot) => format!("kotlin.Any slot {slot}"),
    };
    format!("{interface}.{name}")
}

/// A total order over slot keys, so an interface's members are numbered the same way on every
/// run — the map they come from has no order of its own.
fn key_order(key: &SlotKey) -> (u8, u32, u32) {
    match key {
        SlotKey::Any(slot) => (0, *slot, 0),
        SlotKey::Function(fid) => (1, *fid, 0),
        SlotKey::Getter(target) => property_key_order(2, *target),
        SlotKey::Setter(target) => property_key_order(3, *target),
    }
}

fn property_key_order(kind: u8, target: ResolvedPropertyOverrideTarget) -> (u8, u32, u32) {
    match target {
        ResolvedPropertyOverrideTarget::Module(property) => (kind, 0, property.raw()),
        ResolvedPropertyOverrideTarget::External(property) => (kind, 1, property.raw()),
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
