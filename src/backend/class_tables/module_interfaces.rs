//! Interface member numbers for the whole module, computed from declaration headers alone.
//!
//! A call through an interface-typed receiver knows only the interface, so the number it reads has
//! to mean the same member in every implementor, whichever file declares it. The numbers are
//! therefore decided once from [`ModuleClasses`]: an interface inherits the numbers of the
//! interfaces it extends; a member that redeclares an inherited one joins its number unless it
//! needs a bridge there, and keeps the inherited spelling numbered too; a new member takes the
//! next number. A target that keeps these
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
pub struct NumberedInterface {
    /// The module interfaces it extends, transitively, itself first.
    pub(crate) closure: Vec<TypeName>,
}

/// The module's interface member numbers.
#[derive(Clone, Debug, Default)]
pub struct ModuleInterfaces {
    numbers: HashMap<PlanKey, u32>,
    members: Vec<NumberedMember>,
    /// The members of interfaces a KLIB unit installed in this file, at the numbers
    /// [`ExtendedNumbers`] gave them, all beyond [`Self::members`].
    installed: HashMap<u32, NumberedMember>,
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
                    ..
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
                if let (Some(slot), ResolvedFunctionOverrideTarget::Module(implementation)) =
                    (any_slot(edge.overridden_role), edge.implementation)
                {
                    result.fixed.insert(implementation, slot);
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
                        suspend,
                    } => {
                        // A call and an implementation carry a suspend member in its
                        // continuation-passing form.
                        let (parameters, returned) =
                            super::module_slots::carried(parameters, *returned, *suspend);
                        result.carriers.insert(
                            PlanKey::Callable(*callable),
                            Carriers::Function {
                                parameters,
                                result: returned,
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

    /// This numbering extended with the interfaces `ir` holds that the module did not number:
    /// the ones a KLIB lowering unit installed with the body that uses them. Their members are
    /// numbered by `extended`, which the module's files share: one member of one library
    /// interface has one number in every file that installs it, no two members share one, and
    /// every number follows the module's own, so a call through one never reads a module
    /// member's. An installed interface that extends another numbered interface is refused,
    /// whichever of the two the IR declares first: its members would have to share that
    /// interface's numbers.
    pub(crate) fn with_installed(
        &self,
        ir: &crate::ir::IrFile,
        extended: &mut ExtendedNumbers,
    ) -> Result<Self, Unsupported> {
        let mut result = self.clone();
        // The installed interfaces are all known before any inheritance is checked, so the refusal
        // does not depend on whether a base is declared before or after the interface extending it.
        let installed: std::collections::HashSet<_> = ir
            .classes
            .iter()
            .filter(|class| class.is_interface && !self.interfaces.contains_key(&class.fq_name))
            .map(|class| class.fq_name)
            .collect();
        for class in &ir.classes {
            if !installed.contains(&class.fq_name) {
                continue;
            }
            let name = class.fq_name;
            let qualified = || name.render().replace('/', ".");
            if class
                .interfaces
                .iter()
                .any(|base| self.interfaces.contains_key(&base) || installed.contains(&base))
            {
                return Err(format!(
                    "an installed interface extending one with module-wide numbers (`{}`)",
                    qualified()
                ));
            }
            for (ordinal, &function) in class.methods.iter().enumerate() {
                let declaration = &ir.functions[function as usize];
                let key = PlanKey::Installed(function);
                let ordinal = u32::try_from(ordinal).expect("a method ordinal fits u32");
                let number = extended.installed(self.count(), name, ordinal);
                result.installed.insert(
                    number,
                    NumberedMember {
                        key,
                        aliases: Vec::new(),
                        declarations: vec![MemberDeclaration {
                            interface: name,
                            member: key,
                            bridged_from: None,
                        }],
                    },
                );
                result.numbers.insert(key, number);
                result.carriers.insert(
                    key,
                    Carriers::Function {
                        parameters: declaration.params.clone().into_boxed_slice(),
                        result: declaration.ret,
                    },
                );
            }
            result.interfaces.insert(
                name,
                Ok(NumberedInterface {
                    closure: vec![name],
                }),
            );
        }
        Ok(result)
    }

    /// How many interface members this module numbered. A call through a dependency interface
    /// takes the next numbers, so it does not reuse one of these.
    pub fn count(&self) -> u32 {
        u32::try_from(self.members.len()).expect("an interface member count fits u32")
    }

    /// The number of one interface member spelling, the same in every file of the module.
    pub fn number(&self, key: PlanKey) -> Option<u32> {
        self.numbers.get(&key).copied()
    }

    /// The `kotlin.Any` slot an interface member redeclaring `equals`, `hashCode` or `toString`
    /// stands in: every object's table holds it there, so it has no interface number.
    pub fn fixed_slot(&self, callable: CallableId) -> Option<u32> {
        self.fixed.get(&callable).copied()
    }

    pub(crate) fn member(&self, number: u32) -> &NumberedMember {
        self.members
            .get(number as usize)
            .or_else(|| self.installed.get(&number))
            .expect("a number this numbering gave")
    }

    /// The numbered interface `interface`, `None` when the module declares no such interface.
    pub fn interface(&self, interface: TypeName) -> Option<Result<&NumberedInterface, &str>> {
        self.interfaces
            .get(&interface)
            .map(|numbered| numbered.as_ref().map_err(String::as_str))
    }

    /// The parameters and result a numbered function member declares.
    pub fn function_carriers(&self, key: PlanKey) -> Option<(&[Ty], Ty)> {
        match self.carriers.get(&key)? {
            Carriers::Function { parameters, result } => Some((parameters, *result)),
            Carriers::Property => None,
        }
    }

    /// Every number `interfaces` (each with its closure) declares, ascending.
    pub fn numbers_of(&self, interfaces: &HashSet<TypeName>) -> Vec<u32> {
        let module = self
            .members
            .iter()
            .enumerate()
            .map(|(number, member)| (number as u32, member));
        let installed = self
            .installed
            .iter()
            .map(|(&number, member)| (number, member));
        let mut numbers = module
            .chain(installed)
            .filter(|(_, member)| {
                member
                    .declarations
                    .iter()
                    .any(|declaration| interfaces.contains(&declaration.interface))
            })
            .map(|(number, _)| number)
            .collect::<Vec<_>>();
        numbers.sort_unstable();
        numbers
    }
}

/// The interface member numbers a module's files take beyond the module's own
/// ([`ModuleInterfaces::count`]): the members of interfaces KLIB units install, by their library
/// identity, and the dependency interface members a target numbers as its files call them. One
/// module's files share one, so each number names one member module-wide.
#[derive(Clone, Debug, Default)]
pub struct ExtendedNumbers {
    next: u32,
    installed: HashMap<InstalledMember, u32>,
}

/// A member of an installed interface, by the interface's classifier (named by its library,
/// file and slot, so every file installing it names it alike) and the member's place in it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct InstalledMember {
    interface: TypeName,
    ordinal: u32,
}

impl ExtendedNumbers {
    /// A number no member of this module has, after the module's own `count` numbers.
    pub fn fresh(&mut self, count: u32) -> u32 {
        let number = self.next.max(count);
        self.next = number + 1;
        number
    }

    /// The number of the `ordinal`-th member of the installed `interface`: the one an earlier
    /// file gave it, or a fresh one.
    fn installed(&mut self, count: u32, interface: TypeName, ordinal: u32) -> u32 {
        let member = InstalledMember { interface, ordinal };
        if let Some(&number) = self.installed.get(&member) {
            return number;
        }
        let number = self.fresh(count);
        self.installed.insert(member, number);
        number
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
                ..
            } => {
                let edges = interface
                    .function_overrides
                    .iter()
                    .filter(|edge| {
                        edge.implementation == ResolvedFunctionOverrideTarget::Module(*callable)
                    })
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
                        edge.implementation == ResolvedPropertyOverrideTarget::Module(*property)
                            && edge.overridden_is_interface
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
