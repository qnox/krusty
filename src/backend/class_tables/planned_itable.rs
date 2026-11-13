//! Each class's interface table: the module-wide interface member numbers, filled from the
//! declaring file's IR.
//!
//! [`super::module_interfaces`] numbers every interface member of the module from the headers, so
//! a caller in any file reads the same number. This file has the class's IR and its class table,
//! so it says what each number the class implements holds: the class slot of the member that
//! implements it (found through the exact override edges common lowering recorded, never by name),
//! a bridge where that member carries values differently, or the body the interface supplies. A
//! table is indexed by the number directly and ends after the highest number the class implements.

use crate::fx_hash::{HashMap, HashSet};

use crate::fir::{CallableId, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
use crate::ir::{ClassId, FunId, IrFile, IrLocalPropertyLayout};
use crate::types::{Ty, TypeName};

use super::class_vtable::same_representation;
use super::members::function_key;
use super::module_interfaces::{MemberDeclaration, ModuleInterfaces};
use super::module_slots::PlanKey;
use super::{ClassTable, LaidOutSuperclass, Representation, Slot, SlotKey, Unsupported};

/// What one interface table entry holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InterfaceEntry {
    /// An entry of this file: a class slot's contents, a bridge, or an interface's own body.
    Slot(Slot),
    /// The body an interface another file of the module declares, which that file defines under
    /// the member's module symbol.
    ModuleFunction(CallableId),
    /// What the interface table of the class's superclass laid out by another file holds at this
    /// number, where that is no class slot's member: an interface default, or a bridge.
    Inherited,
}

/// The interface tables of every class of a file, indexed by `ClassId`.
#[derive(Debug)]
pub struct InterfaceTables {
    /// Indexed by member number; an interface's own is what an implementor supplying nothing
    /// would hold, which is where an object a lambda becomes starts.
    pub tables: Vec<Vec<InterfaceEntry>>,
    /// The module interfaces each class implements (an interface: extends, itself first), in a
    /// deterministic order.
    pub implemented: Vec<Vec<TypeName>>,
    /// Each number a class's table fills with a member of the class rather than an interface's
    /// default, with the class slot the member stands at, or `None` for a bridge: what a class of
    /// another file extending it reads ([`super::LaidOutSuperclass::interface_entries`]).
    pub members: Vec<HashMap<u32, Option<u32>>>,
}

/// How a class implements one numbered spelling.
#[derive(Clone)]
enum Implementation {
    /// The class's own table holds the member at this slot.
    Slot(u32),
    /// A bridge wearing the interface's carriers stands for it.
    Bridged(Slot),
}

/// Fill the interface table of every class of `ir`, whose class tables `tables` already holds.
/// `laid_out` holds every superclass another file laid out that a class of `ir` extends.
pub(super) fn interface_tables(
    representation: &dyn Representation,
    ir: &IrFile,
    numbers: &ModuleInterfaces,
    order: &[ClassId],
    tables: &[ClassTable],
    laid_out: &HashMap<TypeName, LaidOutSuperclass>,
) -> Result<InterfaceTables, Unsupported> {
    let mut implemented: Vec<Vec<TypeName>> = vec![Vec::new(); ir.classes.len()];
    for &id in order {
        let class = &ir.classes[id as usize];
        let mut set = match tables[id as usize].superclass {
            Some(parent) => implemented[parent as usize].clone(),
            None => laid_out
                .get(&class.superclass)
                .map(|superclass| superclass.interfaces.clone())
                .unwrap_or_default(),
        };
        let direct = class
            .is_interface
            .then_some(class.fq_name)
            .into_iter()
            .chain(class.interfaces.iter())
            .chain(
                class
                    .supertypes
                    .iter()
                    .copied()
                    .filter_map(Ty::obj_internal),
            );
        for name in direct {
            match numbers.interface(name) {
                Some(Ok(numbered)) => {
                    for &interface in &numbered.closure {
                        if !set.contains(&interface) {
                            set.push(interface);
                        }
                    }
                }
                Some(Err(reason)) => {
                    return Err(format!(
                        "an interface with no module-wide numbering (`{}` implements `{}`): \
                         {reason}",
                        class.fq_name(),
                        name.render().replace('/', ".")
                    ))
                }
                None if class.is_interface && name == class.fq_name => {
                    return Err(format!(
                        "an interface with no module-wide numbering (`{}`)",
                        class.fq_name()
                    ))
                }
                None => {}
            }
        }
        implemented[id as usize] = set;
    }
    let mut filled = vec![Vec::new(); ir.classes.len()];
    let mut members = vec![HashMap::default(); ir.classes.len()];
    for &id in order {
        (filled[id as usize], members[id as usize]) = Filler {
            representation,
            ir,
            numbers,
            tables,
            implemented: &implemented,
            laid_out,
        }
        .table(id)?;
    }
    Ok(InterfaceTables {
        tables: filled,
        implemented,
        members,
    })
}

struct Filler<'a> {
    representation: &'a dyn Representation,
    ir: &'a IrFile,
    numbers: &'a ModuleInterfaces,
    tables: &'a [ClassTable],
    implemented: &'a [Vec<TypeName>],
    laid_out: &'a HashMap<TypeName, LaidOutSuperclass>,
}

/// A class's interface table, and the numbers in it that members of the class fill
/// ([`InterfaceTables::members`]).
type FilledTable = (Vec<InterfaceEntry>, HashMap<u32, Option<u32>>);

impl Filler<'_> {
    fn table(&self, id: ClassId) -> Result<FilledTable, Unsupported> {
        let class = &self.ir.classes[id as usize];
        let implemented = &self.implemented[id as usize];
        let numbered = self
            .numbers
            .numbers_of(&implemented.iter().copied().collect::<HashSet<_>>());
        let Some(&last) = numbered.last() else {
            return Ok((Vec::new(), HashMap::default()));
        };
        let (implementations, elsewhere) = if class.is_interface {
            (HashMap::default(), HashSet::default())
        } else {
            self.implementations(id)?
        };
        let laid_out = self.tables[id as usize]
            .laid_out_superclass
            .map(|name| &self.laid_out[&name]);
        // An object of this class can be made, so every number it implements has to reach a body.
        // A class whose every enum entry has a body is not instantiable as itself: each entry is a
        // subclass with a table of its own.
        let entries_carry_it = class.superclass == crate::types::wk::kotlin_enum()
            && !class.enum_entries.is_empty()
            && class
                .enum_entries
                .iter()
                .all(|entry| entry.subclass.is_some());
        let instantiable = !class.is_interface && !class.is_abstract && !entries_carry_it;
        let vtable = &self.tables[id as usize].vtable;
        let mut table = vec![InterfaceEntry::Slot(Slot::Abstract); last as usize + 1];
        let mut members = HashMap::default();
        for number in numbered {
            let member = self.numbers.member(number);
            let mut spellings = std::iter::once(&member.key).chain(&member.aliases);
            let own = spellings
                .clone()
                .find_map(|key| implementations.get(key))
                .cloned();
            // What the superclass another file laid out put at this number: the member at one of
            // its slots, which this class's table holds at the same number, or a body no slot holds.
            let inherited =
                laid_out.and_then(|superclass| superclass.interface_entries.get(&number).copied());
            let entry = match (own, inherited) {
                (Some(Implementation::Slot(slot)), _) | (None, Some(Some(slot))) => {
                    members.insert(number, Some(slot));
                    InterfaceEntry::Slot(vtable[slot as usize].clone())
                }
                (Some(Implementation::Bridged(bridge)), _) => {
                    members.insert(number, None);
                    InterfaceEntry::Slot(bridge)
                }
                (None, Some(None)) => {
                    members.insert(number, None);
                    InterfaceEntry::Inherited
                }
                (None, None) if spellings.any(|key| elsewhere.contains(key)) => {
                    return Err(format!(
                        "an interface member implemented by a member of a superclass of another \
                         file (number {number} in `{}`)",
                        class.fq_name()
                    ))
                }
                (None, None) => self.supplied(id, number, instantiable, class.is_interface)?,
            };
            if instantiable && entry == InterfaceEntry::Slot(Slot::Abstract) {
                return Err(format!(
                    "an interface member with no implementation found (number {number} in `{}`)",
                    class.fq_name()
                ));
            }
            table[number as usize] = entry;
        }
        Ok((table, members))
    }

    /// The body the most derived interface of `id`'s hierarchy declaring `number` supplies, or the
    /// abstract trap. A function body another file declares is named rather than left abstract
    /// where an object can carry this table: one of exactly this class (`instantiable`), which
    /// compiled without implementing the member, so its interface supplies one, or one a lambda
    /// becomes, which starts from its interface's `template`. Any other body of another file is
    /// refused where the class is instantiable and left abstract in a template, which the lambda
    /// conversion refuses in turn.
    fn supplied(
        &self,
        id: ClassId,
        number: u32,
        instantiable: bool,
        template: bool,
    ) -> Result<InterfaceEntry, Unsupported> {
        let implemented = &self.implemented[id as usize];
        let candidates = self
            .numbers
            .member(number)
            .declarations
            .iter()
            .filter(|declaration| implemented.contains(&declaration.interface))
            .collect::<Vec<_>>();
        let extends = |from: TypeName, to: TypeName| {
            from != to
                && self
                    .numbers
                    .interface(from)
                    .and_then(Result::ok)
                    .is_some_and(|numbered| numbered.closure.contains(&to))
        };
        let Some(declaration) = candidates.iter().find(|candidate| {
            !candidates
                .iter()
                .any(|other| extends(other.interface, candidate.interface))
        }) else {
            return Ok(InterfaceEntry::Slot(Slot::Abstract));
        };
        if let PlanKey::Callable(callable) = declaration.member {
            let elsewhere = !self.ir.checked_callable_functions.contains_key(&callable);
            if (instantiable || template) && elsewhere && declaration.bridged_from.is_none() {
                return Ok(InterfaceEntry::ModuleFunction(callable));
            }
        }
        self.declared_body(declaration, instantiable)
            .map(InterfaceEntry::Slot)
    }

    fn declared_body(
        &self,
        declaration: &MemberDeclaration,
        reaches: bool,
    ) -> Result<Slot, Unsupported> {
        let ir = self.ir;
        let interface = || declaration.interface.render().replace('/', ".");
        let method = |fid: FunId| {
            if ir.functions[fid as usize].body.is_some() {
                Slot::Function(fid)
            } else {
                Slot::Abstract
            }
        };
        match declaration.member {
            PlanKey::Callable(callable) => {
                let Some(&fid) = ir.checked_callable_functions.get(&callable) else {
                    if !reaches {
                        return Ok(Slot::Abstract);
                    }
                    return Err(format!(
                        "an interface default reached through a bridge, declared in another file \
                         (`{}`)",
                        interface()
                    ));
                };
                let body = method(fid);
                match declaration.bridged_from {
                    None => Ok(body),
                    Some(_) if body == Slot::Abstract => Ok(Slot::Abstract),
                    Some(PlanKey::Callable(declared)) => {
                        let declared =
                            ir.checked_callable_functions
                                .get(&declared)
                                .ok_or_else(|| {
                                    format!(
                                        "an interface default bridged to a member of another file \
                                     (`{}`)",
                                        interface()
                                    )
                                })?;
                        Ok(Slot::Bridge {
                            declared: *declared,
                            target_slot: None,
                            target: fid,
                        })
                    }
                    Some(_) => unreachable!("a function is bridged from a function"),
                }
            }
            PlanKey::DependencyCallable(_)
            | PlanKey::DependencyGetter(_)
            | PlanKey::DependencySetter(_) => {
                Err("a dependency member numbered as a member of a module interface".to_owned())
            }
            PlanKey::Getter(property) | PlanKey::Setter(property) => {
                let setter = matches!(declaration.member, PlanKey::Setter(_));
                let accessor = match ir.local_property_layouts.get(&property) {
                    Some(IrLocalPropertyLayout::Member {
                        getter,
                        setter: setter_function,
                        ..
                    }) => {
                        if setter {
                            *setter_function
                        } else {
                            *getter
                        }
                    }
                    Some(IrLocalPropertyLayout::MemberExtension {
                        getter,
                        setter: setter_function,
                        ..
                    }) => {
                        if setter {
                            *setter_function
                        } else {
                            Some(*getter)
                        }
                    }
                    _ if !reaches => return Ok(Slot::Abstract),
                    _ => {
                        return Err(format!(
                            "an interface property default declared in another file (`{}`)",
                            interface()
                        ))
                    }
                };
                let body = accessor.map_or(Slot::Abstract, method);
                if declaration.bridged_from.is_some() && body != Slot::Abstract {
                    return Err(format!(
                        "an interface property default reached through a bridge (`{}`)",
                        interface()
                    ));
                }
                Ok(body)
            }
        }
    }

    /// How class `id` implements each numbered spelling: through its own override edges and
    /// those of its superclasses in this file, nearest last so an override replaces what a base
    /// recorded. Beside them, the spellings an edge implements with a member of the superclass
    /// another file laid out, which has no slot this file can name.
    fn implementations(
        &self,
        id: ClassId,
    ) -> Result<(HashMap<PlanKey, Implementation>, HashSet<PlanKey>), Unsupported> {
        let ir = self.ir;
        let table = &self.tables[id as usize];
        let mut elsewhere = HashSet::default();
        let mut chain = vec![id];
        while let Some(parent) = self.tables[*chain.last().expect("non-empty") as usize].superclass
        {
            chain.push(parent);
        }
        let module_function = |target: &ResolvedFunctionOverrideTarget| match target {
            ResolvedFunctionOverrideTarget::Module(callable) => {
                ir.checked_callable_functions.get(callable).copied()
            }
            ResolvedFunctionOverrideTarget::External(_) => None,
        };
        let mut found: HashMap<PlanKey, Implementation> = HashMap::default();
        for &class in chain.iter().rev() {
            let owner = ir.classes[class as usize].fq_name;
            // A member the class declares itself replaces what a base recorded; a member it only
            // inherits (`class B : A(), I` satisfying `I.f` with `A.f`) fills what nothing else
            // did.
            for own in [true, false] {
                for edge in ir
                    .function_overrides
                    .get(&ir.classes[class as usize].fq_name_id())
                    .into_iter()
                    .flatten()
                    .filter(|edge| edge.overridden_is_interface)
                    .filter(|edge| (edge.implementation_owner == owner) == own)
                {
                    let ResolvedFunctionOverrideTarget::Module(overridden) = edge.overridden else {
                        continue;
                    };
                    let key = PlanKey::Callable(overridden);
                    if self.numbers.number(key).is_none() {
                        continue;
                    }
                    // An implementation an interface supplies is a default, which `supplied`
                    // finds; any other has to be a slot of this class, or the entry would be left
                    // to a body that does not implement it.
                    let implementation_owner = ir.class_id_by_name(edge.implementation_owner);
                    if self.numbers.interface(edge.implementation_owner).is_some()
                        || implementation_owner
                            .is_some_and(|owner| ir.classes[owner as usize].is_interface)
                    {
                        continue;
                    }
                    let slot = edge
                        .implementation_function
                        .or_else(|| module_function(&edge.implementation))
                        .zip(implementation_owner)
                        .and_then(|(implementation, owner)| {
                            let key = function_key(ir, owner, implementation);
                            table.slots.get(&key).map(|&slot| (implementation, slot))
                        });
                    if implementation_owner.is_none() && table.laid_out_superclass.is_some() {
                        elsewhere.insert(key);
                        continue;
                    }
                    let Some((implementation, slot)) = slot else {
                        return Err(format!(
                            "an interface member implemented by a declaration with no slot in \
                             this file (`{}` from `{}`)",
                            ir.classes[id as usize].fq_name(),
                            edge.implementation_owner.render().replace('/', ".")
                        ));
                    };
                    let function = &ir.functions[implementation as usize];
                    let entry = match ir.checked_callable_functions.get(&overridden) {
                        Some(&declared) => {
                            if same_representation(
                                self.representation,
                                function,
                                &ir.functions[declared as usize],
                            ) {
                                Implementation::Slot(slot)
                            } else {
                                Implementation::Bridged(Slot::Bridge {
                                    declared,
                                    target_slot: Some(slot),
                                    target: implementation,
                                })
                            }
                        }
                        None => {
                            let (parameters, result) = self
                                .numbers
                                .function_carriers(key)
                                .ok_or("a numbered interface member with no carriers")?;
                            let same = parameters.len() == function.params.len()
                                && parameters
                                    .iter()
                                    .zip(&function.params)
                                    .all(|(a, b)| self.representation.same(*a, *b))
                                && self.representation.same(result, function.ret);
                            if !same {
                                return Err(format!(
                                    "an implementation of an interface member of another file \
                                     that carries values differently (`{}.{}`)",
                                    ir.classes[id as usize].fq_name(),
                                    function.name
                                ));
                            }
                            Implementation::Slot(slot)
                        }
                    };
                    if own {
                        found.insert(key, entry);
                    } else {
                        found.entry(key).or_insert(entry);
                    }
                }
                for edge in ir
                    .property_overrides
                    .get(&ir.classes[class as usize].fq_name_id())
                    .into_iter()
                    .flatten()
                    .filter(|edge| edge.overridden_is_interface)
                    .filter(|edge| (edge.implementation_owner == owner) == own)
                {
                    let ResolvedPropertyOverrideTarget::Module(overridden) = edge.overridden else {
                        continue;
                    };
                    let receiver = edge.declared_receiver.zip(edge.implementation_receiver);
                    let bridged = !self
                        .representation
                        .same(edge.implementation_type, edge.declared_type)
                        || receiver.is_some_and(|(declared, implemented)| {
                            !self.representation.same(declared, implemented)
                        });
                    for setter in [false, true] {
                        let (from, to) = if setter {
                            (
                                SlotKey::Setter(edge.implementation),
                                PlanKey::Setter(overridden),
                            )
                        } else {
                            (
                                SlotKey::Getter(edge.implementation),
                                PlanKey::Getter(overridden),
                            )
                        };
                        if self.numbers.number(to).is_none() {
                            continue;
                        }
                        let Some(&slot) = table.slots.get(&from) else {
                            continue;
                        };
                        let entry = if bridged {
                            Implementation::Bridged(Slot::AccessorBridge {
                                declared: edge.declared_type,
                                implemented: edge.implementation_type,
                                receiver,
                                setter,
                                target_slot: slot,
                            })
                        } else {
                            Implementation::Slot(slot)
                        };
                        if own {
                            found.insert(to, entry);
                        } else {
                            found.entry(to).or_insert(entry);
                        }
                    }
                }
            }
        }
        Ok((found, elsewhere))
    }
}
