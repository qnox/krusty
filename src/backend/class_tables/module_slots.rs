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

use crate::fx_hash::{HashMap, HashSet};

use super::module_classes::{ModuleClass, ModuleClasses, ModuleMember};
use super::module_interfaces::ModuleInterfaces;
use crate::fir::{
    CallableId, ExternalCallableId, PropertyId, ResolvedFunctionOverrideTarget,
    ResolvedPropertyOverrideTarget,
};
use crate::types::{SemanticCallRole, Ty, TypeName};

use super::members::any_slot;
use super::{AnyMember, Representation, Unsupported, FUNCTION_SLOT};

/// A member's identity in a planned table, the same in every file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlanKey {
    Callable(CallableId),
    Getter(PropertyId),
    Setter(PropertyId),
    /// A member of a dependency class a module class extends, by its provider identity. A
    /// dependency property is keyed by its getter's identity.
    DependencyCallable(ExternalCallableId),
    DependencyGetter(ExternalCallableId),
    DependencySetter(ExternalCallableId),
}

impl PlanKey {
    /// The key of the function `callable`.
    pub(crate) fn function(callable: ResolvedFunctionOverrideTarget) -> Self {
        match callable {
            ResolvedFunctionOverrideTarget::Module(callable) => Self::Callable(callable),
            ResolvedFunctionOverrideTarget::External(callable) => {
                Self::DependencyCallable(callable)
            }
        }
    }

    /// The key of the getter, or with `setter` the setter, of `property`.
    pub(crate) fn accessor(property: ResolvedPropertyOverrideTarget, setter: bool) -> Self {
        match (property, setter) {
            (ResolvedPropertyOverrideTarget::Module(property), false) => Self::Getter(property),
            (ResolvedPropertyOverrideTarget::Module(property), true) => Self::Setter(property),
            (ResolvedPropertyOverrideTarget::External(property), false) => {
                Self::DependencyGetter(property)
            }
            (ResolvedPropertyOverrideTarget::External(property), true) => {
                Self::DependencySetter(property)
            }
        }
    }
}

/// What a planned slot holds, by identity; the declaring file realizes each from its IR.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PlannedSlot {
    /// `kotlin.Any`'s own member, which no class of the hierarchy overrides.
    Any(AnyMember),
    /// The function slot, which nothing in the hierarchy implements.
    Reserved,
    /// The member's implementation: its own, or the one it inherits.
    Member(PlanKey),
    /// See [`super::Slot::Bridge`].
    Bridge {
        declared: CallableId,
        target_slot: u32,
        target: CallableId,
    },
    /// See [`super::Slot::FunctionBridge`].
    FunctionBridge {
        arity: usize,
        target_slot: u32,
        target: CallableId,
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
    /// Whether the declaring file appends interface-delegation forwarders after `entries`, which
    /// the plan does not number yet.
    pub appends_forwarders: bool,
}

/// Every named class's planned table, or why it has none. A class without one is numbered by its
/// declaring file alone, and no other file dispatches through it.
#[derive(Debug, Default)]
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
        let carriers: HashMap<ResolvedFunctionOverrideTarget, (&[Ty], Ty)> = classes
            .iter()
            .flat_map(|(_, class)| class.members.iter())
            .filter_map(|member| match member {
                ModuleMember::Function {
                    callable,
                    parameters,
                    result,
                    ..
                } => Some((*callable, (&parameters[..], *result))),
                ModuleMember::Property { .. } => None,
            })
            .collect();
        let mut planner = Planner {
            classes,
            representation,
            carriers,
            tables: HashMap::default(),
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
                        let signature = if *suspend {
                            // The slot holds the function in the form every target that lowers
                            // coroutines on common IR gives it.
                            let (continuation, completed) =
                                crate::types::continuation_passing(*result);
                            (
                                parameters.iter().copied().chain([continuation]).collect(),
                                completed,
                            )
                        } else {
                            (parameters.clone(), *result)
                        };
                        signatures.insert(PlanKey::function(*callable), signature);
                    }
                    ModuleMember::Property {
                        property,
                        accessor_parameters: parameters,
                        value,
                        getter_value,
                        ..
                    } => {
                        signatures.insert(
                            PlanKey::accessor(*property, false),
                            (parameters.clone(), *getter_value),
                        );
                        let setter = parameters.iter().copied().chain([*value]).collect();
                        signatures.insert(PlanKey::accessor(*property, true), (setter, Ty::Unit));
                    }
                }
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
            PlannedSlot::Reserved | PlannedSlot::FunctionBridge { .. } => {
                Some(PlannedSignature::Untyped)
            }
            PlannedSlot::Member(key) => member(key),
            PlannedSlot::Bridge { declared, .. } => member(&PlanKey::Callable(*declared)),
            PlannedSlot::AccessorBridge {
                declared, setter, ..
            } => Some(if *setter {
                PlannedSignature::Member {
                    parameters: std::slice::from_ref(declared),
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
    carriers: HashMap<ResolvedFunctionOverrideTarget, (&'c [Ty], Ty)>,
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
            !self.representation.owns_base(superclass) && self.classes.class(superclass).is_some()
        });
        let parent = match class.superclass {
            Some(superclass) if self.representation.owns_base(superclass) => None,
            Some(superclass) if self.classes.class(superclass).is_some() => {
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
                    getter,
                    setter,
                    ..
                } => {
                    for (is_setter, needed) in [(false, *getter), (true, *setter)] {
                        if needed {
                            self.accessor(class, *property, is_setter, &mut entries, &mut slots)
                                .map_err(|reason| format!("{reason} (`{}`)", qualified()))?;
                        }
                    }
                }
            }
        }
        Ok(PlannedTable {
            superclass: planned_superclass,
            inherited,
            dependency: class.dependency,
            entries,
            slots,
            appends_forwarders: class.delegates_interfaces,
        })
    }

    fn function(
        &self,
        class: &ModuleClass,
        callable: ResolvedFunctionOverrideTarget,
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
                .get(&overridden)
                .copied()
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
            PlannedSlot::Member(PlanKey::function(callable)),
        );
        slots.insert(PlanKey::function(callable), slot);
        if function_bridge.is_none() && bridged.is_empty() {
            return Ok(());
        }
        // A bridge is a function of the declaring file, converting to and forwarding from module
        // declarations; a dependency class's own overrides are carried as they are declared.
        let module_callable = |callable| match callable {
            ResolvedFunctionOverrideTarget::Module(callable) => Ok(callable),
            ResolvedFunctionOverrideTarget::External(_) => {
                Err("an override carried differently from the dependency member it overrides")
            }
        };
        let target = module_callable(callable)?;
        if let Some(arity) = function_bridge {
            entries[FUNCTION_SLOT as usize] = PlannedSlot::FunctionBridge {
                arity,
                target_slot: slot,
                target,
            };
        }
        for (base_slot, declared) in bridged {
            entries[base_slot as usize] = PlannedSlot::Bridge {
                declared: module_callable(declared)?,
                target_slot: slot,
                target,
            };
        }
        Ok(())
    }

    fn accessor(
        &self,
        class: &ModuleClass,
        property: ResolvedPropertyOverrideTarget,
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
                Some(&slot) => Some((slot, edge.declared_type, edge.implementation_type)),
                // A `val` overridden by a `var` adds a setter the base never had.
                None if setter => None,
                None => return Err("a property override with no slot to replace".into()),
            },
            None => None,
        };
        let entry = PlannedSlot::Member(key(property));
        let slot = match replaced {
            Some((slot, declared, implemented))
                if !self.representation.same(declared, implemented) =>
            {
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
        slots.insert(key(property), slot);
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
