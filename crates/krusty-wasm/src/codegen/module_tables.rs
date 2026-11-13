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

use krusty::fx_hash::HashMap;

use krusty::backend::class_tables::module_slots::{ModuleSlotPlan, PlannedSignature};
use krusty::types::TypeName;

use super::super::encode::{Module, SubType};
use super::super::objects::{ObjectRoot, REFERENCE};
use super::lower::{carrier, Unsupported};

/// One class's table type and the function type of each of its slots, `None` for an untyped one.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ModuleTable {
    pub(super) table_type: u32,
    pub(super) slot_types: Vec<Option<u32>>,
}

/// The table types built so far from the current module's plan, by class. A new plan starts an
/// empty one.
#[derive(Default)]
pub(super) struct ModuleTables {
    tables: HashMap<TypeName, Result<ModuleTable, Unsupported>>,
}

impl ModuleTables {
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
            Ok(planned) => self.build(plan, planned, module, objects),
            Err(reason) => Err(reason.to_owned()),
        };
        self.tables.insert(class, table.clone());
        Some(table)
    }

    fn build(
        &mut self,
        plan: &ModuleSlotPlan,
        planned: &krusty::backend::class_tables::module_slots::PlannedTable,
        module: &mut Module,
        objects: &ObjectRoot,
    ) -> Result<ModuleTable, Unsupported> {
        if planned.appends_forwarders {
            // Its type would lack the forwarders' slots, so no file can type the table yet.
            return Err(
                "a class delegating an interface, whose forwarders the module-wide \
                        plan does not number"
                    .to_owned(),
            );
        }
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
                        params.push(carrier(parameter)?.ok_or("a `Unit` parameter")?);
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
        let table_type = module.subtype(SubType {
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
        });
        Ok(ModuleTable {
            table_type,
            slot_types,
        })
    }
}
