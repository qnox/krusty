//! The classes of one decoded KLIB, indexed by the identities serialized bodies look them up by.
//!
//! A body's dependency closure asks, for one signature at a time, which class has it, declares a
//! function or enum entry under it, or owns a member named by it. Each question has an exact
//! answer over every class in the module; the index records those answers once, when the module
//! is decoded, so a question costs a map access rather than a walk over every serialized class.
//!
//! The classes are the ones a walk from each tree's top-level declaration reaches through class
//! members and enum-entry classes. The lookup of a member's owner sees only the classes reached
//! through class members, not an enum-entry class or anything nested in one.

use crate::fx_hash::HashMap;
use crate::metadata::id_signature::{KlibDeclarationPath, KlibPublicIdSignature};

use super::symbols::{KlibIrSignature, KlibIrSymbolKind};
use super::tree::{KlibIrClassId, KlibIrFunctionId, KlibIrMember};
use super::KlibIrDeclarationTree;

/// One class: the tree whose arena owns it, and its id in that arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ClassAt {
    pub(super) tree: usize,
    pub(super) class: KlibIrClassId,
}

#[derive(Debug, Default)]
pub(super) struct KlibIrClassIndex {
    /// Every class by its own serialized signature.
    by_signature: HashMap<KlibIrSignature, Vec<ClassAt>>,
    /// Every class by each function it declares directly: a member function or constructor, or
    /// a property accessor. A class appears once per signature, with the first such function.
    by_function: HashMap<KlibIrSignature, Vec<(ClassAt, KlibIrFunctionId)>>,
    /// Every class by each enum entry it declares directly.
    by_enum_entry: HashMap<KlibIrSignature, Vec<ClassAt>>,
    /// Every public class outside an enum entry by its paths, the key its members' owner paths
    /// name.
    by_owner_path: HashMap<KlibDeclarationPath, Vec<ClassAt>>,
}

impl KlibIrClassIndex {
    pub(super) fn of(trees: &[KlibIrDeclarationTree]) -> Self {
        let mut index = Self::default();
        for (tree_index, tree) in trees.iter().enumerate() {
            let mut pending = vec![(&tree.declaration, false)];
            while let Some((member, in_enum_entry)) = pending.pop() {
                let (class, in_enum_entry) = match member {
                    KlibIrMember::Class(id) => (*id, in_enum_entry),
                    KlibIrMember::EnumEntry(entry) => match entry.class {
                        Some(id) => (id, true),
                        None => continue,
                    },
                    _ => continue,
                };
                let at = ClassAt {
                    tree: tree_index,
                    class,
                };
                let class = tree.arena.class(class);
                pending.extend(class.members.iter().map(|member| (member, in_enum_entry)));
                index
                    .by_signature
                    .entry(class.base.symbol.signature.clone())
                    .or_default()
                    .push(at);
                if let (false, KlibIrSignature::Public(signature)) =
                    (in_enum_entry, &class.base.symbol.signature)
                {
                    index
                        .by_owner_path
                        .entry(signature.declaration_path())
                        .or_default()
                        .push(at);
                }
                for member in &class.members {
                    let functions = match member {
                        KlibIrMember::Function(id) => [Some(*id), None],
                        KlibIrMember::Property(property) => [property.getter, property.setter],
                        KlibIrMember::EnumEntry(entry) => {
                            if entry.base.symbol.kind == KlibIrSymbolKind::EnumEntry {
                                let classes = index
                                    .by_enum_entry
                                    .entry(entry.base.symbol.signature.clone())
                                    .or_default();
                                if classes.last() != Some(&at) {
                                    classes.push(at);
                                }
                            }
                            continue;
                        }
                        _ => continue,
                    };
                    for function in functions.into_iter().flatten() {
                        let classes = index
                            .by_function
                            .entry(tree.arena.function(function).base.symbol.signature.clone())
                            .or_default();
                        if classes.last().is_none_or(|(declaring, _)| *declaring != at) {
                            classes.push((at, function));
                        }
                    }
                }
            }
        }
        index
    }

    pub(super) fn with_signature(&self, signature: &KlibIrSignature) -> &[ClassAt] {
        self.by_signature.get(signature).map_or(&[], Vec::as_slice)
    }

    pub(super) fn declaring_function(
        &self,
        signature: &KlibIrSignature,
    ) -> &[(ClassAt, KlibIrFunctionId)] {
        self.by_function.get(signature).map_or(&[], Vec::as_slice)
    }

    pub(super) fn declaring_enum_entry(&self, signature: &KlibIrSignature) -> &[ClassAt] {
        self.by_enum_entry.get(signature).map_or(&[], Vec::as_slice)
    }

    pub(super) fn at_path(&self, class: &KlibPublicIdSignature) -> &[ClassAt] {
        self.by_owner_path
            .get(&class.declaration_path())
            .map_or(&[], Vec::as_slice)
    }

    pub(super) fn owning_member(&self, member: &KlibPublicIdSignature) -> &[ClassAt] {
        member
            .owner_path()
            .and_then(|owner| self.by_owner_path.get(&owner))
            .map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::id_signature::{
        callable_signature, class_signature, CallableShape, ClassScope, DeclarationContainer,
        Placement,
    };
    use crate::metadata::klib_ir::tree::{
        KlibIrArena, KlibIrClass, KlibIrDeclarationBase, KlibIrEnumEntry, KlibIrFunction,
        KlibIrProperty, KlibIrType,
    };
    use crate::metadata::klib_ir::KlibIrSymbol;
    use crate::metadata::semantic::KotlinType;

    fn scope(name: &str) -> ClassScope<'_, KotlinType> {
        ClassScope {
            name,
            type_parameters: Vec::new(),
            expect: false,
        }
    }

    fn container<'a>(
        package: &'a [String],
        classes: &'a [ClassScope<'a, KotlinType>],
    ) -> DeclarationContainer<'a, KotlinType> {
        DeclarationContainer {
            package,
            classes,
            native_interop_library: false,
        }
    }

    /// The public signature of the class nested at `path` in package `fixture`.
    fn class_path(path: &[&str]) -> KlibPublicIdSignature {
        let package = ["fixture".to_owned()];
        let (name, outer) = path.split_last().expect("a class has a name");
        let outer = outer.iter().map(|name| scope(name)).collect::<Vec<_>>();
        class_signature(container(&package, &outer), &scope(name))
    }

    /// The signature of a parameterless function `name` declared in the class at `path`.
    fn member(path: &[&str], name: &str) -> KlibPublicIdSignature {
        let package = ["fixture".to_owned()];
        let classes = path.iter().map(|name| scope(name)).collect::<Vec<_>>();
        callable_signature(
            container(&package, &classes),
            &CallableShape {
                name,
                contexts: Vec::new(),
                receiver: None,
                params: Vec::new(),
                vararg: None,
                type_parameters: Vec::new(),
                expect: false,
                placement: Placement::Ordinary,
            },
        )
        .expect("a parameterless function is signable")
    }

    fn base(kind: KlibIrSymbolKind, signature: KlibPublicIdSignature) -> KlibIrDeclarationBase {
        KlibIrDeclarationBase {
            symbol: KlibIrSymbol {
                kind,
                signature: KlibIrSignature::Public(signature),
            },
            origin: "DEFINED".to_owned(),
            flags: 0,
            annotations: Vec::new(),
        }
    }

    fn class(
        arena: &mut KlibIrArena,
        signature: KlibPublicIdSignature,
        members: Vec<KlibIrMember>,
    ) -> KlibIrClassId {
        arena.push_class(KlibIrClass {
            base: base(KlibIrSymbolKind::Class, signature),
            name: String::new(),
            this_receiver: None,
            type_parameters: Vec::new(),
            supertypes: Vec::new(),
            members,
            value_class_representation: None,
            sealed_subclasses: Vec::new(),
        })
    }

    fn function(arena: &mut KlibIrArena, signature: KlibPublicIdSignature) -> KlibIrFunctionId {
        let return_type = arena.push_type(KlibIrType::Error {
            annotations: Vec::new(),
        });
        arena.push_function(KlibIrFunction {
            base: base(KlibIrSymbolKind::Function, signature),
            name: String::new(),
            constructor: false,
            type_parameters: Vec::new(),
            dispatch_receiver: None,
            context_parameters: Vec::new(),
            extension_receiver: None,
            regular_parameters: Vec::new(),
            return_type,
            overridden: Vec::new(),
            companion_extension_class: None,
            prepared_inline_file: None,
            body: None,
        })
    }

    /// `enum class Color { RED { class Inner { fun g() } }; class Nested { fun f(); val p } }`,
    /// where the getter of `p` is serialized under `Nested.p`'s own signature.
    fn tree() -> (KlibIrDeclarationTree, KlibIrFunctionId, KlibIrFunctionId) {
        let mut arena = KlibIrArena::default();
        let f = function(&mut arena, member(&["Color", "Nested"], "f"));
        let getter = function(&mut arena, member(&["Color", "Nested"], "p"));
        let nested = class(
            &mut arena,
            class_path(&["Color", "Nested"]),
            vec![
                KlibIrMember::Function(f),
                KlibIrMember::Property(KlibIrProperty {
                    base: base(
                        KlibIrSymbolKind::Property,
                        member(&["Color", "Nested"], "p"),
                    ),
                    name: "p".to_owned(),
                    backing_field: None,
                    getter: Some(getter),
                    setter: None,
                }),
            ],
        );
        let g = function(&mut arena, member(&["Color", "RED", "Inner"], "g"));
        let inner = class(
            &mut arena,
            class_path(&["Color", "RED", "Inner"]),
            vec![KlibIrMember::Function(g)],
        );
        let entry_class = class(
            &mut arena,
            class_path(&["Color", "RED"]),
            vec![KlibIrMember::Class(inner)],
        );
        let color = class(
            &mut arena,
            class_path(&["Color"]),
            vec![
                KlibIrMember::EnumEntry(KlibIrEnumEntry {
                    base: base(KlibIrSymbolKind::EnumEntry, class_path(&["Color", "RED"])),
                    name: "RED".to_owned(),
                    initializer: None,
                    class: Some(entry_class),
                }),
                KlibIrMember::Class(nested),
            ],
        );
        let tree = KlibIrDeclarationTree {
            arena,
            declaration: KlibIrMember::Class(color),
        };
        (tree, f, g)
    }

    fn at(class: KlibIrClassId) -> ClassAt {
        ClassAt { tree: 0, class }
    }

    fn class_id(tree: &KlibIrDeclarationTree, signature: KlibPublicIdSignature) -> KlibIrClassId {
        let signature = KlibIrSignature::Public(signature);
        let index = tree
            .arena
            .classes
            .iter()
            .position(|class| class.base.symbol.signature == signature)
            .expect("the fixture declares the class");
        KlibIrClassId(u32::try_from(index).expect("arena fits u32"))
    }

    #[test]
    fn every_reachable_class_is_found_by_its_own_signature() {
        let (tree, _, _) = tree();
        let index = KlibIrClassIndex::of(std::slice::from_ref(&tree));
        for path in [
            &["Color"][..],
            &["Color", "Nested"],
            &["Color", "RED"],
            &["Color", "RED", "Inner"],
        ] {
            let signature = KlibIrSignature::Public(class_path(path));
            assert_eq!(
                index.with_signature(&signature),
                [at(class_id(&tree, class_path(path)))],
                "{path:?}"
            );
        }
    }

    #[test]
    fn a_class_is_found_once_by_each_function_and_accessor_it_declares() {
        let (tree, f, g) = tree();
        let index = KlibIrClassIndex::of(std::slice::from_ref(&tree));
        let nested = at(class_id(&tree, class_path(&["Color", "Nested"])));
        let inner = at(class_id(&tree, class_path(&["Color", "RED", "Inner"])));
        let getter = tree
            .arena
            .functions
            .iter()
            .position(|function| {
                function.base.symbol.signature
                    == KlibIrSignature::Public(member(&["Color", "Nested"], "p"))
            })
            .map(|index| KlibIrFunctionId(u32::try_from(index).expect("arena fits u32")))
            .expect("the fixture declares the getter");
        let declaring = |signature: KlibPublicIdSignature| {
            index
                .declaring_function(&KlibIrSignature::Public(signature))
                .to_vec()
        };
        assert_eq!(declaring(member(&["Color", "Nested"], "f")), [(nested, f)]);
        assert_eq!(
            declaring(member(&["Color", "Nested"], "p")),
            [(nested, getter)]
        );
        assert_eq!(
            declaring(member(&["Color", "RED", "Inner"], "g")),
            [(inner, g)]
        );
        assert_eq!(declaring(member(&["Color"], "f")), []);
    }

    #[test]
    fn an_enum_entry_is_found_in_the_class_that_declares_it() {
        let (tree, _, _) = tree();
        let index = KlibIrClassIndex::of(std::slice::from_ref(&tree));
        let color = at(class_id(&tree, class_path(&["Color"])));
        assert_eq!(
            index.declaring_enum_entry(&KlibIrSignature::Public(class_path(&["Color", "RED"]))),
            [color]
        );
    }

    #[test]
    fn a_member_owner_is_a_class_outside_every_enum_entry() {
        let (tree, _, _) = tree();
        let index = KlibIrClassIndex::of(std::slice::from_ref(&tree));
        let nested = at(class_id(&tree, class_path(&["Color", "Nested"])));
        let color = at(class_id(&tree, class_path(&["Color"])));
        assert_eq!(
            index.owning_member(&member(&["Color", "Nested"], "f")),
            [nested]
        );
        assert_eq!(index.owning_member(&member(&["Color"], "values")), [color]);
        assert_eq!(index.owning_member(&member(&["Color", "RED"], "h")), []);
        assert_eq!(
            index.owning_member(&member(&["Color", "RED", "Inner"], "g")),
            []
        );
    }

    #[test]
    fn a_class_path_names_the_classes_outside_every_enum_entry() {
        let (tree, _, _) = tree();
        let index = KlibIrClassIndex::of(std::slice::from_ref(&tree));
        for path in [&["Color"][..], &["Color", "Nested"]] {
            assert_eq!(
                index.at_path(&class_path(path)),
                [at(class_id(&tree, class_path(path)))],
                "{path:?}"
            );
        }
        assert_eq!(index.at_path(&class_path(&["Color", "RED"])), []);
        assert_eq!(index.at_path(&class_path(&["Color", "RED", "Inner"])), []);
        assert_eq!(index.at_path(&class_path(&["Color", "Missing"])), []);
    }
}
