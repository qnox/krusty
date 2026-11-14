//! Rebind selected dependency declarations onto an already-emitted physical slot inventory.

use super::*;
use crate::backend::interface_identity::{member_identity, property_member_identity};
use crate::backend::CheckedBackendCallables;

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
                tables.insert(name, table(class, representation, declarations, &layout));
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

fn table(
    class: &ModuleClass,
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
    for member in &class.members {
        let members = match member {
            ModuleMember::Function {
                callable,
                parameters,
                result,
                suspend,
            } => {
                let ResolvedFunctionOverrideTarget::External(identity) = callable else {
                    return Err("a dependency table containing a source callable identity".into());
                };
                let declaration = declarations
                    .declaration_identity(*identity)
                    .ok_or("an inherited method without its frozen declaration identity")?;
                vec![(
                    PlanKey::function(*callable),
                    member_identity(declaration),
                    carried(parameters, *result, *suspend),
                )]
            }
            ModuleMember::Property {
                property,
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
                    members.push((
                        PlanKey::accessor(*property, false),
                        property_member_identity(declaration, false)
                            .ok_or("an inherited getter without accessor identity")?,
                        (accessor_parameters.clone(), *getter_value),
                    ));
                }
                if *setter {
                    members.push((
                        PlanKey::accessor(*property, true),
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
            if slots.insert(key, slot).is_some() {
                return Err(
                    "a dependency declaration assigned more than one inherited slot".into(),
                );
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
            &imported_class(),
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
            &imported_class(),
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
            callable: ResolvedFunctionOverrideTarget::External(ExternalCallableId::from_raw(7)),
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
            &class,
            &Exact,
            &CheckedBackendCallables::default(),
            &physical
        )
        .is_err());
    }
}
