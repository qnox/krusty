//! Slot numbers for the module's named classes, computed from their headers alone.
//!
//! A call through a class another file declares indexes that class's table by a number the
//! calling file cannot read off any IR. So for every named class of the module the numbering is
//! decided once, from [`ModuleClasses`], by rules every file applies alike; the declaring file
//! then FILLS those numbers from its IR ([`super::class_vtable`]), and a caller elsewhere reads
//! the same number from the same plan.
//!
//! The rules are the per-file ones of [`super::class_vtable`], applied to header members in source
//! order: a table starts as its superclass's (or `kotlin.Any`'s three slots and the reserved
//! function slot); each member takes the slot of the class member it overrides when both carry
//! values alike, or a new slot with a bridge left in the base's; an override of `kotlin.Any`'s
//! members takes their fixed slots, and `invoke` over a function type the fixed function slot.
//! Interface members are numbered in [`super::module_interfaces`].

#[cfg(test)]
mod file_class_tests;
mod published_slots;

pub use published_slots::PublishedClassSlots;

use crate::fx_hash::{HashMap, HashSet};

use super::module_classes::{ForwardedMember, ModuleClass, ModuleClasses, ModuleMember};
use super::module_interfaces::ModuleInterfaces;
use crate::fir::{ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
use crate::types::{SemanticCallRole, Ty, TypeName};

use super::members::any_slot;
use super::{AnyMember, Representation, Unsupported, FUNCTION_SLOT};

pub use super::member_keys::PlanKey;

/// What a planned slot holds, by identity; the declaring file realizes each from its IR.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PlannedSlot {
    /// `kotlin.Any`'s own member, which no class of the hierarchy overrides.
    Any(AnyMember),
    /// The function slot, which nothing in the hierarchy implements.
    Reserved,
    /// A physical producer slot whose declaration is not visible in this consumer inventory.
    Inherited { slot: u32 },
    /// The member's implementation: its own, or the one it inherits.
    Member(PlanKey),
    /// See [`super::Slot::Bridge`].
    Bridge {
        declared: PlanKey,
        signature: (Box<[Ty]>, Ty),
        target_slot: u32,
        target: PlanKey,
    },
    /// See [`super::Slot::FunctionBridge`].
    FunctionBridge {
        arity: usize,
        target_slot: u32,
        target: PlanKey,
    },
    /// See [`super::Slot::AccessorBridge`].
    AccessorBridge {
        declared: Ty,
        implemented: Ty,
        setter: bool,
        target_slot: u32,
    },
}

/// One class's planned table.
#[derive(Clone, Debug)]
pub struct PlannedTable {
    /// The superclass whose planned table this one extends; `None` for one extending
    /// `kotlin.Any` or a base no file declares.
    pub superclass: Option<TypeName>,
    /// How many of `entries` the superclass's planned table holds.
    pub(crate) inherited: u32,
    /// Whether the class is a dependency class, planned from its provider's declarations.
    pub(crate) dependency: bool,
    pub(crate) entries: Vec<PlannedSlot>,
    pub slots: HashMap<PlanKey, u32>,
}

/// Every named class's planned table, or why it has none. A class without one is numbered by its
/// declaring file alone, and no other file dispatches through it.
#[derive(Clone, Debug, Default)]
pub struct ModuleSlotPlan {
    tables: HashMap<TypeName, Result<PlannedTable, Unsupported>>,
    /// What each planned member takes after its dispatch receiver and answers.
    signatures: HashMap<PlanKey, (Box<[Ty]>, Ty)>,
    interfaces: ModuleInterfaces,
}

/// What a planned slot's entry takes after the dispatch receiver and answers, as every file reads
/// it from the plan: the type a target gives the slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlannedSignature<'p> {
    /// `kotlin.Any`'s own member at its fixed slot.
    Any(AnyMember),
    /// The reserved function slot, or one holding a function-type bridge: no file types it.
    Untyped,
    Member {
        parameters: &'p [Ty],
        result: Ty,
    },
}

impl ModuleSlotPlan {
    /// A plan that numbers nothing: every class is numbered by its declaring file alone.
    #[cfg(any(test, feature = "test-support"))]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Plan every class of `classes`, superclasses first.
    pub fn build(classes: &ModuleClasses, representation: &dyn Representation) -> Self {
        Self::build_with_tables(
            classes,
            representation,
            HashMap::default(),
            HashMap::default(),
        )
    }

    /// Plan against already-emitted dependency layouts by their exact declaration bindings.
    pub fn build_with_published_slots(
        classes: &ModuleClasses,
        representation: &dyn Representation,
        declarations: &super::super::CheckedBackendCallables,
        published: &dyn Fn(TypeName) -> Result<Option<PublishedClassSlots>, Unsupported>,
    ) -> Self {
        let tables = published_slots::bind(classes, representation, declarations, published);
        Self::build_with_tables(classes, representation, tables, HashMap::default())
    }

    /// Extend this file's view with anonymous implementations whose superclass already has a
    /// producer-owned module table. The additions are transient: callers must retain this plan
    /// only for the current `IrFile` and never publish its `FileFunction` identities.
    pub(crate) fn with_file_class(
        &self,
        ir: &crate::ir::IrFile,
        class: crate::ir::ClassId,
        representation: &dyn Representation,
    ) -> Result<Self, Unsupported> {
        let Some(classes) = ModuleClasses::from_file_class(ir, class) else {
            return Ok(self.clone());
        };
        let classifier = ir.classes[class as usize].fq_name;
        let superclass = ir.classes[class as usize].superclass;
        if self.table(classifier).is_some() {
            return Err("a file-local class identity collides with a planned declaration".into());
        }
        if self.table(superclass).is_none() {
            return Ok(self.clone());
        }
        let mut projected = Self::build_with_tables(
            &classes,
            representation,
            self.tables.clone(),
            self.signatures.clone(),
        );
        projected.signatures.extend(self.signatures.clone());
        projected.interfaces = self.interfaces.clone();
        Ok(projected)
    }

    fn build_with_tables(
        classes: &ModuleClasses,
        representation: &dyn Representation,
        tables: HashMap<TypeName, Result<PlannedTable, Unsupported>>,
        inherited_signatures: HashMap<PlanKey, (Box<[Ty]>, Ty)>,
    ) -> Self {
        let mut carriers = inherited_signatures;
        carriers.extend(
            classes
                .iter()
                .flat_map(|(_, class)| class.members.iter())
                .flat_map(|member| match member {
                    ModuleMember::Function {
                        callable,
                        parameters,
                        result,
                        ..
                    } => vec![(*callable, (parameters.clone(), *result))],
                    ModuleMember::Property {
                        property,
                        getter_key,
                        setter_key,
                        accessor_parameters,
                        value,
                        getter_value,
                        ..
                    } => vec![
                        (*getter_key, (accessor_parameters.clone(), *getter_value)),
                        (
                            setter_key.unwrap_or_else(|| PlanKey::accessor(*property, true)),
                            (
                                accessor_parameters
                                    .iter()
                                    .copied()
                                    .chain([*value])
                                    .collect(),
                                Ty::Unit,
                            ),
                        ),
                    ],
                })
                .collect::<HashMap<_, _>>(),
        );
        let mut planner = Planner {
            classes,
            representation,
            carriers,
            tables,
        };
        // Planning is a pure function of each class, so the order classes are visited in only
        // decides which table is computed first.
        for (name, class) in classes.iter() {
            if class.dependency && representation.owns_base(name) {
                continue;
            }
            let _ = planner.plan(name);
        }
        let mut signatures: HashMap<PlanKey, (Box<[Ty]>, Ty)> = HashMap::default();
        for (_, class) in classes.iter() {
            for member in class.members.iter() {
                match member {
                    ModuleMember::Function {
                        callable,
                        parameters,
                        result,
                        suspend,
                    } => {
                        signatures.insert(*callable, carried(parameters, *result, *suspend));
                    }
                    ModuleMember::Property {
                        property,
                        getter_key,
                        setter_key,
                        accessor_parameters: parameters,
                        value,
                        getter_value,
                        ..
                    } => {
                        signatures.insert(*getter_key, (parameters.clone(), *getter_value));
                        let setter = parameters.iter().copied().chain([*value]).collect();
                        signatures.insert(
                            setter_key.unwrap_or_else(|| PlanKey::accessor(*property, true)),
                            (setter, Ty::Unit),
                        );
                    }
                }
            }
        }
        for (name, class) in classes.iter() {
            for (ordinal, forwarder) in class.forwarders.iter().enumerate() {
                signatures.insert(
                    PlanKey::Forwarder {
                        class: name,
                        ordinal: u32::try_from(ordinal).expect("too many forwarders"),
                    },
                    carried(&forwarder.parameters, forwarder.result, forwarder.suspend),
                );
            }
        }
        Self {
            tables: planner.tables,
            signatures,
            interfaces: ModuleInterfaces::build(classes, representation),
        }
    }

    /// The signature of slot `slot` of `table`, one of this plan's tables; `None` when the slot is
    /// outside the table or names a member the plan has no signature for.
    pub fn slot_signature<'p>(
        &'p self,
        table: &'p PlannedTable,
        slot: u32,
    ) -> Option<PlannedSignature<'p>> {
        let member = |key: &PlanKey| {
            self.signatures
                .get(key)
                .map(|(parameters, result)| PlannedSignature::Member {
                    parameters,
                    result: *result,
                })
        };
        match table.entries.get(slot as usize)? {
            PlannedSlot::Any(any) => Some(PlannedSignature::Any(*any)),
            PlannedSlot::Reserved
            | PlannedSlot::Inherited { .. }
            | PlannedSlot::FunctionBridge { .. } => Some(PlannedSignature::Untyped),
            PlannedSlot::Member(key) => member(key),
            PlannedSlot::Bridge { signature, .. } => Some(PlannedSignature::Member {
                parameters: &signature.0,
                result: signature.1,
            }),
            PlannedSlot::AccessorBridge {
                declared, setter, ..
            } => Some(if *setter {
                PlannedSignature::Member {
                    parameters: &accessor_bridge_parameters(*declared, None, true),
                    result: Ty::Unit,
                }
            } else {
                PlannedSignature::Member {
                    parameters: &[],
                    result: *declared,
                }
            }),
        }
    }

    /// The module-wide numbers of interface members.
    pub fn interfaces(&self) -> &ModuleInterfaces {
        &self.interfaces
    }

    /// The declaration keys that share one numbered interface slot: its introduced spelling and
    /// every inherited/redeclared alias. A target persisting physical interface entries uses
    /// these checked keys to record stable declaration identities instead of module-local numbers.
    pub fn interface_member_keys(&self, number: u32) -> Option<Vec<PlanKey>> {
        if number >= self.interfaces.count() {
            return None;
        }
        let member = self.interfaces.member(number);
        let mut keys = Vec::with_capacity(member.aliases.len() + 1);
        keys.push(member.key);
        keys.extend(member.aliases.iter().copied());
        Some(keys)
    }

    /// Every class the plan covers, with its planned table or why it has none.
    pub fn tables(&self) -> impl Iterator<Item = (TypeName, Result<&PlannedTable, &str>)> {
        self.tables
            .iter()
            .map(|(name, table)| (*name, table.as_ref().map_err(String::as_str)))
    }

    /// The planned table of `classifier`, `None` when the plan does not cover it at all.
    pub fn table(&self, classifier: TypeName) -> Option<Result<&PlannedTable, &str>> {
        self.tables
            .get(&classifier)
            .map(|table| table.as_ref().map_err(String::as_str))
    }
}

struct Planner<'c> {
    classes: &'c ModuleClasses,
    representation: &'c dyn Representation,
    carriers: HashMap<PlanKey, (Box<[Ty]>, Ty)>,
    tables: HashMap<TypeName, Result<PlannedTable, Unsupported>>,
}

impl Planner<'_> {
    fn plan(&mut self, name: TypeName) -> Result<PlannedTable, Unsupported> {
        if let Some(table) = self.tables.get(&name) {
            return table.clone();
        }
        let class = self
            .classes
            .class(name)
            .expect("only module classes are planned");
        let table = self.table(name, class);
        self.tables.insert(name, table.clone());
        table
    }

    fn table(&mut self, name: TypeName, class: &ModuleClass) -> Result<PlannedTable, Unsupported> {
        let qualified = || name.render().replace('/', ".");
        // A base the target's runtime owns is laid out by the runtime, even where the plan also
        // holds its dependency declaration.
        let planned_superclass = class.superclass.filter(|&superclass| {
            !self.representation.owns_base(superclass)
                && (self.classes.class(superclass).is_some()
                    || self.tables.contains_key(&superclass))
        });
        let parent = match class.superclass {
            Some(superclass) if self.representation.owns_base(superclass) => None,
            Some(superclass)
                if self.classes.class(superclass).is_some()
                    || self.tables.contains_key(&superclass) =>
            {
                Some(self.plan(superclass).map_err(|_| {
                    format!(
                        "a class whose superclass has no module-wide layout (`{}`)",
                        qualified()
                    )
                })?)
            }
            Some(superclass) => {
                return Err(format!(
                    "a superclass declared outside this module (`{}` extends `{}`)",
                    qualified(),
                    superclass.render().replace('/', ".")
                ))
            }
            None => None,
        };
        // One fixed function slot holds one `invoke`: a class overriding the `invoke` of two
        // function types, named directly or through interfaces, declines as `class_vtable` does.
        let function_types = class
            .function_overrides
            .iter()
            .filter(|edge| {
                edge.overridden_role == Some(SemanticCallRole::KotlinFunctionInvoke)
                    && matches!(edge.overridden, ResolvedFunctionOverrideTarget::External(_))
            })
            .map(|edge| edge.overridden_owner)
            .collect::<HashSet<_>>();
        if class.function_type_arities.len() > 1
            || function_types.len() > 1
            || class.function_type_arities.iter().any(|&arity| arity > 22)
        {
            return Err(format!(
                "a class implementing more than one function type, or one of more than 22 \
                 parameters (`{}`)",
                qualified()
            ));
        }
        let inherited = parent
            .as_ref()
            .map_or(0, |parent| parent.entries.len() as u32);
        let (mut entries, mut slots) = match parent {
            Some(parent) => (parent.entries, parent.slots),
            None => (
                AnyMember::ALL
                    .map(PlannedSlot::Any)
                    .into_iter()
                    .chain([PlannedSlot::Reserved])
                    .collect(),
                HashMap::default(),
            ),
        };
        for member in &class.members {
            match member {
                ModuleMember::Function {
                    callable,
                    parameters,
                    result,
                    ..
                } => self.function(
                    class,
                    *callable,
                    (parameters, *result),
                    &mut entries,
                    &mut slots,
                )?,
                ModuleMember::Property {
                    property,
                    getter_key,
                    setter_key,
                    getter,
                    setter,
                    ..
                } => {
                    for (is_setter, needed, implementation_key) in [
                        (false, *getter, Some(*getter_key)),
                        (true, *setter, *setter_key),
                    ] {
                        if needed {
                            let implementation_key = implementation_key
                                .ok_or("a mutable property without a checked setter key")?;
                            self.accessor(
                                class,
                                *property,
                                implementation_key,
                                is_setter,
                                &mut entries,
                                &mut slots,
                            )
                            .map_err(|reason| format!("{reason} (`{}`)", qualified()))?;
                        }
                    }
                }
            }
        }
        for (ordinal, forwarder) in class.forwarders.iter().enumerate() {
            let key = PlanKey::Forwarder {
                class: name,
                ordinal: u32::try_from(ordinal).expect("too many forwarders"),
            };
            // A forwarder that also realizes a class member it inherits (`class E : B(), C by
            // D()` where both declare `x`) takes that member's number, as an override would;
            // one realizing interface members alone takes a new one.
            let mut replaces = None;
            for member in forwarder.realizes.iter() {
                let member = match *member {
                    ForwardedMember::Function(callable) => PlanKey::function(callable),
                    ForwardedMember::Getter(property) => PlanKey::accessor(property, false),
                    ForwardedMember::Setter(property) => PlanKey::accessor(property, true),
                };
                let Some(&slot) = slots.get(&member) else {
                    continue;
                };
                let (parameters, result) = self
                    .carriers
                    .get(&member)
                    .ok_or("a delegated class member with no module-wide signature")?;
                if !self.same(
                    (&forwarder.parameters, forwarder.result),
                    (parameters, *result),
                ) {
                    return Err(format!(
                        "a delegated member carried differently from the class member it \
                         overrides (`{}`)",
                        qualified()
                    ));
                }
                if replaces
                    .replace(slot)
                    .is_some_and(|previous| previous != slot)
                {
                    return Err(format!(
                        "a delegated member overriding class members at different slots (`{}`)",
                        qualified()
                    ));
                }
            }
            let slot = place(&mut entries, replaces, PlannedSlot::Member(key));
            slots.insert(key, slot);
        }
        Ok(PlannedTable {
            superclass: planned_superclass,
            inherited,
            dependency: class.dependency,
            entries,
            slots,
        })
    }

    fn function(
        &self,
        class: &ModuleClass,
        callable: PlanKey,
        carriers: (&[Ty], Ty),
        entries: &mut Vec<PlannedSlot>,
        slots: &mut HashMap<PlanKey, u32>,
    ) -> Result<(), Unsupported> {
        let edges = class
            .function_overrides
            .iter()
            .filter(|edge| edge.implementation == callable)
            .collect::<Vec<_>>();
        let any_replaces = edges.iter().find_map(|edge| any_slot(edge.overridden_role));
        let invoke = edges.iter().any(|edge| {
            edge.overridden_role == Some(SemanticCallRole::KotlinFunctionInvoke)
                && matches!(edge.overridden, ResolvedFunctionOverrideTarget::External(_))
        });
        let carries_references = carriers
            .0
            .iter()
            .all(|parameter| self.representation.is_reference(*parameter))
            && self.representation.is_reference(carriers.1);
        let fixed = any_replaces.or((invoke && carries_references).then_some(FUNCTION_SLOT));
        let function_bridge = (fixed.is_none() && invoke).then_some(carriers.0.len());
        let mut class_replaces = None;
        let mut bridged = Vec::new();
        for edge in &edges {
            if edge.overridden_is_interface {
                continue;
            }
            let overridden = edge.overridden;
            let slot = match (slots.get(&PlanKey::function(overridden)), overridden) {
                (Some(&slot), _) => slot,
                // A dependency member no planned class declares is a dependency class's member
                // that is no table's: one of `kotlin.Any`'s, numbered by its role above.
                (None, ResolvedFunctionOverrideTarget::External(_)) => continue,
                (None, ResolvedFunctionOverrideTarget::Module(_)) => {
                    return Err("an override with no slot to replace".into())
                }
            };
            let declared = self
                .carriers
                .get(&PlanKey::function(overridden))
                .map(|(parameters, result)| (&parameters[..], *result))
                .ok_or("an override of a member with no module-wide layout")?;
            if self.same(carriers, declared) {
                class_replaces = Some(slot);
            } else {
                bridged.push((slot, overridden));
            }
        }
        let slot = place(
            entries,
            fixed.or(class_replaces),
            PlannedSlot::Member(callable),
        );
        slots.insert(callable, slot);
        if function_bridge.is_none() && bridged.is_empty() {
            return Ok(());
        }
        // A bridge is a function of the declaring file, converting to and forwarding from module
        // declarations; a dependency class's own overrides are carried as they are declared.
        if let Some(arity) = function_bridge {
            entries[FUNCTION_SLOT as usize] = PlannedSlot::FunctionBridge {
                arity,
                target_slot: slot,
                target: callable,
            };
        }
        for (base_slot, declared) in bridged {
            let declared_key = PlanKey::function(declared);
            let declared_signature = self
                .carriers
                .get(&declared_key)
                .ok_or("an override of a member with no module-wide layout")?
                .clone();
            entries[base_slot as usize] = PlannedSlot::Bridge {
                declared: declared_key,
                signature: declared_signature,
                target_slot: slot,
                target: callable,
            };
        }
        Ok(())
    }

    fn accessor(
        &self,
        class: &ModuleClass,
        property: ResolvedPropertyOverrideTarget,
        implementation_key: PlanKey,
        setter: bool,
        entries: &mut Vec<PlannedSlot>,
        slots: &mut HashMap<PlanKey, u32>,
    ) -> Result<(), Unsupported> {
        let key = |property| PlanKey::accessor(property, setter);
        // The direct base's edge is the one whose slot is replaced, so it is preferred. A
        // dependency property no planned class declares is not a table's.
        let mut edges = class
            .property_overrides
            .iter()
            .filter(|edge| {
                edge.implementation == property
                    && if setter {
                        edge.setter_key == Some(implementation_key)
                    } else {
                        edge.getter_key == implementation_key
                    }
                    && !edge.overridden_is_interface
                    && match edge.overridden {
                        ResolvedPropertyOverrideTarget::Module(_) => true,
                        ResolvedPropertyOverrideTarget::External(_) => {
                            slots.contains_key(&PlanKey::accessor(edge.overridden, false))
                        }
                    }
            })
            .collect::<Vec<_>>();
        edges.sort_by_key(|edge| edge.depth != 1);
        let replaced = match edges.first() {
            Some(edge) => match slots.get(&key(edge.overridden)) {
                Some(&slot) => {
                    let receiver = match (edge.declared_receiver, edge.implementation_receiver) {
                        (Some(declared), Some(implemented)) => Some((declared, implemented)),
                        (None, None) => None,
                        _ => return Err(
                            "a member-extension property override with mismatched receiver roles"
                                .into(),
                        ),
                    };
                    Some((slot, edge.declared_type, edge.implementation_type, receiver))
                }
                // A `val` overridden by a `var` adds a setter the base never had.
                None if setter => None,
                None => return Err("a property override with no slot to replace".into()),
            },
            None => None,
        };
        let entry = PlannedSlot::Member(implementation_key);
        let slot = match replaced {
            Some((slot, declared, implemented, receiver))
                if !self.representation.same(declared, implemented)
                    || receiver.is_some_and(|(declared, implemented)| {
                        !self.representation.same(declared, implemented)
                    }) =>
            {
                if receiver.is_some() {
                    return Err(
                        "a member-extension property representation bridge is not supported".into(),
                    );
                }
                let own = place(entries, None, entry);
                entries[slot as usize] = PlannedSlot::AccessorBridge {
                    declared,
                    implemented,
                    setter,
                    target_slot: own,
                };
                own
            }
            Some((slot, ..)) => place(entries, Some(slot), entry),
            None => place(entries, None, entry),
        };
        slots.insert(implementation_key, slot);
        Ok(())
    }

    /// Whether an implementation carrying `implementation` can stand in a slot declared with
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

impl PlannedTable {
    /// How many slots the table has.
    pub fn len(&self) -> u32 {
        u32::try_from(self.entries.len()).expect("too many slots")
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// What a slot holding a function of `parameters` answering `result` is typed by: a `suspend`
/// one in the form every target that lowers coroutines on common IR gives it.
fn carried(parameters: &[Ty], result: Ty, suspend: bool) -> (Box<[Ty]>, Ty) {
    if suspend {
        let (continuation, completed) = crate::types::continuation_passing(result);
        (
            parameters.iter().copied().chain([continuation]).collect(),
            completed,
        )
    } else {
        (parameters.into(), result)
    }
}

/// Put `entry` in the slot it replaces, or in a new one at the end.
fn place(entries: &mut Vec<PlannedSlot>, replaces: Option<u32>, entry: PlannedSlot) -> u32 {
    match replaces {
        Some(slot) => {
            entries[slot as usize] = entry;
            slot
        }
        None => {
            entries.push(entry);
            (entries.len() - 1) as u32
        }
    }
}
