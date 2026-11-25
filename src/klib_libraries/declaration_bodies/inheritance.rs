//! Members a class inherits without overriding them.
//!
//! A producer serializes no fake override: the linker rebuilds a member a class inherits without
//! overriding it, as kotlinc's fake-override builder does. A call symbol or an override edge
//! naming one stands for the members with the same member identity in the declaring class's
//! direct supertypes, followed until each reaches a serialized declaration. kotlinc's member ids
//! hash the member's own mangled signature, never its owner, so an inherited member keeps them in
//! every class.

mod member_keys;
mod most_specific;

use super::{KlibCalleeOrigin, KlibDeclarationBodies};
use crate::fx_hash::HashSet;
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrClass, KlibIrMember, KlibIrType};
use crate::metadata::klib_ir::KlibIrSignature;

impl KlibDeclarationBodies {
    /// The one serialized declaration the fake override `declaration` inherits. Of several
    /// inherited declarations, only one with a body is the implementation it inherits; more than
    /// one is ambiguous. When none has a body, the fake override is abstract and stands for the
    /// most specific of them (`most_specific`).
    pub(super) fn inherited_origin(
        &self,
        body_module: &str,
        declaration: &KlibDeclarationSignature,
    ) -> KlibCalleeOrigin {
        let class = match self.member_class(body_module, declaration) {
            Ok(Some((arena, class, _))) => (arena, class),
            Ok(None) => return KlibCalleeOrigin::Missing,
            Err(()) => return KlibCalleeOrigin::Ambiguous,
        };
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
                    [] => self.most_specific(body_module, class, several),
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
        declaration: &KlibDeclarationSignature,
    ) -> Result<Vec<KlibDependencyCallable>, &'static str> {
        match self.callee_origin(body_module, declaration) {
            KlibCalleeOrigin::Exact(identity) => return Ok(vec![identity]),
            KlibCalleeOrigin::Ambiguous => {
                return Err("an overridden declaration has more than one producer")
            }
            KlibCalleeOrigin::Missing => {}
        }
        let found = self
            .inherited(body_module, declaration)
            .map_err(|()| "an overridden declaration's class is not unique")?;
        if found.is_empty() {
            return Err("an overridden declaration reaches no serialized declaration");
        }
        Ok(found)
    }

    /// The declarations an override edge of a body in `body_module` overrides, as
    /// [`Self::overridden_declarations`], and every declaration those override in turn:
    /// `EmptyList.isEmpty` overrides `List.isEmpty` and, through it, `Collection.isEmpty`.
    pub(crate) fn overridden_closure(
        &self,
        body_module: &str,
        declaration: &KlibDeclarationSignature,
    ) -> Result<Vec<KlibDependencyCallable>, &'static str> {
        let mut found = self.overridden_declarations(body_module, declaration)?;
        let mut next = 0;
        while let Some(identity) = found.get(next).cloned() {
            next += 1;
            let Some((_, function)) = self.function(&identity) else {
                continue;
            };
            for symbol in &function.overridden {
                let overridden = member_signature(&symbol.signature)
                    .ok_or("an override names a declaration that is not a member")?;
                for overridden in self.overridden_declarations(&identity.module, &overridden)? {
                    if !found.contains(&overridden) {
                        found.push(overridden);
                    }
                }
            }
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
        // Breadth first, each class's supertypes in their declared order: the order kotlinc's
        // fake-override builder meets the inherited declarations in.
        let mut pending = std::collections::VecDeque::from([declaration.clone()]);
        let mut visited = HashSet::default();
        while let Some(declaration) = pending.pop_front() {
            if !visited.insert(declaration.clone()) {
                continue;
            }
            // The root is the fake override itself, which no module serializes.
            match self.callee_origin(body_module, &declaration) {
                KlibCalleeOrigin::Exact(identity) => {
                    if !found.contains(&identity) {
                        found.push(identity);
                    }
                    continue;
                }
                KlibCalleeOrigin::Ambiguous => return Err(()),
                KlibCalleeOrigin::Missing => {}
            }
            let Some((arena, class, member)) = self.member_class(body_module, &declaration)? else {
                continue;
            };
            for &supertype in &class.supertypes {
                let KlibIrType::Simple { classifier, .. } = arena.ty(supertype) else {
                    continue;
                };
                let KlibIrSignature::Public(supertype) = &classifier.signature else {
                    continue;
                };
                pending.extend(as_member_of(&member, supertype));
            }
        }
        Ok(found)
    }

    /// The members the class `class` (serialized at `signature` in `arena`) inherits from its
    /// supertypes, direct or not, without overriding them, each with the one inherited
    /// declaration that has a body and the bodyless declarations it implements with it:
    /// `AbstractList` implements `List.contains` with `AbstractCollection.contains`, and a
    /// subclass of `AbstractMutableList` implements `List.isEmpty`, which it reaches only through
    /// `MutableList`, with `AbstractCollection.isEmpty`. An interface default that nothing else
    /// declares implements itself: `CombinedContext` implements `CoroutineContext.plus` with it.
    pub(crate) fn inherited_implementations(
        &self,
        body_module: &str,
        arena: &KlibIrArena,
        class: &KlibIrClass,
    ) -> Result<Vec<(KlibDependencyCallable, Vec<KlibDependencyCallable>)>, &'static str> {
        let key = |function| member_keys::function_key(self, body_module, function);
        let declared = class
            .members
            .iter()
            .flat_map(|member| member_functions(arena, member))
            .filter(|function| !function.constructor)
            .filter_map(key)
            .collect::<HashSet<_>>();
        let mut found: Vec<(KlibDependencyCallable, Vec<KlibDependencyCallable>)> = Vec::new();
        let mut met = HashSet::default();
        // Every supertype once, nearest first: a member a supertype redeclares is met through it
        // before the declaration it redeclares.
        let mut pending = class
            .supertypes
            .iter()
            .map(|&supertype| (arena, supertype))
            .collect::<std::collections::VecDeque<_>>();
        let mut visited = HashSet::default();
        while let Some((supertype_arena, supertype)) = pending.pop_front() {
            let Some((interface_arena, interface)) =
                self.supertype_class(body_module, supertype_arena, supertype, &mut visited)?
            else {
                continue;
            };
            pending.extend(
                interface
                    .supertypes
                    .iter()
                    .map(|&supertype| (interface_arena, supertype)),
            );
            for function in interface
                .members
                .iter()
                .flat_map(|member| member_functions(interface_arena, member))
                .filter(|function| !function.constructor)
            {
                let Some(member) = key(function) else {
                    continue;
                };
                if declared.contains(&member) || !met.insert(member.clone()) {
                    continue;
                }
                let declarations =
                    self.fake_override_closure(body_module, arena, class, &member)?;
                let mut bodies = declarations.iter().filter(|identity| {
                    member_keys::member_function(self, identity)
                        .is_some_and(|(_, function)| function.body.is_some())
                });
                let (Some(body), None) = (bodies.next(), bodies.next()) else {
                    continue;
                };
                let body = body.clone();
                let mut implemented = declarations
                    .into_iter()
                    .filter(|identity| *identity != body)
                    .collect::<Vec<_>>();
                // An interface default no other declaration overrides implements itself for the
                // class: the class's instances reach it through the interface it declares. A
                // body a superclass declares is inherited only for the declarations it implements.
                if implemented.is_empty() {
                    if crate::metadata::semantic::class_kind(interface.base.flags)
                        != crate::libraries::TypeKind::Interface
                    {
                        continue;
                    }
                    implemented.push(body.clone());
                }
                if !found.iter().any(|(known, _)| *known == body) {
                    found.push((body, implemented));
                }
            }
        }
        Ok(found)
    }

    /// The class the supertype `supertype` of a class serialized in `arena` names, unless an
    /// earlier step of the same walk met it: a public class of the producer graph rooted at
    /// `body_module`, or a class private to its file, which only that module serializes.
    fn supertype_class<'a>(
        &'a self,
        body_module: &str,
        arena: &'a KlibIrArena,
        supertype: crate::metadata::klib_ir::tree::KlibIrTypeId,
        visited: &mut HashSet<KlibIrSignature>,
    ) -> Result<Option<(&'a KlibIrArena, &'a KlibIrClass)>, &'static str> {
        let KlibIrType::Simple { classifier, .. } = arena.ty(supertype) else {
            return Ok(None);
        };
        if !visited.insert(classifier.signature.clone()) {
            return Ok(None);
        }
        match &classifier.signature {
            KlibIrSignature::Public(signature) => self
                .declaring_class_of(body_module, signature)
                .map_err(|()| "a supertype is serialized twice"),
            local @ KlibIrSignature::FileLocal { .. } => {
                Ok(self.module_local_class(body_module, local))
            }
            KlibIrSignature::Accessor(_) | KlibIrSignature::TypeParameter { .. } => Ok(None),
        }
    }

    /// The declarations the fake override of `member` in the class `class` (serialized in
    /// `arena`) overrides, and every declaration those override in turn. The fake override
    /// overrides, on each path through the class's supertypes, the first supertype's own
    /// declaration of the member, as kotlinc's fake-override builder does.
    fn fake_override_closure(
        &self,
        body_module: &str,
        arena: &KlibIrArena,
        class: &KlibIrClass,
        member: &member_keys::MemberKey,
    ) -> Result<Vec<KlibDependencyCallable>, &'static str> {
        let mut found = Vec::new();
        let mut pending = class
            .supertypes
            .iter()
            .map(|&supertype| (arena, supertype))
            .collect::<std::collections::VecDeque<_>>();
        let mut visited = HashSet::default();
        while let Some((supertype_arena, supertype)) = pending.pop_front() {
            let Some((declaring_arena, declaring)) =
                self.supertype_class(body_module, supertype_arena, supertype, &mut visited)?
            else {
                continue;
            };
            let mut declares = false;
            for function in declaring
                .members
                .iter()
                .flat_map(|candidate| member_functions(declaring_arena, candidate))
                .filter(|function| !function.constructor)
            {
                if member_keys::function_key(self, body_module, function).as_ref() != Some(member) {
                    continue;
                }
                declares = true;
                if let Some(identity) = member_keys::member_identity(
                    self,
                    body_module,
                    &function.base.symbol.signature,
                )? {
                    if !found.contains(&identity) {
                        found.push(identity);
                    }
                }
            }
            if !declares {
                pending.extend(
                    declaring
                        .supertypes
                        .iter()
                        .map(|&supertype| (declaring_arena, supertype)),
                );
            }
        }
        let mut next = 0;
        while let Some(identity) = found.get(next).cloned() {
            next += 1;
            let Some((_, function)) = member_keys::member_function(self, &identity) else {
                continue;
            };
            for symbol in &function.overridden {
                let overridden = match member_signature(&symbol.signature) {
                    Some(overridden) => {
                        self.overridden_declarations(&identity.module, &overridden)?
                    }
                    None => {
                        member_keys::member_identity(self, &identity.module, &symbol.signature)?
                            .into_iter()
                            .collect()
                    }
                };
                for overridden in overridden {
                    if !found.contains(&overridden) {
                        found.push(overridden);
                    }
                }
            }
        }
        Ok(found)
    }

    /// [`Self::inherited_implementations`] of the class serialized at `signature` in the producer
    /// graph rooted at `body_module`; a class private to its file is one `body_module` itself
    /// serializes.
    pub(crate) fn inherited_implementations_of(
        &self,
        body_module: &str,
        signature: &KlibIrSignature,
    ) -> Result<Vec<(KlibDependencyCallable, Vec<KlibDependencyCallable>)>, &'static str> {
        let class = match signature {
            KlibIrSignature::Public(public) => self
                .declaring_class_of(body_module, public)
                .map_err(|()| "a class is serialized twice")?,
            local @ KlibIrSignature::FileLocal { .. } => {
                self.module_local_class(body_module, local)
            }
            KlibIrSignature::Accessor(_) | KlibIrSignature::TypeParameter { .. } => None,
        };
        match class {
            Some((arena, class)) => self.inherited_implementations(body_module, arena, class),
            None => Ok(Vec::new()),
        }
    }

    /// The one class in the producer graph rooted at `body_module` serialized at `signature`.
    fn declaring_class_of(
        &self,
        body_module: &str,
        signature: &KlibPublicIdSignature,
    ) -> Result<Option<(&KlibIrArena, &KlibIrClass)>, ()> {
        let wanted = KlibIrSignature::Public(signature.clone());
        let mut found = None;
        for module in self.body_provider_modules(body_module) {
            let Some(&library) = self.modules.get(&module) else {
                continue;
            };
            for (arena, class) in self.libraries[library].classes_at_path(signature) {
                if class.base.symbol.signature == wanted && found.replace((arena, class)).is_some()
                {
                    return Err(());
                }
            }
        }
        Ok(found)
    }

    /// The class a member `declaration` of a body in `body_module` belongs to, with the arena
    /// that owns it and the member's public identity in that class: the one class serializing the
    /// member's owner, or, for a fake override of a class private to its file, that class. kotlinc
    /// names such a fake override `Composite(File, Public)` with the public identity it would have
    /// in a public class, so its member id is the one of the member it inherits.
    fn member_class(
        &self,
        body_module: &str,
        declaration: &KlibDeclarationSignature,
    ) -> Result<Option<(&KlibIrArena, &KlibIrClass, KlibDeclarationSignature)>, ()> {
        let KlibDeclarationSignature::FileLocal { file, .. } = declaration else {
            let Some(member) = declaring_member(declaration) else {
                return Ok(None);
            };
            return Ok(self
                .declaring_class(body_module, member)?
                .map(|(arena, class)| (arena, class, declaration.clone())));
        };
        let Some(&library) = self.modules.get(body_module) else {
            return Ok(None);
        };
        let library = &self.libraries[library];
        let signature = ir_signature(declaration);
        // A member the class declares is its own declaration, never a fake override.
        if library.local_function(&signature).is_some() {
            return Ok(None);
        }
        let Some(member) = library.file_private_identity(&signature) else {
            return Ok(None);
        };
        let Some(owner) = member.owner_path().filter(|_| member.member_id().is_some()) else {
            return Ok(None);
        };
        Ok(library
            .file_private_class_at(*file, &owner)
            .map(|(arena, class)| {
                (
                    arena,
                    class,
                    KlibDeclarationSignature::Public(member.clone()),
                )
            }))
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
            for class in self.libraries[library].classes_owning_member(member) {
                if found.replace(class).is_some() {
                    return Err(());
                }
            }
        }
        Ok(found)
    }
}

/// The declaration a member function or property accessor symbol names. Only these take part
/// in dispatch: a file-private declaration is never a member.
pub(crate) fn member_signature(signature: &KlibIrSignature) -> Option<KlibDeclarationSignature> {
    match signature {
        KlibIrSignature::Public(signature) => {
            Some(KlibDeclarationSignature::Public(signature.clone()))
        }
        KlibIrSignature::Accessor(signature) => {
            Some(KlibDeclarationSignature::Accessor(signature.clone()))
        }
        KlibIrSignature::FileLocal { .. } | KlibIrSignature::TypeParameter { .. } => None,
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

/// The functions a class member declares: itself, or a property's accessors.
fn member_functions<'a>(
    arena: &'a KlibIrArena,
    member: &KlibIrMember,
) -> Vec<&'a crate::metadata::klib_ir::tree::KlibIrFunction> {
    match member {
        KlibIrMember::Function(function) => vec![arena.function(*function)],
        KlibIrMember::Property(property) => [property.getter, property.setter]
            .into_iter()
            .flatten()
            .map(|function| arena.function(function))
            .collect(),
        _ => Vec::new(),
    }
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
