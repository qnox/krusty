//! Rebind selected dependency declarations onto an already-emitted physical slot inventory.

use super::super::super::interface_identity::{member_identity, property_member_identity};
use super::super::super::CheckedBackendCallables;
use super::*;

/// Ty-free producer evidence. Unknown consumer declarations retain their physical inherited slots.
#[derive(Clone, Debug)]
pub struct PublishedClassSlots {
    pub count: u32,
    pub declarations: HashMap<Vec<u8>, u32>,
}

pub(super) fn bind(
    classes: &ModuleClasses,
    representation: &dyn Representation,
    declarations: &CheckedBackendCallables,
    published: &dyn Fn(TypeName) -> Result<Option<PublishedClassSlots>, Unsupported>,
) -> HashMap<TypeName, Result<PlannedTable, Unsupported>> {
    let mut tables = HashMap::default();
    for (name, class) in classes.iter() {
        if !class.dependency || representation.owns_base(name) {
            continue;
        }
        match published(name) {
            Ok(Some(layout)) => {
                tables.insert(
                    name,
                    lineage(classes, name, representation)
                        .and_then(|lineage| table(&lineage, representation, declarations, &layout)),
                );
            }
            Err(reason) => {
                tables.insert(name, Err(reason));
            }
            Ok(None) => {
                tables.insert(
                    name,
                    Err("a dependency class without its published physical slot inventory".into()),
                );
            }
        }
    }
    tables
}

/// Rebind declarations already selected in every ancestor header, including members a direct
/// parent merely inherits. Physical slot identities remain those published by that parent.
fn lineage<'a>(
    classes: &'a ModuleClasses,
    mut name: TypeName,
    representation: &dyn Representation,
) -> Result<Vec<&'a ModuleClass>, Unsupported> {
    let mut chain = Vec::new();
    let mut seen = HashSet::default();
    loop {
        if !seen.insert(name) {
            return Err("a cycle in published dependency class headers".into());
        }
        let class = classes
            .class(name)
            .ok_or("a published superclass without its selected header")?;
        chain.push(class);
        match class.superclass {
            Some(parent) if !representation.owns_base(parent) => name = parent,
            _ => return Ok(chain),
        }
    }
}

fn table(
    classes: &[&ModuleClass],
    representation: &dyn Representation,
    declarations: &CheckedBackendCallables,
    physical: &PublishedClassSlots,
) -> Result<PlannedTable, Unsupported> {
    if physical.count < FUNCTION_SLOT + 1 {
        return Err("a published class table missing its common prefix".into());
    }
    let mut entries = (0..physical.count)
        .map(|slot| PlannedSlot::Inherited { slot })
        .collect::<Vec<_>>();
    for member in AnyMember::ALL {
        entries[member.slot() as usize] = PlannedSlot::Any(member);
    }
    let mut slots = HashMap::default();
    let mut frames: HashMap<u32, (Box<[Ty]>, Ty)> = HashMap::default();
    for member in classes.iter().flat_map(|class| class.members.iter()) {
        let members = match member {
            ModuleMember::Function {
                callable,
                parameters,
                result,
                suspend,
            } => {
                let PlanKey::DependencyCallable(identity) = callable else {
                    return Err("a dependency table containing a source callable identity".into());
                };
                let declaration = declarations
                    .declaration_identity(*identity)
                    .ok_or("an inherited method without its frozen declaration identity")?;
                vec![(
                    *callable,
                    member_identity(declaration),
                    carried(parameters, *result, *suspend),
                )]
            }
            ModuleMember::Property {
                property,
                getter_key,
                setter_key,
                accessor_parameters,
                value,
                getter_value,
                getter,
                setter,
            } => {
                let ResolvedPropertyOverrideTarget::External(identity) = property else {
                    return Err("a dependency table containing a source property identity".into());
                };
                let declaration = declarations
                    .declaration_identity(*identity)
                    .ok_or("an inherited property without its frozen getter identity")?;
                let mut members = Vec::new();
                if *getter {
                    let PlanKey::DependencyGetter(getter_key_identity) = getter_key else {
                        return Err(
                            "a dependency table containing a file-local getter identity".into()
                        );
                    };
                    if *getter_key_identity != *identity {
                        return Err(
                            "a dependency getter key that disagrees with its property".into()
                        );
                    }
                    members.push((
                        *getter_key,
                        property_member_identity(declaration, false)
                            .ok_or("an inherited getter without accessor identity")?,
                        (accessor_parameters.clone(), *getter_value),
                    ));
                }
                if *setter {
                    let Some(PlanKey::DependencySetter(_)) = setter_key else {
                        return Err(
                            "a dependency table containing a file-local setter identity".into()
                        );
                    };
                    members.push((
                        *setter_key.expect("validated setter key"),
                        property_member_identity(declaration, true)
                            .ok_or("an inherited setter without accessor identity")?,
                        (
                            accessor_parameters
                                .iter()
                                .copied()
                                .chain([*value])
                                .collect(),
                            Ty::Unit,
                        ),
                    ));
                }
                members
            }
        };
        for (key, identity, frame) in members {
            let &slot = physical
                .declarations
                .get(&identity)
                .ok_or("an inherited declaration without its producer slot binding")?;
            let entry = entries
                .get_mut(slot as usize)
                .ok_or("a published member slot outside its class table")?;
            if let Some(previous) = frames.get(&slot) {
                if previous.0.len() != frame.0.len()
                    || !previous
                        .0
                        .iter()
                        .zip(frame.0.iter())
                        .all(|(&a, &b)| representation.same(a, b))
                    || !representation.same(previous.1, frame.1)
                {
                    return Err(
                        "conflicting declaration frames for one physical inherited slot".into(),
                    );
                }
            } else {
                frames.insert(slot, frame);
                *entry = PlannedSlot::Member(key);
            }
            if let Some(previous) = slots.insert(key, slot) {
                if previous != slot {
                    return Err(
                        "a dependency declaration assigned conflicting inherited slots".into(),
                    );
                }
            }
        }
    }
    Ok(PlannedTable {
        superclass: None,
        inherited: 0,
        dependency: true,
        entries,
        slots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fir::ExternalCallableId;

    struct Exact;
    impl Representation for Exact {
        fn same(&self, a: Ty, b: Ty) -> bool {
            a == b
        }
        fn is_reference(&self, _: Ty) -> bool {
            true
        }
        fn owns_base(&self, _: TypeName) -> bool {
            false
        }
    }

    fn imported_class() -> ModuleClass {
        ModuleClass {
            superclass: None,
            dependency: true,
            function_type_arities: Box::default(),
            members: Box::default(),
            function_overrides: Box::default(),
            property_overrides: Box::default(),
            forwarders: Box::default(),
        }
    }

    #[test]
    fn unprojected_producer_members_keep_their_physical_slots() {
        // Provider headers may omit final members. Their slots must still precede new
        // consumer overrides/members, and retain the producer's exact physical signatures.
        let physical = PublishedClassSlots {
            count: 9,
            declarations: HashMap::default(),
        };
        let planned = table(
            &[&imported_class()],
            &Exact,
            &CheckedBackendCallables::default(),
            &physical,
        )
        .unwrap();
        assert_eq!(planned.entries.len(), 9);
        for slot in FUNCTION_SLOT..9 {
            assert!(
                matches!(planned.entries[slot as usize], PlannedSlot::Inherited { slot: actual } if actual == slot)
            );
        }
        assert!(planned.slots.is_empty());
    }

    #[test]
    fn incomplete_physical_prefix_is_rejected() {
        let physical = PublishedClassSlots {
            count: FUNCTION_SLOT,
            declarations: HashMap::default(),
        };
        assert!(table(
            &[&imported_class()],
            &Exact,
            &CheckedBackendCallables::default(),
            &physical
        )
        .is_err());
    }

    #[test]
    fn imported_member_cannot_bind_without_its_frozen_identity() {
        let mut class = imported_class();
        class.members = vec![ModuleMember::Function {
            callable: PlanKey::DependencyCallable(ExternalCallableId::from_raw(7)),
            parameters: Box::default(),
            result: Ty::Unit,
            suspend: false,
        }]
        .into_boxed_slice();
        let physical = PublishedClassSlots {
            count: 9,
            declarations: HashMap::default(),
        };
        assert!(table(
            &[&class],
            &Exact,
            &CheckedBackendCallables::default(),
            &physical
        )
        .is_err());
    }

    #[test]
    fn file_local_function_keys_cannot_be_published_as_dependency_declarations() {
        let mut class = imported_class();
        class.members = vec![ModuleMember::Function {
            callable: PlanKey::FileFunction(0),
            parameters: Box::default(),
            result: Ty::Unit,
            suspend: false,
        }]
        .into_boxed_slice();
        let physical = PublishedClassSlots {
            count: 9,
            declarations: HashMap::default(),
        };
        let error = table(
            &[&class],
            &Exact,
            &CheckedBackendCallables::default(),
            &physical,
        )
        .expect_err("a file-local function identity is never a provider declaration");
        assert!(error.contains("source callable identity"));
    }

    #[test]
    fn file_local_property_accessors_cannot_be_published_as_dependency_declarations() {
        let getter = PlanKey::FileGetter {
            property: crate::fir::ResolvedPropertyOverrideTarget::External(
                ExternalCallableId::from_raw(8),
            ),
            function: 0,
        };
        let setter = PlanKey::FileSetter {
            property: crate::fir::ResolvedPropertyOverrideTarget::External(
                ExternalCallableId::from_raw(8),
            ),
            function: 1,
        };
        let class = ModuleClass {
            superclass: None,
            dependency: true,
            function_type_arities: Box::default(),
            members: vec![ModuleMember::Property {
                property: crate::fir::ResolvedPropertyOverrideTarget::External(
                    ExternalCallableId::from_raw(8),
                ),
                getter_key: getter,
                setter_key: Some(setter),
                accessor_parameters: Box::default(),
                value: Ty::String,
                getter_value: Ty::String,
                getter: true,
                setter: true,
            }]
            .into_boxed_slice(),
            function_overrides: Box::default(),
            property_overrides: Box::default(),
            forwarders: Box::default(),
        };
        let physical = PublishedClassSlots {
            count: 9,
            declarations: HashMap::default(),
        };
        let error = table(
            &[&class],
            &Exact,
            &CheckedBackendCallables::default(),
            &physical,
        )
        .expect_err("file-local accessors are never producer declarations");
        assert!(error.contains("file-local getter identity"));
    }
}
