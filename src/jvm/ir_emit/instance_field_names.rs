//! Physical JVM names of instance backing fields.
//!
//! Kotlin permits an instance property and a companion property with the same source name, but the
//! JVM field signature does not include the STATIC flag. kotlinc therefore keeps the companion
//! static's source name and suffixes the instance backing field (`result` -> `result$1`). This is
//! solely a JVM realization decision: IR properties, metadata, accessors, and resolver identities
//! retain the source name.
//!
//! Names are assigned once, in declaration order, for every class that emission touches. Later
//! consumers index that immutable class plan; they never rescan predecessors or rebuild the class's
//! static/declared-name sets.

use std::collections::{HashMap, HashSet};

use crate::ir::{IrClass, IrFile};
use crate::jvm::names::type_descriptor;
use crate::types::TypeName;

use super::declaration_types::{jvm_declared_ty, jvm_value_ty};

pub(super) fn instance_field_jvm_name(
    ir: &IrFile,
    run: &super::EmitRun,
    class: &IrClass,
    field_index: usize,
) -> String {
    run.instance_field_names
        .borrow_mut()
        .name(ir, class, field_index)
}

#[derive(Default)]
pub(super) struct InstanceFieldNamePlans {
    by_class: HashMap<TypeName, Vec<String>>,
}

impl InstanceFieldNamePlans {
    pub(super) fn name(&mut self, ir: &IrFile, class: &IrClass, field_index: usize) -> String {
        self.by_class
            .entry(class.fq_name)
            .or_insert_with(|| class_names(ir, class).names)
            .get(field_index)
            .expect("an instance field name index must belong to its class")
            .clone()
    }
}

struct FieldInput {
    source: String,
    value_descriptor: String,
    declared_descriptor: String,
    fixed: Option<String>,
}

struct NameAllocation {
    names: Vec<String>,
    /// Candidate names tested by the allocator. Retained for a deterministic scaling regression:
    /// unrelated fields require exactly one probe each, independent of class size.
    #[cfg(test)]
    candidate_probes: usize,
}

fn class_names(ir: &IrFile, class: &IrClass) -> NameAllocation {
    let captures: HashMap<usize, String> = class
        .ctor_args
        .iter()
        .filter_map(|argument| {
            let index = argument.field_index? as usize;
            let capture = argument.capture.as_ref()?;
            Some((
                index,
                super::super::capture_names::class_capture(ir, class, capture).field,
            ))
        })
        .collect();
    let fields = class
        .fields
        .iter()
        .enumerate()
        .map(|(index, field)| FieldInput {
            source: field.name.clone(),
            value_descriptor: type_descriptor(jvm_value_ty(&field.ty)),
            declared_descriptor: type_descriptor(jvm_declared_ty(&field.ty)),
            fixed: captures.get(&index).cloned().or_else(|| {
                class.lambda.as_ref().map(|lambda| {
                    super::super::capture_names::lambda_class_capture(ir, lambda, index)
                        .expect("every field of a lambda class stores one of its captures")
                        .field
                })
            }),
        })
        .collect();
    let owner = class.fq_name();
    let statics = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, field)| field.owner_matches(&owner))
        .map(|(index, field)| {
            (
                ir.static_field_jvm_name(index as u32).to_string(),
                type_descriptor(jvm_declared_ty(&field.ty)),
            )
        })
        .collect();
    allocate_names(fields, statics)
}

fn allocate_names(fields: Vec<FieldInput>, statics: HashSet<(String, String)>) -> NameAllocation {
    let statics = group_signatures(statics);
    let declared = group_signatures(
        fields
            .iter()
            .map(|field| (field.source.clone(), field.declared_descriptor.clone())),
    );
    let mut assigned = HashSet::with_capacity(fields.len());
    let mut names = Vec::with_capacity(fields.len());
    #[cfg(test)]
    let mut candidate_probes = 0;

    for field in fields {
        let name = if let Some(fixed) = field.fixed {
            fixed
        } else {
            let available = |candidate: &str| {
                !assigned.contains(candidate)
                    && !statics
                        .get(candidate)
                        .is_some_and(|descriptors| descriptors.contains(&field.value_descriptor))
            };
            #[cfg(test)]
            {
                candidate_probes += 1;
            }
            if available(&field.source) {
                field.source
            } else {
                (1usize..)
                    .map(|suffix| format!("{}${suffix}", field.source))
                    .find(|candidate| {
                        #[cfg(test)]
                        {
                            candidate_probes += 1;
                        }
                        available(candidate)
                            && !declared.get(candidate).is_some_and(|descriptors| {
                                descriptors.contains(&field.value_descriptor)
                            })
                    })
                    .expect("an unused JVM backing-field suffix always exists")
            }
        };
        assigned.insert(name.clone());
        names.push(name);
    }

    NameAllocation {
        names,
        #[cfg(test)]
        candidate_probes,
    }
}

fn group_signatures(
    entries: impl IntoIterator<Item = (String, String)>,
) -> HashMap<String, HashSet<String>> {
    let mut grouped = HashMap::<String, HashSet<String>>::new();
    for (name, descriptor) in entries {
        grouped.entry(name).or_default().insert(descriptor);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(source: &str, descriptor: &str) -> FieldInput {
        FieldInput {
            source: source.to_string(),
            value_descriptor: descriptor.to_string(),
            declared_descriptor: descriptor.to_string(),
            fixed: None,
        }
    }

    #[test]
    fn allocates_the_complete_collision_plan() {
        let mut capture = field("captured", "I");
        capture.fixed = Some("$captured_value".to_string());
        let allocation = allocate_names(
            vec![
                capture,
                field("result", "I"),
                field("same", "I"),
                field("same", "I"),
                field("same$1", "I"),
                field("typed", "I"),
            ],
            HashSet::from([
                ("result".to_string(), "I".to_string()),
                ("typed".to_string(), "J".to_string()),
            ]),
        );

        assert_eq!(
            allocation.names,
            [
                "$captured_value",
                "result$1",
                "same",
                "same$2",
                "same$1",
                "typed",
            ]
        );
    }

    #[test]
    fn unrelated_fields_take_one_candidate_probe_each() {
        let fields = (0..4096)
            .map(|index| field(&format!("property{index}"), "I"))
            .collect();
        let allocation = allocate_names(fields, HashSet::new());

        assert_eq!(allocation.names.len(), 4096);
        assert_eq!(allocation.candidate_probes, 4096);
    }
}
