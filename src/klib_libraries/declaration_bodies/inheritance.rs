//! Members a class inherits without overriding them.
//!
//! A producer serializes no fake override: the linker rebuilds a member a class inherits without
//! overriding it, as kotlinc's fake-override builder does. A call symbol or an override edge
//! naming one stands for the members with the same member identity in the declaring class's
//! direct supertypes, followed until each reaches a serialized declaration. kotlinc's member ids
//! hash the member's own mangled signature, never its owner, so an inherited member keeps them in
//! every class.

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
        let found = self.overridden_declarations(body_module, declaration)?;
        self.override_closure(found)
    }

    /// `found` and every declaration its declarations override in turn.
    fn override_closure(
        &self,
        mut found: Vec<KlibDependencyCallable>,
    ) -> Result<Vec<KlibDependencyCallable>, &'static str> {
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
        self.inherited_through(
            body_module,
            std::collections::VecDeque::from([declaration.clone()]),
        )
    }

    /// Every serialized declaration the member `member` of the class `class` (in `arena`)
    /// inherits through each direct supertype of the class, as [`Self::inherited`] follows a fake
    /// override from its declaring class.
    fn inherited_in_class(
        &self,
        body_module: &str,
        arena: &KlibIrArena,
        class: &KlibIrClass,
        member: &KlibDeclarationSignature,
    ) -> Result<Vec<KlibDependencyCallable>, ()> {
        let pending = public_supertypes(arena, class)
            .filter_map(|supertype| as_member_of(member, supertype))
            .collect();
        self.inherited_through(body_module, pending)
    }

    /// Every serialized declaration the `pending` member declarations reach, each followed
    /// through the direct supertypes of its declaring class until a serialized one is reached.
    fn inherited_through(
        &self,
        body_module: &str,
        mut pending: std::collections::VecDeque<KlibDeclarationSignature>,
    ) -> Result<Vec<KlibDependencyCallable>, ()> {
        let mut found = Vec::new();
        // Breadth first, each class's supertypes in their declared order: the order kotlinc's
        // fake-override builder meets the inherited declarations in.
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
            pending.extend(
                public_supertypes(arena, class)
                    .filter_map(|supertype| as_member_of(&member, supertype)),
            );
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
        let Some(signature) = self.class_identity(body_module, &class.base.symbol.signature) else {
            return Ok(Vec::new());
        };
        let signature = &signature;
        // A member of a class private to its file is declared under its file-private signature,
        // compared here by the public identity it scopes to the file.
        let mut declared = HashSet::default();
        for function in class
            .members
            .iter()
            .flat_map(|member| member_functions(arena, member))
        {
            let declaration = &function.base.symbol.signature;
            match declaration {
                KlibIrSignature::FileLocal { .. } => {
                    match self.class_identity(body_module, declaration) {
                        Some(identity) => {
                            declared.insert(KlibIrSignature::Public(identity));
                        }
                        // A declaration that overrides nothing overrides no inherited member.
                        None if function.overridden.is_empty() => {}
                        // One of unknown identity could override any inherited member.
                        None => return Ok(Vec::new()),
                    }
                }
                _ => {
                    declared.insert(declaration.clone());
                }
            }
        }
        let mut found = Vec::new();
        // Every supertype once, nearest first: a member a supertype redeclares is met through it
        // before the declaration it redeclares.
        let mut pending = class
            .supertypes
            .iter()
            .map(|&supertype| (arena, supertype))
            .collect::<std::collections::VecDeque<_>>();
        let mut visited = HashSet::default();
        while let Some((supertype_arena, supertype)) = pending.pop_front() {
            let KlibIrType::Simple { classifier, .. } = supertype_arena.ty(supertype) else {
                continue;
            };
            let KlibIrSignature::Public(supertype) = &classifier.signature else {
                continue;
            };
            if !visited.insert(supertype.clone()) {
                continue;
            }
            let Some((interface_arena, interface)) = self
                .declaring_class_of(body_module, supertype)
                .map_err(|()| "a supertype is serialized twice")?
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
            {
                let Some(member) = member_signature(&function.base.symbol.signature) else {
                    continue;
                };
                let Some(inherited) = as_member_of(&member, signature) else {
                    continue;
                };
                if declared.contains(&ir_signature(&inherited)) {
                    continue;
                }
                // The class itself, not its signature, roots the walk: a class private to its
                // file is found by no signature of the member it inherits.
                let reached = self
                    .inherited_in_class(body_module, arena, class, &inherited)
                    .map_err(|()| "an overridden declaration's class is not unique")?;
                if reached.is_empty() {
                    return Err("an overridden declaration reaches no serialized declaration");
                }
                let declarations = self.override_closure(reached)?;
                let mut bodies = declarations
                    .iter()
                    .filter(|identity| self.function_has_body(identity) == Some(true));
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

    /// The public identity of the class or member declaration `signature` of a body in
    /// `body_module`: a public signature itself, or the identity a file-private one scopes to its
    /// file. `None` for an accessor or a file-local declaration of no public identity.
    fn class_identity(
        &self,
        body_module: &str,
        signature: &KlibIrSignature,
    ) -> Option<KlibPublicIdSignature> {
        match signature {
            KlibIrSignature::Public(signature) => Some(signature.clone()),
            KlibIrSignature::FileLocal { .. } => {
                let library = *self.modules.get(body_module)?;
                self.libraries[library]
                    .file_private_identity(signature)
                    .cloned()
            }
            // An accessor or type parameter signature names no class.
            _ => None,
        }
    }

    /// [`Self::inherited_implementations`] of the class serialized at `signature` in the producer
    /// graph rooted at `body_module`: a public class, or a class private to its file.
    pub(crate) fn inherited_implementations_of(
        &self,
        body_module: &str,
        signature: &KlibIrSignature,
    ) -> Result<Vec<(KlibDependencyCallable, Vec<KlibDependencyCallable>)>, &'static str> {
        let KlibIrSignature::Public(public) = signature else {
            return match self.module_local_class(body_module, signature) {
                Some((arena, class)) => self.inherited_implementations(body_module, arena, class),
                None => Ok(Vec::new()),
            };
        };
        match self
            .declaring_class_of(body_module, public)
            .map_err(|()| "a class is serialized twice")?
        {
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

/// The public classes `class` (in `arena`) names as direct supertypes, in declared order.
fn public_supertypes<'a>(
    arena: &'a KlibIrArena,
    class: &'a KlibIrClass,
) -> impl Iterator<Item = &'a KlibPublicIdSignature> {
    class.supertypes.iter().filter_map(|&supertype| {
        let KlibIrType::Simple { classifier, .. } = arena.ty(supertype) else {
            return None;
        };
        match &classifier.signature {
            KlibIrSignature::Public(supertype) => Some(supertype),
            _ => None,
        }
    })
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

/// The serialized symbol signature of the member declaration `declaration`.
fn ir_signature(declaration: &KlibDeclarationSignature) -> KlibIrSignature {
    match declaration {
        KlibDeclarationSignature::Public(signature) => KlibIrSignature::Public(signature.clone()),
        KlibDeclarationSignature::Accessor(accessor) => KlibIrSignature::Accessor(accessor.clone()),
        KlibDeclarationSignature::FileLocal { file, slot } => KlibIrSignature::FileLocal {
            file: *file,
            slot: *slot,
        },
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
