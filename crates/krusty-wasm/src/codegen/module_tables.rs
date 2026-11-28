//! The dispatch-table type of every class the module-wide slot plan covers, built from the plan
//! alone.
//!
//! A call through a class another file declares casts the receiver's table to the class's table
//! type and calls the function at the slot the plan numbers. Only the declaring file has the
//! class's IR, and it may come later, so the type is not the declaring file's to build: every file
//! reads it from here, built from the plan's slot signatures (Pass 1's headers,
//! `krusty::backend::class_tables::module_slots`), and the declaring file lays its table out as
//! this type. A class the plan does not cover, a body-local one among them, is typed by its file
//! alone, and no other file dispatches through it.
//!
//! A support unit is lowered between two files of a source module, under its own plan. The source
//! module's table types outlive it: the module's later files resume the tables its earlier files
//! built, so a call an earlier file casts to a class's table type meets the declaring file's table.
//!
//! A dependency class a support unit materializes is one class of the whole program, so its table
//! type is too: every module's plan, and the support unit laying it out, take the one type the
//! first of them built ([`ModuleTables::dependency`]). Otherwise a module class extending it
//! would extend a second, unrelated type, and a call in a support body casting to the first would
//! trap.

use krusty::fx_hash::HashMap;

use krusty::backend::class_tables::module_slots::{ModuleSlotPlan, PlannedSignature};
use krusty::types::TypeName;

use super::super::encode::{FieldType, Module, SubType};
use super::super::objects::{ObjectRoot, REFERENCE};
use super::lower::{carrier, parameter_carrier_of, Unsupported};

/// One class's table type and the function type of each of its slots, `None` for an untyped one.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ModuleTable {
    pub(super) table_type: u32,
    pub(super) slot_types: Vec<Option<u32>>,
}

/// The table types built so far from the current module's plan, by class, and the dependency
/// classes' of the whole program. A new plan starts with only the dependency classes'.
/// A plan's table types by class, or why a class has none.
type TableSet = HashMap<TypeName, Result<ModuleTable, Unsupported>>;

#[derive(Default)]
pub(super) struct ModuleTables {
    tables: TableSet,
    /// The plan `tables` belongs to: a source module's name, or `None` for a support unit's.
    source_module: Option<String>,
    /// The tables of the source module a support unit's plan interrupted, by its name.
    suspended: Option<(String, TableSet)>,
    published: TableSet,
    dependencies: HashMap<TypeName, ModuleTable>,
    /// The interface member numbers the current module's files take beyond the plan's: the
    /// members of interfaces KLIB units installed, numbered alike in every file.
    pub(super) extended: krusty::backend::class_tables::module_interfaces::ExtendedNumbers,
    structures: HashMap<TypeName, (Vec<FieldType>, Option<u32>, u32)>,
}

impl ModuleTables {
    /// Begin a new plan, forgetting the previous plan's tables but not the dependency classes'.
    /// `source_module` names the source module the plan is for, `None` for a support unit's: a
    /// support unit's plan sets the interrupted source module's tables aside, and that module's
    /// next file resumes them instead of building a second table type for a class it already
    /// typed.
    ///
    /// The plan's dependency classes bind current-provider names to layouts selected by exact
    /// frozen class identities; provider-local names never outlive the plan. A dependency class
    /// no producer has laid out yet takes the program's one table type
    /// ([`Self::dependency`]).
    pub(super) fn enter_plan(
        &mut self,
        source_module: Option<&str>,
        classes: &krusty::backend::class_tables::module_classes::ModuleClasses,
        classifiers: &dyn krusty::backend::BackendClassifierSource,
        layouts: &super::laid_out_classes::LaidOutClasses,
    ) {
        let previous = std::mem::take(&mut self.tables);
        if let Some(module) = self.source_module.take() {
            self.suspended = Some((module, previous));
        }
        if let Some(module) = source_module {
            if let Some((suspended, tables)) = self.suspended.take() {
                if suspended == module {
                    self.tables = tables;
                }
            }
            self.source_module = Some(module.to_owned());
        }
        self.published.clear();
        self.extended = Default::default();
        for (name, _) in classes.iter() {
            if !classes.is_dependency_class(name) {
                continue;
            }
            let Some(identity) = classifiers
                .classifier(name)
                .and_then(|fact| fact.klib_identity.clone())
            else {
                self.published.insert(
                    name,
                    Err("a dependency class without its selected declaration identity".into()),
                );
                continue;
            };
            match layouts.published_layout(&identity) {
                Ok(Some(layout)) => {
                    self.published.insert(
                        name,
                        Ok(ModuleTable {
                            table_type: layout.table_type,
                            slot_types: layout.slot_types.clone(),
                        }),
                    );
                }
                Err(reason) => {
                    self.published.insert(name, Err(reason));
                }
                Ok(None) => {}
            }
        }
    }

    /// The table type of the dependency class `class`, carried with `slot_types`: the one an
    /// earlier module or support unit built for it, or else `build`'s, which every later one then
    /// takes.
    pub(super) fn dependency(
        &mut self,
        class: TypeName,
        slot_types: &[Option<u32>],
        build: impl FnOnce() -> u32,
    ) -> Result<u32, Unsupported> {
        if let Some(known) = self.dependencies.get(&class) {
            if known.slot_types != slot_types {
                return Err(format!(
                    "a dependency class whose table two modules lay out differently (`{}`) {:?} vs {:?}",
                    // Boundary conversion for a diagnostic.
                    class.render().replace('/', "."), known.slot_types, slot_types
                ));
            }
            return Ok(known.table_type);
        }
        let table_type = build();
        self.dependencies.insert(
            class,
            ModuleTable {
                table_type,
                slot_types: slot_types.to_vec(),
            },
        );
        Ok(table_type)
    }

    /// The struct type of the dependency class `class`: the one an earlier support unit built for
    /// it, or else `build`'s, which every later one then takes. A struct built twice would be two
    /// types, and a value one unit made would fail the other's casts.
    pub(super) fn dependency_structure(
        &mut self,
        class: TypeName,
        fields: &[FieldType],
        supertype: Option<u32>,
        build: impl FnOnce() -> u32,
    ) -> Result<u32, Unsupported> {
        if let Some((known_fields, known_supertype, structure)) = self.structures.get(&class) {
            if known_fields.as_slice() != fields || *known_supertype != supertype {
                return Err(format!(
                    "a dependency class whose struct two modules lay out differently (`{}`)",
                    // Boundary conversion for a diagnostic.
                    class.render().replace('/', ".")
                ));
            }
            return Ok(*structure);
        }
        let structure = build();
        self.structures
            .insert(class, (fields.to_vec(), supertype, structure));
        Ok(structure)
    }

    /// The table of `class`, building it (and its superclasses') the first time a file asks;
    /// `None` when the plan does not cover the class.
    pub(super) fn table(
        &mut self,
        plan: &ModuleSlotPlan,
        class: TypeName,
        module: &mut Module,
        objects: &ObjectRoot,
    ) -> Option<Result<ModuleTable, Unsupported>> {
        if let Some(table) = self.tables.get(&class) {
            return Some(table.clone());
        }
        let planned = plan.table(class)?;
        let table = match planned {
            Ok(planned) => {
                let published = self.published.get(&class).cloned();
                match published {
                    Some(Ok(table)) => {
                        self.validate_published(plan, planned, table, module, objects)
                    }
                    Some(Err(reason)) => Err(reason),
                    None => self.build(plan, class, planned, module, objects),
                }
            }
            Err(reason) => Err(reason.to_owned()),
        };
        self.tables.insert(class, table.clone());
        Some(table)
    }

    /// Preserve the producer's nominal type, checking every consumer-known physical frame.
    fn validate_published(
        &self,
        plan: &ModuleSlotPlan,
        planned: &krusty::backend::class_tables::module_slots::PlannedTable,
        table: ModuleTable,
        module: &mut Module,
        objects: &ObjectRoot,
    ) -> Result<ModuleTable, Unsupported> {
        if planned.len() as usize != table.slot_types.len() {
            return Err("a published class table with a different physical slot count".into());
        }
        for slot in 0..planned.len() {
            let expected = match plan
                .slot_signature(planned, slot)
                .ok_or("a published slot without its selected member signature")?
            {
                PlannedSignature::Untyped => continue,
                PlannedSignature::Any(member) => objects.any_slot_types[member.slot() as usize],
                PlannedSignature::Member { parameters, result } => {
                    let mut params = vec![REFERENCE];
                    for &parameter in parameters {
                        params.push(parameter_carrier_of(parameter)?);
                    }
                    module.func_type(params, carrier(result)?.into_iter().collect())
                }
            };
            if table.slot_types[slot as usize] != Some(expected) {
                return Err(
                    "a selected member frame differing from its published physical slot".into(),
                );
            }
        }
        Ok(table)
    }

    fn build(
        &mut self,
        plan: &ModuleSlotPlan,
        class: TypeName,
        planned: &krusty::backend::class_tables::module_slots::PlannedTable,
        module: &mut Module,
        objects: &ObjectRoot,
    ) -> Result<ModuleTable, Unsupported> {
        let parent = match planned.superclass {
            Some(superclass) => Some(
                self.table(plan, superclass, module, objects)
                    .ok_or("a planned superclass the plan does not cover")??,
            ),
            None => None,
        };
        let mut slot_types = Vec::with_capacity(planned.len() as usize);
        for slot in 0..planned.len() {
            let own = match plan
                .slot_signature(planned, slot)
                .ok_or("a planned slot without its member's signature")?
            {
                PlannedSignature::Any(member) => {
                    Some(objects.any_slot_types[member.slot() as usize])
                }
                PlannedSignature::Untyped => None,
                PlannedSignature::Member { parameters, result } => {
                    let mut params = vec![REFERENCE];
                    for &parameter in parameters {
                        params.push(parameter_carrier_of(parameter)?);
                    }
                    Some(module.func_type(params, carrier(result)?.into_iter().collect()))
                }
            };
            let inherited = parent
                .as_ref()
                .and_then(|parent| parent.slot_types.get(slot as usize).copied().flatten());
            slot_types.push(match (inherited, own) {
                (Some(inherited), Some(own)) if inherited != own => {
                    return Err(
                        "an override whose signature differs from the member it overrides"
                            .to_string(),
                    )
                }
                (inherited, None) => inherited,
                (_, own) => own,
            });
        }
        let mut declare = || {
            module.subtype(SubType {
                composite: super::classes::table_struct(
                    &slot_types,
                    objects.itable,
                    objects.type_infos.ty,
                ),
                supertype: Some(
                    parent
                        .as_ref()
                        .map_or(objects.any_table, |parent| parent.table_type),
                ),
                open: true,
            })
        };
        let table_type = if planned.is_dependency() {
            self.dependency(class, &slot_types, declare)?
        } else {
            declare()
        };
        Ok(ModuleTable {
            table_type,
            slot_types,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoClassifiers;
    impl krusty::backend::BackendClassifierSource for NoClassifiers {
        fn classifier(
            &self,
            _: TypeName,
        ) -> Option<std::sync::Arc<krusty::backend::BackendClassifierFact>> {
            None
        }
    }

    fn typed(tables: &mut ModuleTables, class: TypeName, table_type: u32) {
        tables.tables.insert(
            class,
            Ok(ModuleTable {
                table_type,
                slot_types: vec![None],
            }),
        );
    }

    fn table_type(tables: &ModuleTables, class: TypeName) -> Option<u32> {
        tables
            .tables
            .get(&class)
            .and_then(|table| table.as_ref().ok())
            .map(|table| table.table_type)
    }

    #[test]
    fn a_support_unit_between_two_files_keeps_the_source_module_tables() {
        let (classes, layouts) = (Default::default(), Default::default());
        let class = krusty::types::type_name("Phase");
        let mut tables = ModuleTables::default();
        tables.enter_plan(Some("main"), &classes, &NoClassifiers, &layouts);
        typed(&mut tables, class, 44);

        // A support unit plans apart from the module, then the module's next file resumes it.
        tables.enter_plan(None, &classes, &NoClassifiers, &layouts);
        assert_eq!(table_type(&tables, class), None);
        typed(&mut tables, class, 7);
        tables.enter_plan(Some("main"), &classes, &NoClassifiers, &layouts);
        assert_eq!(table_type(&tables, class), Some(44));

        // Another source module starts its own tables, and the first module's are not resumed.
        tables.enter_plan(Some("other"), &classes, &NoClassifiers, &layouts);
        assert_eq!(table_type(&tables, class), None);
        tables.enter_plan(None, &classes, &NoClassifiers, &layouts);
        tables.enter_plan(Some("main"), &classes, &NoClassifiers, &layouts);
        assert_eq!(table_type(&tables, class), None);
    }
}
