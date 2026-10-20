use crate::types::{Ty, TyKind, TypeVariance};

use super::{
    DeclarationId, DeclarationNameId, ResolvedModuleIndex, ResolvedTy, TypeParameterId,
    UnpublishableType,
};

/// One resolved upper bound of a declaration-owned type parameter. The classifier/interface
/// distinction is semantic metadata needed by generic-signature emitters; a backend must not
/// rediscover it from a physical owner name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedTypeParameterBound {
    pub ty: ResolvedTy,
    pub is_interface: bool,
}

impl ResolvedTypeParameterBound {
    /// Whether an upper bound is an interface-like bound rather than the one class bound. A
    /// function type is a `kotlin.FunctionN` interface type; a classifier type is one when its
    /// declaration is an interface; another type parameter is a type variable, never the class
    /// bound, whatever its own bound is (kotlinc signs `<U : Base, T : U>` as `T::TU;`).
    pub fn is_interface_type(
        bound: Ty,
        classifier_is_interface: impl FnOnce(crate::types::TypeName) -> bool,
    ) -> bool {
        {
            let bound = bound.non_null();
            match bound.kind() {
                TyKind::Fun(_) | TyKind::TyParam(..) => true,
                _ => bound.obj_internal().is_some_and(classifier_is_interface),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolvedTypeParameterFlags(u8);

impl ResolvedTypeParameterFlags {
    const IN: u8 = 1 << 0;
    const OUT: u8 = 1 << 1;
    const NON_NULL: u8 = 1 << 2;
    const REIFIED: u8 = 1 << 3;

    pub const fn new(variance: TypeVariance, non_null: bool, reified: bool) -> Self {
        let mut bits = match variance {
            TypeVariance::Invariant => 0,
            TypeVariance::In => Self::IN,
            TypeVariance::Out => Self::OUT,
        };
        if non_null {
            bits |= Self::NON_NULL;
        }
        if reified {
            bits |= Self::REIFIED;
        }
        Self(bits)
    }

    pub const fn variance(self) -> TypeVariance {
        if self.0 & Self::IN != 0 {
            TypeVariance::In
        } else if self.0 & Self::OUT != 0 {
            TypeVariance::Out
        } else {
            TypeVariance::Invariant
        }
    }

    pub const fn is_non_null(self) -> bool {
        self.0 & Self::NON_NULL != 0
    }

    pub const fn is_reified(self) -> bool {
        self.0 & Self::REIFIED != 0
    }
}

/// Compact persistent declaration facts for one type parameter. `id` is its semantic identity;
/// `name` is the declared metadata/display name, while `semantic_name` is the resolver's
/// alpha-renamed variable used inside published [`Ty`] shapes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTypeParameterHeader {
    pub id: TypeParameterId,
    name: DeclarationNameId,
    semantic_name: DeclarationNameId,
    pub flags: ResolvedTypeParameterFlags,
    pub bounds: Box<[ResolvedTypeParameterBound]>,
}

struct PreparedTypeParameter {
    ordinal: u32,
    name: String,
    semantic_name: String,
    flags: ResolvedTypeParameterFlags,
    bounds: Box<[ResolvedTypeParameterBound]>,
}

impl ResolvedModuleIndex {
    pub(crate) fn publish_declaration_type_parameter_count(
        &mut self,
        declaration: DeclarationId,
        count: u32,
    ) {
        assert!(
            self.declaration_type_parameter_counts
                .insert(declaration, count)
                .is_none(),
            "a declaration's source type-parameter count is published once"
        );
    }

    pub(crate) fn declaration_type_parameters_complete(&self, declaration: DeclarationId) -> bool {
        let Some(&count) = self.declaration_type_parameter_counts.get(&declaration) else {
            // Syntaxless generated declarations carry their generic contract directly from their
            // provider and have no compact source count to await.
            return true;
        };
        // A classifier may append captured lexical parameters after its own source formals. The
        // lifecycle count describes only the written prefix; those applied-layout slots do not
        // make an otherwise complete declaration generic contract incomplete.
        (0..count).all(|ordinal| self.type_parameter(declaration, ordinal).is_some())
    }

    /// Bound ordinals with the primary concrete-class constituent first and source order preserved
    /// within the class/interface groups. Metadata and target erasure consume this finalized header
    /// fact without rediscovering classifier kinds downstream.
    pub fn type_parameter_bound_order(
        &self,
        declaration: DeclarationId,
        ordinal: u32,
    ) -> Vec<usize> {
        let Some(parameter) = self.type_parameter(declaration, ordinal) else {
            return Vec::new();
        };
        let Some(header) = self.type_parameter_header(parameter) else {
            return Vec::new();
        };
        let mut order = (0..header.bounds.len()).collect::<Vec<_>>();
        order.sort_by_key(|&bound| header.bounds[bound].is_interface);
        order
    }

    /// Declaration spellings permuted with [`Self::type_parameter_bound_order`]. The semantic type
    /// and its type-alias spelling must remain parallel when a class bound moves ahead of interfaces.
    pub fn declaration_spellings_primary_bound_first(
        &self,
        declaration: DeclarationId,
    ) -> crate::spelling::DeclaredSpellings {
        let mut spellings = self
            .declaration_spellings(declaration)
            .cloned()
            .unwrap_or_default();
        for ordinal in 0..spellings.type_param_bounds.len() {
            let source = spellings.type_param_bounds[ordinal].clone();
            spellings.type_param_bounds[ordinal] = self
                .type_parameter_bound_order(declaration, ordinal as u32)
                .into_iter()
                .map(|bound| source.get(bound).cloned().unwrap_or_default())
                .collect();
        }
        spellings
    }

    pub fn publish_type_parameter(
        &mut self,
        declaration: DeclarationId,
        ordinal: u32,
        name: &str,
        semantic_name: &str,
        flags: ResolvedTypeParameterFlags,
        bounds: impl IntoIterator<Item = (Ty, bool)>,
    ) -> Result<TypeParameterId, UnpublishableType> {
        self.publish_type_parameters_atomically(
            declaration,
            [(
                ordinal,
                name.to_owned(),
                semantic_name.to_owned(),
                flags,
                bounds,
            )],
        )
        .map(|mut parameters| parameters.pop().expect("one type parameter was published"))
    }

    /// Validate a declaration's complete generic contract before publishing any of it.
    pub(crate) fn publish_type_parameters_atomically<I, B>(
        &mut self,
        declaration: DeclarationId,
        parameters: I,
    ) -> Result<Vec<TypeParameterId>, UnpublishableType>
    where
        I: IntoIterator<Item = (u32, String, String, ResolvedTypeParameterFlags, B)>,
        B: IntoIterator<Item = (Ty, bool)>,
    {
        let prepared = parameters
            .into_iter()
            .map(|(ordinal, name, semantic_name, flags, bounds)| {
                let bounds = bounds
                    .into_iter()
                    .map(|(ty, is_interface)| {
                        Ok(ResolvedTypeParameterBound {
                            ty: ResolvedTy::new(ty)?,
                            is_interface,
                        })
                    })
                    .collect::<Result<Vec<_>, UnpublishableType>>()?
                    .into_boxed_slice();
                assert!(
                    !self.type_parameters.contains_key(&(declaration, ordinal)),
                    "a declaration type parameter may be published only once"
                );
                Ok(PreparedTypeParameter {
                    ordinal,
                    name,
                    semantic_name,
                    flags,
                    bounds,
                })
            })
            .collect::<Result<Vec<_>, UnpublishableType>>()?;
        for (position, parameter) in prepared.iter().enumerate() {
            assert!(
                prepared[position + 1..]
                    .iter()
                    .all(|later| later.ordinal != parameter.ordinal),
                "a declaration type-parameter batch cannot repeat an ordinal"
            );
        }
        let mut published = Vec::with_capacity(prepared.len());
        for parameter in prepared {
            let id = TypeParameterId::from_raw(super::header::next_id(
                self.type_parameter_headers.len(),
                "resolved type parameters",
            ));
            let name = self.intern_declaration_name(&parameter.name);
            let semantic_name = self.intern_declaration_name(&parameter.semantic_name);
            self.type_parameter_headers
                .push(ResolvedTypeParameterHeader {
                    id,
                    name,
                    semantic_name,
                    flags: parameter.flags,
                    bounds: parameter.bounds,
                });
            self.type_parameter_owners
                .push((declaration, parameter.ordinal));
            self.type_parameters
                .insert((declaration, parameter.ordinal), id);
            published.push(id);
        }
        Ok(published)
    }

    pub fn type_parameter(
        &self,
        declaration: DeclarationId,
        ordinal: u32,
    ) -> Option<TypeParameterId> {
        self.type_parameters.get(&(declaration, ordinal)).copied()
    }

    pub fn type_parameter_header(
        &self,
        parameter: TypeParameterId,
    ) -> Option<&ResolvedTypeParameterHeader> {
        self.type_parameter_headers.get(parameter.raw() as usize)
    }

    /// The stable declaration that owns `parameter`.
    ///
    /// Type-parameter source spelling is deliberately absent from this relation: two declarations
    /// may both write `T`, while local-class erasure and lexical substitution need the exact owner.
    pub fn type_parameter_owner(&self, parameter: TypeParameterId) -> Option<DeclarationId> {
        self.type_parameter_owners
            .get(parameter.raw() as usize)
            .map(|(declaration, _)| *declaration)
    }

    /// A classifier's own type parameters, in order, each as the type its declarations refer to it
    /// by: its semantic identity over its first upper bound.
    pub fn own_type_parameter_types(&self, classifier: DeclarationId) -> Option<Box<[Ty]>> {
        let own = self.classifier_own_type_parameter_count(classifier)? as usize;
        self.classifier_type_arguments(classifier)?
            .iter()
            .take(own)
            .map(|&parameter| {
                let bound = self
                    .type_parameter_header(parameter)?
                    .bounds
                    .first()
                    .map_or_else(
                        || Ty::nullable(Ty::obj("kotlin/Any")),
                        |bound| bound.ty.get(),
                    );
                Some(Ty::ty_param(
                    self.type_parameter_semantic_name(parameter)?,
                    bound,
                ))
            })
            .collect()
    }

    pub fn type_parameter_name(&self, parameter: TypeParameterId) -> Option<&str> {
        let header = self.type_parameter_header(parameter)?;
        self.declaration_names
            .get(header.name.raw() as usize)
            .map(AsRef::as_ref)
    }

    pub fn type_parameter_semantic_name(&self, parameter: TypeParameterId) -> Option<&str> {
        let header = self.type_parameter_header(parameter)?;
        self.declaration_names
            .get(header.semantic_name.raw() as usize)
            .map(AsRef::as_ref)
    }

    /// Source parameter names owned by a finalized type alias, in declaration order.
    pub fn type_alias_formals(&self, declaration: DeclarationId) -> Vec<String> {
        (0..)
            .map_while(|ordinal| self.type_parameter(declaration, ordinal))
            .filter_map(|parameter| self.type_parameter_name(parameter).map(str::to_owned))
            .collect()
    }

    /// Type-parameter identities carried by an applied classifier type, in the same order as
    /// [`Ty::type_args`]: parameters declared by the classifier, followed by lexically captured
    /// parameters from nearest owner outward.
    pub fn classifier_type_arguments(
        &self,
        classifier: DeclarationId,
    ) -> Option<&[TypeParameterId]> {
        self.classifier_type_arguments
            .get(&classifier)
            .map(Box::as_ref)
    }

    pub fn classifier_own_type_parameter_count(&self, classifier: DeclarationId) -> Option<u32> {
        self.classifier_own_type_parameter_counts
            .get(&classifier)
            .copied()
    }

    /// The classifier applied to its own stable semantic type-parameter identities.
    pub fn classifier_self_type(&self, classifier: DeclarationId) -> Option<Ty> {
        let identity = self.classifier_header(classifier)?.classifier;
        let arguments = self
            .classifier_type_arguments(classifier)
            .unwrap_or_default()
            .iter()
            .map(|parameter| {
                let header = self.type_parameter_header(*parameter)?;
                let semantic_name = self.type_parameter_semantic_name(*parameter)?;
                let bound = header
                    .bounds
                    .first()
                    .map(|bound| bound.ty.get())
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                Some(Ty::ty_param(semantic_name, bound))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Ty::obj_args_name(identity, &arguments))
    }

    pub(crate) fn publish_classifier_type_arguments(
        &mut self,
        classifier: DeclarationId,
        own_count: u32,
        parameters: impl IntoIterator<Item = TypeParameterId>,
    ) {
        assert!(
            self.classifier_header(classifier).is_some(),
            "classifier type arguments require a published classifier header"
        );
        let parameters = parameters.into_iter().collect::<Box<[_]>>();
        assert!(
            own_count as usize <= parameters.len(),
            "own classifier parameters must be a prefix of its applied layout"
        );
        assert!(
            self.classifier_own_type_parameter_counts
                .insert(classifier, own_count)
                .is_none()
                && self
                    .classifier_type_arguments
                    .insert(classifier, parameters)
                    .is_none(),
            "a classifier type-argument layout may be published only once"
        );
    }

    /// Complete semantic substitution domain of a finalized type alias, in declaration order.
    /// Its expansion has already been alpha-renamed to these identities, so source spellings are
    /// not interchangeable here. Missing identity metadata invalidates the whole stable header.
    pub(crate) fn type_alias_substitution_formals(
        &self,
        declaration: DeclarationId,
    ) -> Option<Vec<String>> {
        (0..)
            .map_while(|ordinal| self.type_parameter(declaration, ordinal))
            .map(|parameter| {
                self.type_parameter_semantic_name(parameter)
                    .map(str::to_owned)
            })
            .collect()
    }

    pub(crate) fn type_parameter_by_semantic_name(
        &self,
        semantic_name: &str,
    ) -> Option<TypeParameterId> {
        self.type_parameter_headers.iter().find_map(|parameter| {
            (self.type_parameter_semantic_name(parameter.id) == Some(semantic_name))
                .then_some(parameter.id)
        })
    }

    /// Every declared upper bound of the type parameter with semantic identity `semantic_name`,
    /// in declaration order, or `None` when no published parameter has that identity.
    pub(crate) fn type_parameter_declared_bounds(&self, semantic_name: &str) -> Option<Vec<Ty>> {
        let parameter = self.type_parameter_by_semantic_name(semantic_name)?;
        let header = self.type_parameter_header(parameter)?;
        Some(header.bounds.iter().map(|bound| bound.ty.get()).collect())
    }

    /// Bind a selected declaration's normalized type-variable identity to its stable parameter.
    /// The declaration and its lexical owners form one scope tower: callable parameters shadow
    /// enclosing classifier/function parameters, while an unrelated declaration never participates.
    pub(crate) fn type_parameter_in_declaration_scope(
        &self,
        declaration: DeclarationId,
        semantic_name: &str,
    ) -> Option<TypeParameterId> {
        let scopes = std::iter::successors(Some(declaration), |&owner| {
            self.declaration_anchor(owner)
                .and_then(|anchor| anchor.owner)
        });
        for scope in scopes {
            let parameters = (0..).map_while(|ordinal| self.type_parameter(scope, ordinal));
            for parameter in parameters {
                if self.type_parameter_semantic_name(parameter) == Some(semantic_name) {
                    return Some(parameter);
                }
            }
        }
        None
    }

    pub(super) fn type_parameter_storage_payload_bytes(&self) -> usize {
        self.type_parameters.len()
            * (std::mem::size_of::<(DeclarationId, u32)>() + std::mem::size_of::<TypeParameterId>())
            + self.type_parameter_owners.len() * std::mem::size_of::<(DeclarationId, u32)>()
            + self.type_parameter_headers.len() * std::mem::size_of::<ResolvedTypeParameterHeader>()
            + self
                .type_parameter_headers
                .iter()
                .map(|parameter| {
                    parameter.bounds.len() * std::mem::size_of::<ResolvedTypeParameterBound>()
                })
                .sum::<usize>()
    }
}
