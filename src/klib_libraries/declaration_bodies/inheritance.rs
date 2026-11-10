//! Members a class inherits without overriding them.
//!
//! A producer serializes no fake override: the linker rebuilds a member a class inherits without
//! overriding it, as kotlinc's fake-override builder does. A call symbol or an override edge
//! naming one stands for the members with the same member identity in the declaring class's
//! direct supertypes, followed until each reaches a serialized declaration. kotlinc's member ids
//! hash the member's own mangled signature, never its owner, so an inherited member keeps them in
//! every class.

use super::{KlibCalleeOrigin, KlibDeclarationBodies};
use crate::fx_hash::HashSet;
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrClass, KlibIrMember, KlibIrType};
use crate::metadata::klib_ir::KlibIrSignature;

impl KlibDeclarationBodies {
    /// The one serialized declaration the fake override `declaration` inherits. Of several
    /// inherited declarations, only one with a body is the implementation it inherits; more than
    /// one is ambiguous.
    pub(super) fn inherited_origin(
        &self,
        body_module: &str,
        declaration: &KlibDeclarationSignature,
    ) -> KlibCalleeOrigin {
        if declaring_member(declaration).is_none() {
            return KlibCalleeOrigin::Missing;
        }
        let found = match self.inherited(body_module, declaration) {
            Ok(found) => found,
            Err(()) => return KlibCalleeOrigin::Ambiguous,
        };
        match found.as_slice() {
            [] => KlibCalleeOrigin::Missing,
            [one] => KlibCalleeOrigin::Exact(one.clone()),
            several => {
                let implemented = several
                    .iter()
                    .filter(|identity| self.function_has_body(identity) == Some(true))
                    .collect::<Vec<_>>();
                match implemented.as_slice() {
                    [one] => KlibCalleeOrigin::Exact((*one).clone()),
                    _ => KlibCalleeOrigin::Ambiguous,
                }
            }
        }
    }

    /// The declarations a serialized override edge of a body in `body_module` overrides: the
    /// declaration it names, or those a fake override it names inherits.
    pub(crate) fn overridden_declarations(
        &self,
        body_module: &str,
        signature: &KlibPublicIdSignature,
    ) -> Result<Vec<KlibDependencyCallable>, &'static str> {
        let declaration = KlibDeclarationSignature::Public(signature.clone());
        match self.serialized_origin(body_module, &declaration) {
            KlibCalleeOrigin::Exact(identity) => return Ok(vec![identity]),
            KlibCalleeOrigin::Ambiguous => {
                return Err("an overridden declaration has more than one producer")
            }
            KlibCalleeOrigin::Missing => {}
        }
        let found = self
            .inherited(body_module, &declaration)
            .map_err(|()| "an overridden declaration's class is not unique")?;
        if found.is_empty() {
            return Err("an overridden declaration reaches no serialized declaration");
        }
        Ok(found)
    }

    /// Every serialized declaration the fake override `declaration` inherits, through each
    /// direct supertype of its declaring class until a serialized declaration is reached.
    fn inherited(
        &self,
        body_module: &str,
        declaration: &KlibDeclarationSignature,
    ) -> Result<Vec<KlibDependencyCallable>, ()> {
        let mut found = Vec::new();
        let mut pending = vec![declaration.clone()];
        let mut visited = HashSet::default();
        while let Some(declaration) = pending.pop() {
            if !visited.insert(declaration.clone()) {
                continue;
            }
            // The root is the fake override itself, which no module serializes.
            match self.serialized_origin(body_module, &declaration) {
                KlibCalleeOrigin::Exact(identity) => {
                    if !found.contains(&identity) {
                        found.push(identity);
                    }
                    continue;
                }
                KlibCalleeOrigin::Ambiguous => return Err(()),
                KlibCalleeOrigin::Missing => {}
            }
            let Some(member) = declaring_member(&declaration) else {
                continue;
            };
            let Some((arena, class)) = self.declaring_class(body_module, member)? else {
                continue;
            };
            for &supertype in &class.supertypes {
                let KlibIrType::Simple { classifier, .. } = arena.ty(supertype) else {
                    continue;
                };
                let KlibIrSignature::Public(supertype) = &classifier.signature else {
                    continue;
                };
                pending.extend(as_member_of(&declaration, supertype));
            }
        }
        Ok(found)
    }

    /// The one class in the producer graph rooted at `body_module` that declares `member`.
    fn declaring_class(
        &self,
        body_module: &str,
        member: &KlibPublicIdSignature,
    ) -> Result<Option<(&KlibIrArena, &KlibIrClass)>, ()> {
        let mut found = None;
        for module in self.body_provider_modules(body_module) {
            let Some(&library) = self.modules.get(&module) else {
                continue;
            };
            for tree in self.libraries[library].trees() {
                let mut pending = vec![&tree.declaration];
                while let Some(declaration) = pending.pop() {
                    let KlibIrMember::Class(id) = declaration else {
                        continue;
                    };
                    let class = tree.arena.class(*id);
                    pending.extend(class.members.iter());
                    let KlibIrSignature::Public(signature) = &class.base.symbol.signature else {
                        continue;
                    };
                    if member.is_member_of(signature)
                        && found.replace((&tree.arena, class)).is_some()
                    {
                        return Err(());
                    }
                }
            }
        }
        Ok(found)
    }
}

/// The member signature whose declaring class a member declaration names: a function's own, or
/// an accessor's property's. A top-level declaration has none.
fn declaring_member(declaration: &KlibDeclarationSignature) -> Option<&KlibPublicIdSignature> {
    let member = match declaration {
        KlibDeclarationSignature::Public(signature) => signature,
        KlibDeclarationSignature::Accessor(accessor) => accessor.property(),
        KlibDeclarationSignature::FileLocal { .. } => return None,
    };
    (member.declaration().segments().len() > 1).then_some(member)
}

/// `declaration` as the same member of `class`.
fn as_member_of(
    declaration: &KlibDeclarationSignature,
    class: &KlibPublicIdSignature,
) -> Option<KlibDeclarationSignature> {
    match declaration {
        KlibDeclarationSignature::Public(signature) => signature
            .as_member_of(class)
            .map(KlibDeclarationSignature::Public),
        KlibDeclarationSignature::Accessor(accessor) => {
            let property = accessor.property().as_member_of(class)?;
            Some(KlibDeclarationSignature::Accessor(
                KlibAccessorIdSignature::new(
                    property,
                    accessor.name().to_owned(),
                    accessor.member_id(),
                    accessor.mask(),
                ),
            ))
        }
        KlibDeclarationSignature::FileLocal { .. } => None,
    }
}
