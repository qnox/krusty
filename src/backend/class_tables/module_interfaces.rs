//! Interface member numbers for the whole module, computed from declaration headers alone.
//!
//! A call through an interface-typed receiver knows only the interface, so the number it reads has
//! to mean the same member in every implementor, whichever file declares it. The numbers are
//! therefore decided once from [`ModuleClasses`], by the rules [`super::interface_slots`] applies
//! within one file: an interface inherits the numbers of the interfaces it extends; a member that
//! redeclares an inherited one joins its number unless it needs a bridge there, and keeps the
//! inherited spelling numbered too; a new member takes the next number. A target that keeps these
//! numbers in a table of their own indexes that table by the number directly.

use crate::fx_hash::{HashMap, HashSet};

use crate::fir::{CallableId, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
use crate::types::{Ty, TypeName};

use super::members::any_slot;
use super::module_classes::{ModuleClasses, ModuleInterface, ModuleInterfaceMember};
use super::module_slots::PlanKey;
use super::{Representation, Unsupported};

/// One numbered interface member: every spelling of it, and every interface that declares it.
#[derive(Clone, Debug)]
pub(crate) struct NumberedMember {
    /// The spelling the number was introduced with.
    pub(crate) key: PlanKey,
    /// The same member as spelled through each interface that redeclares it.
    pub(crate) aliases: Vec<PlanKey>,
    /// Each interface that declares this number, with the member it declares there.
    pub(crate) declarations: Vec<MemberDeclaration>,
}

/// An interface's own member standing at a number: what an implementor inherits through that
/// interface when it supplies nothing itself, if the member has a body.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemberDeclaration {
    pub(crate) interface: TypeName,
    pub(crate) member: PlanKey,
    /// The inherited spelling whose carriers the number wears, where `member` carries values
    /// differently and so stands there only through a bridge.
    pub(crate) bridged_from: Option<PlanKey>,
}

/// One interface's place in the numbering.
#[derive(Clone, Debug)]
pub(crate) struct NumberedInterface {
    /// The module interfaces it extends, transitively, itself first.
    pub(crate) closure: Vec<TypeName>,
}

/// The module's interface member numbers.
#[derive(Debug, Default)]
pub(crate) struct ModuleInterfaces {
    numbers: HashMap<PlanKey, u32>,
    members: Vec<NumberedMember>,
    interfaces: HashMap<TypeName, Result<NumberedInterface, Unsupported>>,
    /// Each numbered member's carriers as its declaration states them.
    carriers: HashMap<PlanKey, Carriers>,
    /// Interface members that redeclare one of `kotlin.Any`'s, by the fixed class slot they stand
    /// in instead of a number.
    fixed: HashMap<CallableId, u32>,
}

/// A member's parameters and result, or a property accessor's value type.
#[derive(Clone, Debug)]
pub(crate) enum Carriers {
    Function { parameters: Box<[Ty]>, result: Ty },
    Property,
}

impl ModuleInterfaces {
    /// Number every interface of `classes`, extended interfaces first.
    pub(crate) fn build(classes: &ModuleClasses, representation: &dyn Representation) -> Self {
        let declared: HashMap<TypeName, &ModuleInterface> = classes.interfaces().collect();
        let mut numbering = Numbering {
            declared: &declared,
            representation,
            mutable: classes
                .interfaces()
                .flat_map(|(_, interface)| interface.members.iter())
                .filter_map(|member| match member {
                    ModuleInterfaceMember::Property { property, mutable } => {
                        Some((*property, *mutable))
                    }
                    ModuleInterfaceMember::Function { .. } => None,
                })
                .collect(),
            carriers: HashMap::default(),
            result: Self::default(),
        };
        for (_, interface) in classes.interfaces() {
            for member in interface.members.iter() {
                if let ModuleInterfaceMember::Function {
                    callable,
                    parameters,
                    result,
                } = member
                {
                    numbering
                        .carriers
                        .insert(*callable, (&parameters[..], *result));
                }
            }
        }
        for (name, _) in classes.interfaces() {
            let _ = numbering.number(name, &mut Vec::new());
        }
        let mut result = numbering.result;
        for (_, interface) in classes.interfaces() {
            for edge in interface.function_overrides.iter() {
                if let Some(slot) = any_slot(edge.overridden_role) {
                    result.fixed.insert(edge.implementation, slot);
                }
            }
        }
        for (_, interface) in classes.interfaces() {
            for member in interface.members.iter() {
                match member {
                    ModuleInterfaceMember::Function {
                        callable,
                        parameters,
                        result: returned,
                    } => {
                        result.carriers.insert(
                            PlanKey::Callable(*callable),
                            Carriers::Function {
                                parameters: parameters.clone(),
                                result: *returned,
                            },
                        );
                    }
                    ModuleInterfaceMember::Property { property, .. } => {
                        result
                            .carriers
                            .insert(PlanKey::Getter(*property), Carriers::Property);
                        result
                            .carriers
                            .insert(PlanKey::Setter(*property), Carriers::Property);
                    }
                }
            }
        }
        result
    }

    /// The number of one interface member spelling, the same in every file of the module.
    pub(crate) fn number(&self, key: PlanKey) -> Option<u32> {
        self.numbers.get(&key).copied()
    }

    /// The `kotlin.Any` slot an interface member redeclaring `equals`, `hashCode` or `toString`
    /// stands in: every object's table holds it there, so it has no interface number.
    pub(crate) fn fixed_slot(&self, callable: CallableId) -> Option<u32> {
        self.fixed.get(&callable).copied()
    }

    pub(crate) fn member(&self, number: u32) -> &NumberedMember {
        &self.members[number as usize]
    }

    /// The numbered interface `interface`, `None` when the module declares no such interface.
    pub(crate) fn interface(
        &self,
        interface: TypeName,
    ) -> Option<Result<&NumberedInterface, &str>> {
        self.interfaces
            .get(&interface)
            .map(|numbered| numbered.as_ref().map_err(String::as_str))
    }

    /// The parameters and result a numbered function member declares.
    pub(crate) fn function_carriers(&self, key: PlanKey) -> Option<(&[Ty], Ty)> {
        match self.carriers.get(&key)? {
            Carriers::Function { parameters, result } => Some((parameters, *result)),
            Carriers::Property => None,
        }
    }

    /// Every number `interfaces` (each with its closure) declares, ascending.
    pub(crate) fn numbers_of(&self, interfaces: &HashSet<TypeName>) -> Vec<u32> {
        let mut numbers = self
            .members
            .iter()
            .enumerate()
            .filter(|(_, member)| {
                member
                    .declarations
                    .iter()
                    .any(|declaration| interfaces.contains(&declaration.interface))
            })
            .map(|(number, _)| number as u32)
            .collect::<Vec<_>>();
        numbers.sort_unstable();
        numbers
    }
}

struct Numbering<'c> {
    declared: &'c HashMap<TypeName, &'c ModuleInterface>,
    representation: &'c dyn Representation,
    mutable: HashMap<crate::fir::PropertyId, bool>,
    carriers: HashMap<crate::fir::CallableId, (&'c [Ty], Ty)>,
    result: ModuleInterfaces,
}

impl Numbering<'_> {
    /// Number `name` after every interface it extends; `visiting` guards against a cycle.
    fn number(
        &mut self,
        name: TypeName,
        visiting: &mut Vec<TypeName>,
    ) -> Result<NumberedInterface, Unsupported> {
        if let Some(numbered) = self.result.interfaces.get(&name) {
            return numbered.clone();
        }
        if visiting.contains(&name) {
            return Err("a cycle among interfaces".into());
        }
        visiting.push(name);
        let numbered = self.numbered(name, visiting);
        visiting.pop();
        self.result.interfaces.insert(name, numbered.clone());
        numbered
    }

    fn numbered(
        &mut self,
        name: TypeName,
        visiting: &mut Vec<TypeName>,
    ) -> Result<NumberedInterface, Unsupported> {
        let qualified = || name.render().replace('/', ".");
        let interface = self.declared[&name];
        let mut closure = vec![name];
        for &base in interface.interfaces.iter() {
            // An interface a dependency declares has no number here; its members are called the
            // way they are called today.
            if !self.declared.contains_key(&base) {
                continue;
            }
            let numbered = self.number(base, visiting).map_err(|reason| {
                format!(
                    "an interface extending one with no module-wide numbering (`{}`): {reason}",
                    qualified()
                )
            })?;
            for inherited in numbered.closure {
                if !closure.contains(&inherited) {
                    closure.push(inherited);
                }
            }
        }
        for member in interface.members.iter() {
            for group in self.groups(interface, member)? {
                self.place(name, group);
            }
        }
        Ok(NumberedInterface { closure })
    }

    /// The spellings one member stands for, each group one number at most: the member's own key
    /// first, then each inherited interface member it redeclares, with whether that one needs a
    /// bridge. A property contributes a getter group and, for a `var`, a setter group.
    fn groups(
        &self,
        interface: &ModuleInterface,
        member: &ModuleInterfaceMember,
    ) -> Result<Vec<Vec<(PlanKey, bool)>>, Unsupported> {
        match member {
            ModuleInterfaceMember::Function {
                callable,
                parameters,
                result,
            } => {
                let edges = interface
                    .function_overrides
                    .iter()
                    .filter(|edge| edge.implementation == *callable)
                    .collect::<Vec<_>>();
                // A member standing in one of `kotlin.Any`'s fixed slots is numbered there, the
                // same way for every object.
                if edges
                    .iter()
                    .any(|edge| any_slot(edge.overridden_role).is_some())
                {
                    return Ok(Vec::new());
                }
                let mut group = vec![(PlanKey::Callable(*callable), false)];
                for edge in edges {
                    let ResolvedFunctionOverrideTarget::Module(overridden) = edge.overridden else {
                        continue;
                    };
                    if !edge.overridden_is_interface {
                        continue;
                    }
                    let declared = self
                        .carriers
                        .get(&overridden)
                        .copied()
                        .ok_or("a redeclared interface member with no module-wide header")?;
                    let bridged = !self.same((parameters, *result), declared);
                    group.push((PlanKey::Callable(overridden), bridged));
                }
                Ok(vec![group])
            }
            ModuleInterfaceMember::Property { property, mutable } => {
                let accessors: &[bool] = if *mutable { &[false, true] } else { &[false] };
                let mut groups = Vec::new();
                for &setter in accessors {
                    let key = |property| {
                        if setter {
                            PlanKey::Setter(property)
                        } else {
                            PlanKey::Getter(property)
                        }
                    };
                    let mut group = vec![(key(*property), false)];
                    for edge in interface.property_overrides.iter().filter(|edge| {
                        edge.implementation == *property && edge.overridden_is_interface
                    }) {
                        let ResolvedPropertyOverrideTarget::Module(overridden) = edge.overridden
                        else {
                            continue;
                        };
                        // A `val` redeclared as a `var` adds a setter the base never had.
                        if setter && !self.mutable.get(&overridden).copied().unwrap_or(false) {
                            continue;
                        }
                        // A member extension's receiver crosses the accessor like its value.
                        let receiver = edge.declared_receiver.zip(edge.implementation_receiver);
                        let bridged = !self
                            .representation
                            .same(edge.declared_type, edge.implementation_type)
                            || receiver.is_some_and(|(declared, implemented)| {
                                !self.representation.same(declared, implemented)
                            });
                        group.push((key(overridden), bridged));
                    }
                    groups.push(group);
                }
                Ok(groups)
            }
        }
    }

    /// Number one group of `interface`'s spellings.
    fn place(&mut self, interface: TypeName, group: Vec<(PlanKey, bool)>) {
        let member = group[0].0;
        let inherited: Vec<Option<u32>> = group
            .iter()
            .map(|(key, _)| self.result.numbers.get(key).copied())
            .collect();
        // The member's own spelling joins an inherited number that carries it as it stands; a
        // number needing a bridge wears the base's carriers, which the member does not take.
        let joined = match group
            .iter()
            .zip(&inherited)
            .find_map(|((_, bridged), number)| number.filter(|_| !bridged))
        {
            Some(number) => number,
            None => {
                let number = self.result.members.len() as u32;
                self.result.members.push(NumberedMember {
                    key: member,
                    aliases: Vec::new(),
                    declarations: Vec::new(),
                });
                number
            }
        };
        for ((key, bridged), number) in group.iter().zip(&inherited) {
            let number = number.unwrap_or(joined);
            self.result.numbers.insert(*key, number);
            let numbered = &mut self.result.members[number as usize];
            if *key != numbered.key && !numbered.aliases.contains(key) {
                numbered.aliases.push(*key);
            }
            // One interface declares a number once; where two of its spellings land on the same
            // number, the one carrying the member as it stands is what the interface supplies.
            let declaration = MemberDeclaration {
                interface,
                member,
                bridged_from: bridged.then_some(*key),
            };
            match numbered
                .declarations
                .iter_mut()
                .find(|existing| existing.interface == interface)
            {
                Some(existing) if !bridged => *existing = declaration,
                Some(_) => {}
                None => numbered.declarations.push(declaration),
            }
        }
    }

    /// Whether an implementation carrying `implementation` can stand at a number declared with
    /// `declared`.
    fn same(&self, implementation: (&[Ty], Ty), declared: (&[Ty], Ty)) -> bool {
        implementation.0.len() == declared.0.len()
            && implementation
                .0
                .iter()
                .zip(declared.0)
                .all(|(a, b)| self.representation.same(*a, *b))
            && self.representation.same(implementation.1, declared.1)
    }
}
