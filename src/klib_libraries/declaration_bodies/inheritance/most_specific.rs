//! The most specific of several bodyless declarations a class inherits for one member.
//!
//! When no inherited declaration has a body, the fake override is abstract and stands for the
//! declaration kotlinc's fake-override builder selects: the first, in inheritance order, whose
//! result type is a subtype of every other's. The results are compared as the inheriting class
//! sees them, in the shared semantic type model: each serialized header is normalized to its
//! semantic type, each declaring class's type parameters are substituted through the applied
//! hierarchy that reaches it, and the comparison is the one subtyping relation, with each
//! classifier's declaration-site variance and the results' use-site projections.
//! `Reader<List<E>>.read(): List<E>?` is less specific than `Strict<E>.read(): List<E>`, and
//! `Cov<String>` is more specific than `Cov<CharSequence>` for `interface Cov<out T>`. A type
//! parameter's declared bounds take part: `Cov<T>` for `T : Sub` is more specific than `Cov<Base>`.

use crate::assignable::{TyCtx, TypeOracle};
use crate::fx_hash::HashMap;
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::semantic_types::{
    classifier_name, semantic_type, KlibTypeParameters,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrClass, KlibIrFunction, KlibIrType, KlibIrTypeArgument, KlibIrTypeId,
    KlibIrTypeParameter, KlibIrVariance,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::{Ty, TypeName, TypeParameters, TypeVariance};

use super::super::{KlibCalleeOrigin, KlibDeclarationBodies};

impl KlibDeclarationBodies {
    /// The most specific of the bodyless `inherited` declarations of a member of the class
    /// `class`, which inherits them all.
    pub(super) fn most_specific(
        &self,
        body_module: &str,
        class: (&KlibIrArena, &KlibIrClass),
        inherited: &[KlibDependencyCallable],
    ) -> KlibCalleeOrigin {
        let declarations = inherited
            .iter()
            .map(|identity| self.function(identity))
            .collect::<Option<Vec<_>>>();
        let Some(declarations) = declarations else {
            return KlibCalleeOrigin::Ambiguous;
        };
        let Some(hierarchy) = SerializedHierarchy::of(self, body_module, class, &declarations)
        else {
            return KlibCalleeOrigin::Ambiguous;
        };
        let results = inherited
            .iter()
            .zip(&declarations)
            .map(|(identity, &declaration)| hierarchy.inherited_result(identity, declaration))
            .collect::<Option<Vec<_>>>();
        let Some(results) = results else {
            return KlibCalleeOrigin::Ambiguous;
        };
        let cx = TyCtx::new();
        // Of several equally specific declarations, such as `AbstractCollection.size` and
        // `List.size`, kotlinc keeps the first it inherits.
        let candidates = inherited.iter().zip(&results).collect::<Vec<_>>();
        crate::assignable::most_specific_results(&candidates, |(_, &candidate), (_, &other)| {
            crate::assignable::is_subtype(&cx, &hierarchy, candidate, other)
        })
        .first()
        .map_or(KlibCalleeOrigin::Ambiguous, |&(one, _)| {
            KlibCalleeOrigin::Exact(one.clone())
        })
    }
}

/// One serialized class as the shared class model sees it: its type parameters, each a semantic
/// type parameter of its own, and its direct supertypes naming them.
struct SerializedClass {
    signature: KlibPublicIdSignature,
    formals: TypeParameters<Vec<Vec<Ty>>>,
    /// The serialized symbols of the class's type parameters, bound to [`Self::formals`].
    parameters: KlibTypeParameters,
    /// The class applied to its own type parameters.
    applied: Ty,
    supertypes: Vec<Ty>,
}

/// The serialized classes the inheriting class and its inherited results name, normalized into
/// the shared class model the subtyping relation walks.
struct SerializedHierarchy {
    /// The inheriting class applied to its own type parameters.
    inheriting: Ty,
    classes: HashMap<TypeName, SerializedClass>,
    /// The declared upper bounds of each type parameter the classes name.
    bounds: HashMap<&'static str, Vec<Ty>>,
    /// The member's own type parameters by position, which every inherited declaration of the
    /// member declares in the same order.
    member: Vec<Ty>,
}

impl SerializedHierarchy {
    /// The classes `class` reaches and the inherited `declarations` name. `None` when one of them
    /// is serialized more than once, is not serialized, or declares a type the semantic model
    /// does not represent.
    fn of(
        bodies: &KlibDeclarationBodies,
        body_module: &str,
        (arena, class): (&KlibIrArena, &KlibIrClass),
        declarations: &[(&KlibIrArena, &KlibIrFunction)],
    ) -> Option<Self> {
        let KlibIrSignature::Public(signature) = &class.base.symbol.signature else {
            return None;
        };
        let own = declarations
            .iter()
            .map(|(_, function)| function.type_parameters.len())
            .max()
            .unwrap_or(0);
        let mut hierarchy = Self {
            inheriting: Ty::Unit,
            classes: HashMap::default(),
            bounds: HashMap::default(),
            member: (0..own)
                .map(|_| {
                    Ty::ty_param(
                        crate::types::metadata_type_parameter("T"),
                        Ty::nullable(Ty::obj("kotlin/Any")),
                    )
                })
                .collect(),
        };
        let root = hierarchy.normalize(signature.clone(), arena, class)?;
        hierarchy.inheriting = hierarchy.classes[&root].applied;
        let mut pending = vec![(arena, class)];
        for &(arena, function) in declarations {
            hierarchy.reach(
                bodies,
                body_module,
                arena,
                function.return_type,
                &mut pending,
            )?;
        }
        // A class's supertypes and the bounds of its type parameters both relate its classifiers
        // to others: `T : Sub` makes `T` a subtype of whatever `Sub` is.
        while let Some((arena, class)) = pending.pop() {
            let bounds = class
                .type_parameters
                .iter()
                .flat_map(|parameter| &parameter.supertypes);
            for &ty in class.supertypes.iter().chain(bounds) {
                hierarchy.reach(bodies, body_module, arena, ty, &mut pending)?;
            }
        }
        Some(hierarchy)
    }

    /// Record every class the serialized type `ty` of `arena` names, queueing each new one to
    /// have its own supertypes recorded.
    fn reach<'a>(
        &mut self,
        bodies: &'a KlibDeclarationBodies,
        body_module: &str,
        arena: &KlibIrArena,
        ty: KlibIrTypeId,
        pending: &mut Vec<(&'a KlibIrArena, &'a KlibIrClass)>,
    ) -> Option<()> {
        classifiers(arena, ty, &mut |classifier| {
            if self
                .classes
                .contains_key(&classifier_name(classifier).ok()?)
            {
                return Some(());
            }
            let (arena, class) = bodies.declaring_class_of(body_module, classifier).ok()??;
            self.normalize(classifier.clone(), arena, class)?;
            pending.push((arena, class));
            Some(())
        })
    }

    /// Record `class`, serialized at `signature` in `arena`, under its classifier identity.
    fn normalize(
        &mut self,
        signature: KlibPublicIdSignature,
        arena: &KlibIrArena,
        class: &KlibIrClass,
    ) -> Option<TypeName> {
        let name = classifier_name(&signature).ok()?;
        let mut parameters = KlibTypeParameters::default();
        let formals = self.bind(arena, &class.type_parameters, &mut parameters)?;
        let supertypes = class
            .supertypes
            .iter()
            .map(|&supertype| semantic_type(arena, &parameters, supertype).ok())
            .collect::<Option<Vec<_>>>()?;
        let variances = class
            .type_parameters
            .iter()
            .map(|parameter| match parameter.variance()? {
                KlibIrVariance::Invariant => Some(TypeVariance::Invariant),
                KlibIrVariance::In => Some(TypeVariance::In),
                KlibIrVariance::Out => Some(TypeVariance::Out),
            })
            .collect::<Option<Vec<_>>>()?;
        let bounds = formals
            .iter()
            .map(|&(formal, _)| self.bounds[formal].clone())
            .collect();
        let applied =
            Ty::obj_args_name(name, &formals.iter().map(|&(_, ty)| ty).collect::<Vec<_>>());
        self.classes.insert(
            name,
            SerializedClass {
                signature,
                formals: TypeParameters::new(
                    formals
                        .iter()
                        .map(|&(formal, _)| formal.to_owned())
                        .collect(),
                    bounds,
                    variances,
                ),
                parameters,
                applied,
                supertypes,
            },
        );
        Some(name)
    }

    /// Bind each of `declared` to a semantic type parameter of its own in `parameters`, and record
    /// its declared upper bounds. The identities and their types, in declaration order.
    fn bind(
        &mut self,
        arena: &KlibIrArena,
        declared: &[KlibIrTypeParameter],
        parameters: &mut KlibTypeParameters,
    ) -> Option<Vec<(&'static str, Ty)>> {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let formals = declared
            .iter()
            .map(|parameter| {
                let formal = crate::types::metadata_type_parameter(&parameter.name);
                let ty = Ty::ty_param(formal, any);
                parameters
                    .bind(&parameter.base.symbol, ty)
                    .then_some((formal, ty))
            })
            .collect::<Option<Vec<_>>>()?;
        for (parameter, &(formal, _)) in declared.iter().zip(&formals) {
            let bounds = parameter
                .supertypes
                .iter()
                .map(|&bound| semantic_type(arena, parameters, bound).ok())
                .collect::<Option<Vec<_>>>()?;
            self.bounds.insert(formal, bounds);
        }
        Some(formals)
    }

    /// The result of the inherited `declaration` of `identity`, as the inheriting class sees it
    /// through the applied hierarchy that reaches its declaring class.
    fn inherited_result(
        &self,
        identity: &KlibDependencyCallable,
        (arena, function): (&KlibIrArena, &KlibIrFunction),
    ) -> Option<Ty> {
        // An accessor is a member of the class that declares its property.
        let signature = match &identity.declaration {
            KlibDeclarationSignature::Public(signature) => signature,
            KlibDeclarationSignature::Accessor(accessor) => accessor.property(),
            KlibDeclarationSignature::FileLocal { .. } => return None,
        };
        let mut owners = self
            .classes
            .values()
            .filter(|class| signature.owner_path() == Some(class.signature.declaration_path()));
        let (Some(declaring), None) = (owners.next(), owners.next()) else {
            return None;
        };
        let mut parameters = declaring.parameters.clone();
        for (parameter, &own) in function.type_parameters.iter().zip(&self.member) {
            if !parameters.bind(&parameter.base.symbol, own) {
                return None;
            }
        }
        let declared = semantic_type(arena, &parameters, function.return_type).ok()?;
        let applied =
            crate::assignable::applied_supertype(self, self.inheriting, declaring.applied)?;
        let bindings = declaring
            .formals
            .type_params()
            .iter()
            .cloned()
            .zip(applied.type_args().iter().copied())
            .collect::<HashMap<_, _>>();
        Some(crate::types::ty_subst_applied_arguments(
            declared, &bindings,
        ))
    }
}

/// Visit the public classifier of every class type `ty` names.
fn classifiers(
    arena: &KlibIrArena,
    ty: KlibIrTypeId,
    visit: &mut dyn FnMut(&KlibPublicIdSignature) -> Option<()>,
) -> Option<()> {
    match arena.ty(ty) {
        KlibIrType::Simple {
            classifier,
            arguments,
            ..
        } => {
            if classifier.kind == KlibIrSymbolKind::Class {
                let KlibIrSignature::Public(signature) = &classifier.signature else {
                    return None;
                };
                visit(signature)?;
            }
            for argument in arguments {
                if let KlibIrTypeArgument::Type { ty, .. } = argument {
                    classifiers(arena, *ty, visit)?;
                }
            }
            Some(())
        }
        KlibIrType::DefinitelyNotNull(inner) => classifiers(arena, *inner, visit),
        KlibIrType::Dynamic { .. } | KlibIrType::Error { .. } => None,
    }
}

impl TypeOracle for SerializedHierarchy {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        let Some(class) = ty
            .kotlin_class_internal()
            .and_then(|name| self.classes.get(&name))
        else {
            return Vec::new();
        };
        crate::assignable::applied_supertype_templates(&class.formals, &class.supertypes, ty)
    }

    fn type_param_variance(&self, owner: TypeName, index: usize) -> TypeVariance {
        self.classes
            .get(&owner)
            .and_then(|class| class.formals.type_param_variances().get(index).copied())
            .unwrap_or_default()
    }

    fn type_param_upper_bounds(&self, owner: TypeName, index: usize) -> Vec<Ty> {
        self.classes
            .get(&owner)
            .and_then(|class| class.formals.type_param_bounds().get(index).cloned())
            .filter(|bounds| !bounds.is_empty())
            .unwrap_or_else(|| vec![Ty::nullable(Ty::obj("kotlin/Any"))])
    }

    fn type_variable_bounds(&self, name: &str) -> Vec<Ty> {
        self.bounds.get(name).cloned().unwrap_or_default()
    }
}
