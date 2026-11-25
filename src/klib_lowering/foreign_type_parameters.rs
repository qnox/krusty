//! Type parameters a serialized body names but none of its declarations declares.
//!
//! A compiler plugin may write a type that names another public class's own type parameter, as
//! atomicfu's `AtomicArray<T>` field types name `kotlin.concurrent.AtomicArray`'s `T`. Kotlin
//! links such a symbol to that class's declaration like any other. It is that class's formal,
//! with the bounds the class declares for it; it is substituted nowhere, so a body only carries
//! it.

use std::sync::{Mutex, OnceLock};

use super::decline::KlibBodyDeclineReason;
use super::klib_types::{semantic_type, KlibTypeParameters};
use crate::fx_hash::HashMap;
use crate::klib_libraries::KlibDeclarationBodies;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::KlibIrArena;
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::Ty;

/// The formal identity minted for each (class, ordinal), as a provider mints one per declaration.
type MintedFormals = HashMap<(KlibPublicIdSignature, u32), &'static str>;

static FOREIGN_FORMALS: OnceLock<Mutex<MintedFormals>> = OnceLock::new();

/// `types` with every class type parameter the types of `arena` name that `types` leaves unbound
/// bound to that class's formal. A type parameter of a declaration other than a public class is
/// left unbound, so its first use still declines.
pub(super) fn bind(
    types: &mut KlibTypeParameters,
    arena: &KlibIrArena,
    bodies: &KlibDeclarationBodies,
    module: &str,
) -> Result<(), KlibBodyDeclineReason> {
    for signature in arena.classifier_signatures_in_types() {
        let KlibIrSignature::TypeParameter { owner, index } = signature else {
            continue;
        };
        let symbol = KlibIrSymbol {
            kind: KlibIrSymbolKind::TypeParameter,
            signature: signature.clone(),
        };
        if types.binds(&symbol) {
            continue;
        }
        let Some((owner_arena, parameter)) =
            bodies.public_class_type_parameter(module, owner, *index)
        else {
            continue;
        };
        if parameter.base.symbol != symbol {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a class type parameter is serialized under another identity".to_owned(),
            ));
        }
        let name = *FOREIGN_FORMALS
            .get_or_init(|| Mutex::new(HashMap::default()))
            .lock()
            .unwrap()
            .entry((owner.clone(), *index))
            .or_insert_with(|| crate::types::metadata_type_parameter(&parameter.name));
        // The bounds are read with only this formal in scope, so an F-bound such as `Enum`'s
        // `E : Enum<E>` reads; a bound naming another formal of its owner leaves this one
        // unbound, and only a use of it declines.
        let mut scope = KlibTypeParameters::default();
        scope.bind(
            &symbol,
            Ty::ty_param(name, Ty::nullable(Ty::obj("kotlin/Any"))),
        );
        let Ok(mut bounds) = parameter
            .supertypes
            .iter()
            .map(|bound| semantic_type(owner_arena, &scope, *bound))
            .collect::<Result<Vec<_>, _>>()
        else {
            continue;
        };
        if bounds.is_empty() {
            bounds.push(Ty::nullable(Ty::obj("kotlin/Any")));
        }
        let formal = Ty::ty_param(name, bounds[0]);
        if !types.bind(&symbol, formal) || !types.record_bounds(formal, bounds) {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a class type parameter is bound twice".to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::klib_ir::tree::{
        KlibIrClass, KlibIrDeclarationBase, KlibIrMember, KlibIrNullability, KlibIrType,
        KlibIrTypeArgument, KlibIrTypeId, KlibIrTypeParameter, KlibIrVariance,
    };
    use crate::metadata::klib_ir::{KlibIrDeclarationTree, KlibIrModuleTrees};
    use crate::types::TyKind;

    fn atomic_array() -> KlibPublicIdSignature {
        KlibPublicIdSignature::classifier(crate::types::type_name("kotlin/concurrent/AtomicArray"))
    }

    fn base(symbol: KlibIrSymbol) -> KlibIrDeclarationBase {
        KlibIrDeclarationBase {
            symbol,
            origin: "DEFINED".to_owned(),
            flags: 0,
            annotations: Vec::new(),
        }
    }

    fn parameter(owner: KlibPublicIdSignature, index: u32) -> KlibIrSymbol {
        KlibIrSymbol {
            kind: KlibIrSymbolKind::TypeParameter,
            signature: KlibIrSignature::TypeParameter { owner, index },
        }
    }

    fn simple(
        arena: &mut KlibIrArena,
        classifier: KlibIrSymbol,
        arguments: Vec<KlibIrTypeArgument>,
    ) -> KlibIrTypeId {
        arena.push_type(KlibIrType::Simple {
            classifier,
            nullability: KlibIrNullability::NotSpecified,
            arguments,
            annotations: Vec::new(),
            abbreviation: None,
        })
    }

    /// The stdlib declaring `class AtomicArray<T>`, with no other member.
    fn stdlib() -> KlibDeclarationBodies {
        library(atomic_array(), "AtomicArray", false)
    }

    /// A library declaring only the class `owner` with one type parameter `T`, bounded by
    /// `owner<T>` when `self_bounded`.
    fn library(
        owner: KlibPublicIdSignature,
        name: &str,
        self_bounded: bool,
    ) -> KlibDeclarationBodies {
        let mut arena = KlibIrArena::default();
        let supertypes = if self_bounded {
            let argument = simple(&mut arena, parameter(owner.clone(), 0), Vec::new());
            let class = KlibIrSymbol {
                kind: KlibIrSymbolKind::Class,
                signature: KlibIrSignature::Public(owner.clone()),
            };
            vec![simple(
                &mut arena,
                class,
                vec![KlibIrTypeArgument::Type {
                    variance: KlibIrVariance::Invariant,
                    ty: argument,
                }],
            )]
        } else {
            Vec::new()
        };
        let class = arena.push_class(KlibIrClass {
            base: base(KlibIrSymbol {
                kind: KlibIrSymbolKind::Class,
                signature: KlibIrSignature::Public(owner.clone()),
            }),
            name: name.to_owned(),
            this_receiver: None,
            type_parameters: vec![KlibIrTypeParameter {
                base: base(parameter(owner, 0)),
                name: "T".to_owned(),
                supertypes,
            }],
            supertypes: Vec::new(),
            members: Vec::new(),
            value_class_representation: None,
            sealed_subclasses: Vec::new(),
        });
        let trees = KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
            arena,
            declaration: KlibIrMember::Class(class),
        }])
        .expect("one class");
        KlibDeclarationBodies::from_libraries(vec![("stdlib".to_owned(), trees)])
            .expect("one module")
    }

    /// A consumer body's `AtomicArray<T>` whose argument is the class's own `T`, as atomicfu
    /// writes the type of a field it generates.
    fn consumer(owner: KlibPublicIdSignature) -> (KlibIrArena, KlibIrTypeId) {
        application_of(atomic_array(), owner)
    }

    /// A body's `class<T>` whose argument is the type parameter 0 of `owner`.
    fn application_of(
        class: KlibPublicIdSignature,
        owner: KlibPublicIdSignature,
    ) -> (KlibIrArena, KlibIrTypeId) {
        let mut arena = KlibIrArena::default();
        let argument = simple(&mut arena, parameter(owner, 0), Vec::new());
        let class = KlibIrSymbol {
            kind: KlibIrSymbolKind::Class,
            signature: KlibIrSignature::Public(class),
        };
        let application = simple(
            &mut arena,
            class,
            vec![KlibIrTypeArgument::Type {
                variance: KlibIrVariance::Invariant,
                ty: argument,
            }],
        );
        (arena, application)
    }

    #[test]
    fn a_foreign_class_type_parameter_is_that_class_formal_with_its_declared_bound() {
        let bodies = stdlib();
        let (arena, application) = consumer(atomic_array());
        let mut types = KlibTypeParameters::default();
        bind(&mut types, &arena, &bodies, "stdlib").expect("binds the foreign formal");
        let ty = semantic_type(&arena, &types, application).expect("a class application");
        let TyKind::Obj(name, arguments) = ty.kind() else {
            panic!("an application of AtomicArray, not {ty:?}");
        };
        assert_eq!(
            name,
            crate::types::type_name("kotlin/concurrent/AtomicArray")
        );
        let [argument] = arguments.as_slice() else {
            panic!("one type argument, not {arguments:?}");
        };
        let TyKind::TyParam(_, bound) = argument.kind() else {
            panic!("the class formal, not {argument:?}");
        };
        assert_eq!(bound, Ty::nullable(Ty::obj("kotlin/Any")));
        // A second body naming the same formal names the same identity.
        let mut again = KlibTypeParameters::default();
        bind(&mut again, &arena, &bodies, "stdlib").expect("binds the foreign formal");
        assert_eq!(semantic_type(&arena, &again, application), Ok(ty));
    }

    #[test]
    fn a_self_bounded_foreign_formal_reads_its_bound_with_itself_in_scope() {
        let ordered = KlibPublicIdSignature::classifier(crate::types::type_name("fixture/Ordered"));
        let bodies = library(ordered.clone(), "Ordered", true);
        let (arena, application) = application_of(ordered.clone(), ordered);
        let mut types = KlibTypeParameters::default();
        bind(&mut types, &arena, &bodies, "stdlib").expect("binds the foreign formal");
        let ty = semantic_type(&arena, &types, application).expect("a class application");
        let TyKind::Obj(_, arguments) = ty.kind() else {
            panic!("an application of Ordered, not {ty:?}");
        };
        let TyKind::TyParam(_, bound) = arguments[0].kind() else {
            panic!("the class formal, not {:?}", arguments[0]);
        };
        let TyKind::Obj(name, _) = bound.kind() else {
            panic!("an `Ordered` bound, not {bound:?}");
        };
        assert_eq!(name, crate::types::type_name("fixture/Ordered"));
    }

    #[test]
    fn a_type_parameter_of_an_undeclared_owner_stays_unbound() {
        let bodies = stdlib();
        let missing = KlibPublicIdSignature::classifier(crate::types::type_name("fixture/Missing"));
        let (arena, application) = consumer(missing);
        let mut types = KlibTypeParameters::default();
        bind(&mut types, &arena, &bodies, "stdlib").expect("leaves it unbound");
        assert_eq!(
            semantic_type(&arena, &types, application),
            Err(KlibBodyDeclineReason::UnsupportedType(
                "a type parameter its declaration does not bind"
            ))
        );
    }
}
