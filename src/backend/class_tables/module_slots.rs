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
//! Interface members are numbered elsewhere.

use crate::fx_hash::{HashMap, HashSet};

use super::module_classes::{ModuleClass, ModuleClasses, ModuleMember};
use crate::fir::{
    CallableId, PropertyId, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget,
};
use crate::types::{SemanticCallRole, Ty, TypeName};

use super::members::any_slot;
use super::{AnyMember, Representation, Unsupported, FUNCTION_SLOT};

/// A member's identity in a planned table, the same in every file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PlanKey {
    Callable(CallableId),
    Getter(PropertyId),
    Setter(PropertyId),
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
pub(crate) struct PlannedTable {
    /// How many of `entries` the superclass's planned table holds.
    pub(crate) inherited: u32,
    pub(crate) entries: Vec<PlannedSlot>,
    pub(crate) slots: HashMap<PlanKey, u32>,
}

/// Every named class's planned table, or why it has none. A class without one is numbered by its
/// declaring file alone, and no other file dispatches through it.
#[derive(Debug, Default)]
pub(crate) struct ModuleSlotPlan {
    tables: HashMap<TypeName, Result<PlannedTable, Unsupported>>,
}

impl ModuleSlotPlan {
    /// A plan that numbers nothing: every class is numbered by its declaring file alone.
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// Plan every class of `classes`, superclasses first.
    pub(crate) fn build(classes: &ModuleClasses, representation: &dyn Representation) -> Self {
        let carriers: HashMap<CallableId, (&[Ty], Ty)> = classes
            .iter()
            .flat_map(|(_, class)| class.members.iter())
            .filter_map(|member| match member {
                ModuleMember::Function {
                    callable,
                    parameters,
                    result,
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
        for (name, _) in classes.iter() {
            let _ = planner.plan(name);
        }
        Self {
            tables: planner.tables,
        }
    }

    /// The planned table of `classifier`, `None` when the plan does not cover it at all.
    pub(crate) fn table(&self, classifier: TypeName) -> Option<Result<&PlannedTable, &str>> {
        self.tables
            .get(&classifier)
            .map(|table| table.as_ref().map_err(String::as_str))
    }
}

struct Planner<'c> {
    classes: &'c ModuleClasses,
    representation: &'c dyn Representation,
    carriers: HashMap<CallableId, (&'c [Ty], Ty)>,
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
        let parent = match class.superclass {
            Some(superclass) if self.classes.class(superclass).is_some() => {
                Some(self.plan(superclass).map_err(|_| {
                    format!(
                        "a class whose superclass has no module-wide layout (`{}`)",
                        qualified()
                    )
                })?)
            }
            Some(superclass) if self.representation.owns_base(superclass) => None,
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
            inherited,
            entries,
            slots,
        })
    }

    fn function(
        &self,
        class: &ModuleClass,
        callable: CallableId,
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
            let ResolvedFunctionOverrideTarget::Module(overridden) = edge.overridden else {
                continue;
            };
            if edge.overridden_is_interface {
                continue;
            }
            let slot = slots
                .get(&PlanKey::Callable(overridden))
                .copied()
                .ok_or("an override with no slot to replace")?;
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
            PlannedSlot::Member(PlanKey::Callable(callable)),
        );
        slots.insert(PlanKey::Callable(callable), slot);
        if let Some(arity) = function_bridge {
            entries[FUNCTION_SLOT as usize] = PlannedSlot::FunctionBridge {
                arity,
                target_slot: slot,
                target: callable,
            };
        }
        for (base_slot, declared) in bridged {
            entries[base_slot as usize] = PlannedSlot::Bridge {
                declared,
                target_slot: slot,
                target: callable,
            };
        }
        Ok(())
    }

    fn accessor(
        &self,
        class: &ModuleClass,
        property: PropertyId,
        setter: bool,
        entries: &mut Vec<PlannedSlot>,
        slots: &mut HashMap<PlanKey, u32>,
    ) -> Result<(), Unsupported> {
        // The direct base's edge is the one whose slot is replaced, so it is preferred.
        let mut edges = class
            .property_overrides
            .iter()
            .filter(|edge| {
                edge.implementation == property
                    && !edge.overridden_is_interface
                    && matches!(edge.overridden, ResolvedPropertyOverrideTarget::Module(_))
            })
            .collect::<Vec<_>>();
        edges.sort_by_key(|edge| edge.depth != 1);
        let key = |property| {
            if setter {
                PlanKey::Setter(property)
            } else {
                PlanKey::Getter(property)
            }
        };
        let replaced = match edges.first() {
            Some(edge) => {
                let ResolvedPropertyOverrideTarget::Module(overridden) = edge.overridden else {
                    unreachable!("filtered to module properties above");
                };
                match slots.get(&key(overridden)) {
                    Some(&slot) => Some((slot, edge.declared_type, edge.implementation_type)),
                    // A `val` overridden by a `var` adds a setter the base never had.
                    None if setter => None,
                    None => return Err("a property override with no slot to replace".into()),
                }
            }
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
