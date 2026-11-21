//! Members a local or anonymous class inherits without overriding them.
//!
//! A producer serializes no fake override, and gives the one a body of a local class reads a
//! file-local signature: the slot of a signature table entry that holds only the signature it is
//! nested in and an id. The id is the hash of the mangled callables the class is declared in
//! followed by the mangled member (`local_fake_override_id`), so it names the member without
//! naming its class: a call of the fake override stands for the member of a direct or indirect
//! supertype whose hash it is. The member is found among the supertypes' declarations, never by
//! its spelling, and a hash no inherited member produces is not a call this module can link.

use std::collections::VecDeque;

use super::{declaration_signature, KlibCalleeOrigin, KlibDeclarationBodies};
use crate::fx_hash::HashSet;
use crate::libraries::KlibDeclarationSignature;
use crate::metadata::id_signature::{local_fake_override_id, Accessor, LocalMember};
use crate::metadata::klib_ir::signature_types::{
    KlibMangleCallable, KlibMangleContainer, KlibMangleProperty, KlibMangleScope,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrClass, KlibIrFunction, KlibIrMember, KlibIrType,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};

const NO_SUCH_MEMBER: &str = "a file-local call names no member its class inherits";
const NOT_MANGLEABLE: &str =
    "a file-local call names a member whose declaration has types the signature mangler cannot spell";
const AMBIGUOUS_MEMBER: &str = "a file-local call names members of unrelated supertypes";
const PRIVATE_MEMBER: &str = "a file-local call names an inherited member private to its file";
const UNSUPPORTED_ENCLOSING: &str =
    "a local class is declared in something other than a function or an accessor";
const LOCAL_SUPERTYPE: &str = "a local class inherits from a file-local class";
const AMBIGUOUS_CLASS: &str = "a supertype of a local class is serialized twice";

/// What a file-local call names, by the shape of the signature nesting it.
enum Named {
    /// A function the local class inherits, identified by its hash.
    Function { id: u64 },
    /// An accessor of a property the local class inherits: the hash of the property, then of the
    /// accessor.
    Accessor { property: u64, id: u64 },
}

/// A declaration a local class is nested in.
enum Container<'a> {
    Class(&'a KlibIrArena, &'a KlibIrClass),
    Function(&'a KlibIrArena, &'a KlibIrFunction),
}

/// A class a local class inherits from, directly or not, with the type parameters of its
/// declarations replaced by what the local class's supertypes supply.
struct Supertype<'a> {
    arena: &'a KlibIrArena,
    class: &'a KlibIrClass,
    scope: KlibMangleScope,
}

impl KlibDeclarationBodies {
    /// The declaration the fake override `callee`, a file-local call of a body of `module`, is
    /// of: the member of a supertype of the local class it is a member of whose hash it carries.
    /// `None` when `callee` is not nested in a local class, which makes it no fake override.
    pub(crate) fn local_inherited_signature(
        &self,
        module: &str,
        callee: &KlibIrSignature,
    ) -> Result<Option<KlibIrSignature>, &'static str> {
        let &KlibIrSignature::FileLocal { file, slot } = callee else {
            return Ok(None);
        };
        let Some(&library) = self.modules.get(module) else {
            return Ok(None);
        };
        let trees = &self.libraries[library];
        // A serialized declaration is itself, not a fake override of one.
        if trees.local_function(callee).is_some() {
            return Ok(None);
        }
        let Some(origin) = trees.local_origin(file, slot) else {
            return Ok(None);
        };
        let (class, named) = if self.module_local_class(module, &origin.container).is_some() {
            (origin.container.clone(), Named::Function { id: origin.id })
        } else if let &KlibIrSignature::FileLocal {
            file: property_file,
            slot: property_slot,
        } = &origin.container
        {
            let Some(property) = trees.local_origin(property_file, property_slot) else {
                return Ok(None);
            };
            if self
                .module_local_class(module, &property.container)
                .is_none()
            {
                return Ok(None);
            }
            (
                property.container.clone(),
                Named::Accessor {
                    property: property.id,
                    id: origin.id,
                },
            )
        } else {
            return Ok(None);
        };
        let Some((arena, local_class)) = self.module_local_class(module, &class) else {
            return Ok(None);
        };
        let containers = self.containers_of(module, &class, arena, local_class)?;
        let scope =
            containers.iter().fold(
                KlibMangleScope::default(),
                |scope, container| match container {
                    Container::Class(_, class) => scope.declaring(&class.type_parameters),
                    Container::Function(_, function) => scope.declaring(&function.type_parameters),
                },
            );
        let containers = containers
            .iter()
            .rev()
            .map(|container| match container {
                Container::Class(arena, class) => KlibMangleContainer::class(&scope, arena, class),
                Container::Function(arena, function) => {
                    KlibMangleContainer::callable(&scope, arena, function)
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| NOT_MANGLEABLE)?;
        let enclosing = containers
            .iter()
            .map(KlibMangleContainer::shape)
            .collect::<Vec<_>>();

        let mut found: Vec<&KlibIrSignature> = Vec::new();
        let mut unspellable = false;
        for supertype in self.supertypes_of(module, arena, local_class, &scope)? {
            for member in &supertype.class.members {
                match (member, &named) {
                    (KlibIrMember::Function(function), Named::Function { id }) => {
                        let function = supertype.arena.function(*function);
                        if function.constructor || function.dispatch_receiver.is_none() {
                            continue;
                        }
                        let Ok(callable) =
                            KlibMangleCallable::of(&supertype.scope, supertype.arena, function)
                        else {
                            unspellable = true;
                            continue;
                        };
                        let hash = local_fake_override_id(
                            &enclosing,
                            &LocalMember::Callable(&callable.shape()),
                        );
                        if hash == Ok(*id) {
                            found.push(&function.base.symbol.signature);
                        }
                    }
                    (
                        KlibIrMember::Property(property),
                        Named::Accessor {
                            property: wanted,
                            id,
                        },
                    ) => {
                        let getter = property
                            .getter
                            .map(|getter| supertype.arena.function(getter));
                        let setter = property
                            .setter
                            .map(|setter| supertype.arena.function(setter));
                        let Ok(Some(mangled)) = KlibMangleProperty::of(
                            &supertype.scope,
                            supertype.arena,
                            &property.name,
                            getter,
                            setter,
                        ) else {
                            unspellable = true;
                            continue;
                        };
                        let shape = mangled.shape();
                        if local_fake_override_id(&enclosing, &LocalMember::Property(&shape))
                            != Ok(*wanted)
                        {
                            continue;
                        }
                        if let Some(getter) = getter {
                            let hash = local_fake_override_id(
                                &enclosing,
                                &LocalMember::Accessor(&shape, Accessor::Getter),
                            );
                            if hash == Ok(*id) {
                                found.push(&getter.base.symbol.signature);
                            }
                        }
                        if let (Some(setter), Some(value)) = (setter, mangled.setter_value()) {
                            let hash = local_fake_override_id(
                                &enclosing,
                                &LocalMember::Accessor(&shape, Accessor::Setter { value }),
                            );
                            if hash == Ok(*id) {
                                found.push(&setter.base.symbol.signature);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let Some(&nearest) = found.first() else {
            return Err(if unspellable {
                NOT_MANGLEABLE
            } else {
                NO_SUCH_MEMBER
            });
        };
        // The nearest supertype that declares the member is the one the fake override is of; a
        // declaration further out must be one it overrides.
        if found.iter().any(|other| *other != nearest) {
            let nearest_declaration = declaration_signature(nearest).ok_or(PRIVATE_MEMBER)?;
            let overridden = self
                .overridden_closure(module, &nearest_declaration)
                .map_err(|_| AMBIGUOUS_MEMBER)?;
            for other in &found {
                let other = declaration_signature(other).ok_or(PRIVATE_MEMBER)?;
                if !overridden
                    .iter()
                    .any(|identity| identity.declaration == other)
                {
                    return Err(AMBIGUOUS_MEMBER);
                }
            }
        }
        if declaration_signature(nearest).is_none() {
            return Err(PRIVATE_MEMBER);
        }
        Ok(Some(nearest.clone()))
    }

    /// The declarations the local class `class` is nested in, the class itself first and the
    /// outermost last. A class around a declaration has no part in the mangle but its type
    /// parameters are a scope.
    fn member_owner_class(
        &self,
        module: &str,
        declaration: &KlibDeclarationSignature,
    ) -> Result<Option<(&KlibIrArena, &KlibIrClass)>, &'static str> {
        let member = match declaration {
            KlibDeclarationSignature::Public(signature) => signature,
            KlibDeclarationSignature::Accessor(accessor) => accessor.property(),
            KlibDeclarationSignature::FileLocal { .. } => return Ok(None),
        };
        // A top-level declaration has no class around it.
        if member.declaration().segments().len() <= 1 {
            return Ok(None);
        }
        self.declaring_class(module, member)
            .map_err(|()| AMBIGUOUS_CLASS)
    }

    fn containers_of<'a>(
        &'a self,
        module: &str,
        class: &KlibIrSignature,
        arena: &'a KlibIrArena,
        local_class: &'a KlibIrClass,
    ) -> Result<Vec<Container<'a>>, &'static str> {
        let library = *self.modules.get(module).ok_or(UNSUPPORTED_ENCLOSING)?;
        let trees = &self.libraries[library];
        let container_of = |signature: &KlibIrSignature| match signature {
            &KlibIrSignature::FileLocal { file, slot } => trees
                .local_origin(file, slot)
                .map(|origin| origin.container.clone()),
            _ => None,
        };
        let mut chain = vec![Container::Class(arena, local_class)];
        let mut current = container_of(class);
        while let Some(signature) = current.take() {
            match &signature {
                KlibIrSignature::FileLocal { .. } => {
                    if let Some((arena, function)) = trees.local_function(&signature) {
                        chain.push(Container::Function(arena, function));
                    } else if let Some((arena, class)) = self.module_local_class(module, &signature)
                    {
                        chain.push(Container::Class(arena, class));
                    } else {
                        return Err(UNSUPPORTED_ENCLOSING);
                    }
                    current = container_of(&signature);
                }
                KlibIrSignature::Public(_) | KlibIrSignature::Accessor(_) => {
                    let declaration =
                        declaration_signature(&signature).ok_or(UNSUPPORTED_ENCLOSING)?;
                    if let KlibCalleeOrigin::Exact(identity) =
                        self.callee_origin(module, &declaration)
                    {
                        let (arena, function) =
                            self.function(&identity).ok_or(UNSUPPORTED_ENCLOSING)?;
                        chain.push(Container::Function(arena, function));
                        // A member of a class sits in it: its type parameters are in scope.
                        if let Some(owner) = self.member_owner_class(module, &declaration)? {
                            chain.push(Container::Class(owner.0, owner.1));
                        }
                    } else if let KlibIrSignature::Public(class) = &signature {
                        let (arena, class) = self
                            .declaring_class_of(module, class)
                            .map_err(|()| AMBIGUOUS_CLASS)?
                            .ok_or(UNSUPPORTED_ENCLOSING)?;
                        chain.push(Container::Class(arena, class));
                    } else {
                        return Err(UNSUPPORTED_ENCLOSING);
                    }
                }
            }
        }
        Ok(chain)
    }

    /// Every serialized class `class` inherits from, nearest first, each with the type
    /// parameters of its declarations bound to what its subclasses apply it to.
    fn supertypes_of<'a>(
        &'a self,
        module: &str,
        arena: &'a KlibIrArena,
        class: &'a KlibIrClass,
        scope: &KlibMangleScope,
    ) -> Result<Vec<Supertype<'a>>, &'static str> {
        let mut found = Vec::new();
        let mut seen = HashSet::default();
        let mut pending = VecDeque::from([(arena, class, scope.clone())]);
        while let Some((arena, class, scope)) = pending.pop_front() {
            for &supertype in &class.supertypes {
                let KlibIrType::Simple {
                    classifier,
                    arguments,
                    ..
                } = arena.ty(supertype)
                else {
                    continue;
                };
                if classifier.kind != KlibIrSymbolKind::Class {
                    continue;
                }
                let KlibIrSignature::Public(signature) = &classifier.signature else {
                    return Err(LOCAL_SUPERTYPE);
                };
                // A class no library serializes declares nothing to inherit, as `Any` does not.
                let Some((super_arena, super_class)) = self
                    .declaring_class_of(module, signature)
                    .map_err(|()| AMBIGUOUS_CLASS)?
                else {
                    continue;
                };
                if !seen.insert(classifier.signature.clone()) {
                    continue;
                }
                let arguments = arguments
                    .iter()
                    .map(|argument| scope.argument(arena, *argument))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| NOT_MANGLEABLE)?;
                let scope = KlibMangleScope::substituting(&super_class.type_parameters, arguments)
                    .map_err(|_| NOT_MANGLEABLE)?;
                pending.push_back((super_arena, super_class, scope.clone()));
                found.push(Supertype {
                    arena: super_arena,
                    class: super_class,
                    scope,
                });
            }
        }
        Ok(found)
    }
}
