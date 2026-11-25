//! Calls of a fake override a local class inherits, which no declaration of the library defines.
//!
//! kotlinc names a fake override of a local class in a call as a file-local signature whose
//! container is the fake override's property (or class) and whose id is the hash of the
//! override's own mangled signature (`FileLocalIdSignatureComputer.stableIndexForFakeOverride`).
//! A class contributes no text to a mangle, but the function a local class is declared in
//! contributes its whole mangled signature, so that text precedes the inherited declaration's own
//! mangle. The call is linked by finding, among the members the class inherits, the declaration
//! whose enclosed mangle has exactly that hash.

mod enclosing_function;

use super::*;
use crate::klib_libraries::KlibDeclarationBodies;
use crate::metadata::id_signature::{
    accessor_mangled, mangled_id, property_mangled, Accessor, ClassScope, DeclarationContainer,
    Placement, PropertyShape,
};
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrClass, KlibIrMember, KlibIrType};
use crate::metadata::klib_ir::{KlibFileLocalOrigin, KlibIrSignature};
use crate::metadata::semantic::KotlinType;
use enclosing_function::enclosing_function_mangle;

/// The outcome of resolving a fake override: the inherited getter's signature, or `None` when the
/// call names no fake override.
pub(super) type Inherited = Result<Option<KlibIrSignature>, KlibBodyDeclineReason>;

/// The getter of the inherited property that the file-local `callee` of the active module is a
/// fake override of, or `None` when `callee` is no fake override of a local class.
pub(super) fn inherited_getter(linker: &Linker<'_, '_>, callee: &KlibIrSignature) -> Inherited {
    resolve_inherited_getter(linker.sources.bodies, &linker.active_module, callee)
}

/// What the emitted body of `identity` reaches: its serialized edges, with a call of a fake
/// override of a local class resolved to the inherited getter lowering links. A fake override no
/// unit can resolve keeps its serialized edge, and lowering reports it.
pub(crate) fn emitted_body(
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
) -> crate::klib_libraries::EmittedBody {
    bodies.emitted_body(identity, super::super::EMITTED_CALL_FORMS, &|callee| {
        resolve_inherited_getter(bodies, &identity.module, callee)
            .ok()
            .flatten()
    })
}

/// [`inherited_getter`] for a body of `module`, over the libraries alone. The dependency closure
/// walks the calls a body lowers, so it resolves a fake override as lowering does.
pub(crate) fn resolve_inherited_getter(
    bodies: &KlibDeclarationBodies,
    module: &str,
    callee: &KlibIrSignature,
) -> Inherited {
    LocalHierarchy { bodies, module }.resolve_inherited_getter(callee)
}

struct LocalHierarchy<'b> {
    bodies: &'b KlibDeclarationBodies,
    module: &'b str,
}

impl LocalHierarchy<'_> {
    /// Resolve the file-local `callee` of the active module as a getter of a property its class
    /// inherits.
    fn resolve_inherited_getter(
        &self,
        callee: &KlibIrSignature,
    ) -> Result<Option<KlibIrSignature>, KlibBodyDeclineReason> {
        let module = self.module;
        let bodies = self.bodies;
        // A getter the module declares carries a counter id, whatever its container chain: only a
        // call no declaration of the module defines can be a fake override.
        if bodies.module_local_function(module, callee).is_some() {
            return Ok(None);
        }
        let Some(KlibFileLocalOrigin {
            container: property,
            id: getter_id,
        }) = bodies.module_file_local_origin(module, callee)
        else {
            return Ok(None);
        };
        let Some(KlibFileLocalOrigin {
            container: class_signature,
            id: property_id,
        }) = bodies.module_file_local_origin(module, property)
        else {
            return Ok(None);
        };
        let Some((arena, class)) = bodies.module_local_class(module, class_signature) else {
            return Ok(None);
        };
        let enclosing = self.enclosing_function_mangle(class_signature)?;
        let mut matching: Vec<Candidate> = Vec::new();
        let mut pending = supertype_signatures(arena, class);
        let mut seen = crate::fx_hash::HashSet::default();
        while let Some(owner) = pending.pop() {
            if !seen.insert(owner.clone()) {
                continue;
            }
            // A supertype this module does not serialize declares no member to inherit here.
            let Some((owner_arena, owner_class)) = self.class_of(&owner) else {
                continue;
            };
            for member in &owner_class.members {
                let KlibIrMember::Property(declared) = member else {
                    continue;
                };
                let Some(getter) = declared.getter else {
                    continue;
                };
                let getter = owner_arena.function(getter);
                if crate::metadata::semantic::declaration_visibility(declared.base.flags)
                    == crate::types::Visibility::Private
                {
                    continue;
                }
                if getter.extension_receiver.is_some()
                    || !getter.context_parameters.is_empty()
                    || !getter.type_parameters.is_empty()
                {
                    // Such a property's mangled signature names types this unit does not model.
                    return Err(KlibBodyDeclineReason::UnsupportedOperation(
                        "a fake override of a property with a receiver or type parameters",
                    ));
                }
                let ids = enclosed_ids(&enclosing, &declared.name);
                if ids == (*property_id as u64, *getter_id as u64) {
                    matching.push(Candidate {
                        getter: getter.base.symbol.signature.clone(),
                        overridden: getter
                            .overridden
                            .iter()
                            .map(|symbol| symbol.signature.clone())
                            .collect(),
                    });
                }
            }
            pending.extend(supertype_signatures(owner_arena, owner_class));
        }
        most_specific_getter(&matching).map(Some)
    }

    /// The mangled signature of the function the local class `class` is declared in, found by
    /// following the classes that enclose it outward.
    fn enclosing_function_mangle(
        &self,
        class: &KlibIrSignature,
    ) -> Result<String, KlibBodyDeclineReason> {
        let module = self.module;
        let bodies = self.bodies;
        // The type-parameter containers kotlinc meets before the function: the property or getter
        // itself, then each local class outward.
        let mut containers = 1;
        let mut class = class;
        loop {
            let Some((_, declared)) = self.class_of(class) else {
                return Err(KlibBodyDeclineReason::UnsupportedOperation(
                    "a local class the module does not serialize",
                ));
            };
            if !declared.type_parameters.is_empty() {
                return Err(KlibBodyDeclineReason::UnsupportedOperation(
                    "a local class with type parameters",
                ));
            }
            containers += 1;
            let Some(origin) = bodies.module_file_local_origin(module, class) else {
                return Err(KlibBodyDeclineReason::UnsupportedOperation(
                    "a local class whose enclosing declaration the signature table does not record",
                ));
            };
            let container = &origin.container;
            if let Some((arena, function)) = bodies.module_public_function(module, container) {
                return enclosing_function_mangle(arena, function, container, containers);
            }
            if bodies.module_local_class(module, container).is_none() {
                return Err(KlibBodyDeclineReason::UnsupportedOperation(
                    "a local class declared in a declaration that is no top-level function or local class",
                ));
            }
            class = container;
        }
    }

    /// The class the serialized `signature` names, among the classes of the active module.
    fn class_of(&self, signature: &KlibIrSignature) -> Option<(&KlibIrArena, &KlibIrClass)> {
        self.bodies.module_declared_class(self.module, signature)
    }
}

/// An inherited getter whose enclosed mangle has the id of the call, with the getters it overrides.
struct Candidate {
    getter: KlibIrSignature,
    overridden: Vec<KlibIrSignature>,
}

/// The getter at the bottom of the override group the candidates form: the one no other candidate
/// overrides, directly or through other candidates. Candidates that do not form one group are
/// ambiguous.
fn most_specific_getter(
    candidates: &[Candidate],
) -> Result<KlibIrSignature, KlibBodyDeclineReason> {
    let overrides = |upper: &Candidate, lower: &Candidate| {
        let mut pending: Vec<&KlibIrSignature> = lower.overridden.iter().collect();
        let mut seen = crate::fx_hash::HashSet::default();
        while let Some(next) = pending.pop() {
            if !seen.insert(next) {
                continue;
            }
            if *next == upper.getter {
                return true;
            }
            if let Some(through) = candidates
                .iter()
                .find(|candidate| candidate.getter == *next)
            {
                pending.extend(through.overridden.iter());
            }
        }
        false
    };
    let mut bottoms = candidates.iter().filter(|candidate| {
        !candidates
            .iter()
            .any(|other| other.getter != candidate.getter && overrides(candidate, other))
    });
    match (bottoms.next(), bottoms.next()) {
        (Some(only), None) => Ok(only.getter.clone()),
        (None, _) => Err(KlibBodyDeclineReason::UnsupportedOperation(
            "a fake override that no inherited property getter matches",
        )),
        (Some(_), Some(_)) => Err(KlibBodyDeclineReason::SignatureMismatch(
            "a fake override names two inherited properties".to_owned(),
        )),
    }
}

/// The serialized signatures of the direct supertypes of a class.
fn supertype_signatures(arena: &KlibIrArena, class: &KlibIrClass) -> Vec<KlibIrSignature> {
    class
        .supertypes
        .iter()
        .rev()
        .filter_map(|&supertype| match arena.ty(supertype) {
            KlibIrType::Simple { classifier, .. } => Some(classifier.signature.clone()),
            _ => None,
        })
        .collect()
}

/// The ids of the non-extension, non-generic property `name` and of its getter as a fake override
/// of a class enclosed by a function mangled `enclosing`.
fn enclosed_ids(enclosing: &str, name: &str) -> (u64, u64) {
    let shape = PropertyShape::<KotlinType> {
        name,
        contexts: Vec::new(),
        receiver: None,
        type_parameters: Vec::new(),
        expect: false,
        placement: Placement::Ordinary,
    };
    let container = || DeclarationContainer::<KotlinType> {
        package: &[],
        classes: &[] as &[ClassScope<'_, KotlinType>],
        native_interop_library: false,
    };
    let property = property_mangled(container(), &shape)
        .expect("a property without a receiver or type parameters has a mangled signature");
    let (_, getter) = accessor_mangled(container(), &shape, Accessor::Getter)
        .expect("a getter without a receiver or type parameters has a mangled signature");
    (
        mangled_id(&format!("{enclosing}{property}")),
        mangled_id(&format!("{enclosing}{getter}")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn getter(slot: u32) -> KlibIrSignature {
        KlibIrSignature::FileLocal { file: 0, slot }
    }

    fn candidate(slot: u32, overridden: &[u32]) -> Candidate {
        Candidate {
            getter: getter(slot),
            overridden: overridden.iter().map(|&slot| getter(slot)).collect(),
        }
    }

    #[test]
    fn an_override_chain_resolves_to_its_most_specific_getter() {
        // `Derived.text` overrides `Middle.text`, which overrides `Base.text`; the scan meets them
        // in any order.
        let derived = candidate(3, &[2]);
        let middle = candidate(2, &[1]);
        let base = candidate(1, &[]);
        for group in [
            [&base, &middle, &derived],
            [&derived, &base, &middle],
            [&middle, &derived, &base],
        ] {
            let group: Vec<Candidate> = group
                .iter()
                .map(|member| candidate(slot_of(member), &slots(&member.overridden)))
                .collect();
            assert_eq!(most_specific_getter(&group).unwrap(), getter(3));
        }
    }

    #[test]
    fn getters_of_unrelated_properties_are_ambiguous() {
        let group = [candidate(1, &[]), candidate(2, &[])];
        assert_eq!(
            most_specific_getter(&group),
            Err(KlibBodyDeclineReason::SignatureMismatch(
                "a fake override names two inherited properties".to_owned()
            ))
        );
    }

    #[test]
    fn no_candidate_is_no_match() {
        assert!(matches!(
            most_specific_getter(&[]),
            Err(KlibBodyDeclineReason::UnsupportedOperation(_))
        ));
    }

    fn slot_of(candidate: &Candidate) -> u32 {
        match candidate.getter {
            KlibIrSignature::FileLocal { slot, .. } => slot,
            _ => unreachable!("the fixture uses file-local getters"),
        }
    }

    fn slots(signatures: &[KlibIrSignature]) -> Vec<u32> {
        signatures
            .iter()
            .map(|signature| match signature {
                KlibIrSignature::FileLocal { slot, .. } => *slot,
                _ => unreachable!("the fixture uses file-local getters"),
            })
            .collect()
    }
}
