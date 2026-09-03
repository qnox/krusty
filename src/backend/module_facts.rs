//! Frozen semantic classifier facts exposed to representation backends.
//!
//! Pass 2 still owns a migration-era frontend symbol table while it checks one active source unit.
//! A backend must never receive that table: it contains lookup APIs, AST-backed member keys, and can
//! contain provisional body-local entries for a source that has not streamed yet. This module copies
//! only finalized classifier records before Pass 2 starts and rejects every nested pending/error type.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::libraries::{
    CallSig, DefaultCallRealization, GenericSig, InlineBodyPlan, LibraryCallable, LibraryMember,
    LibraryType,
};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName, TypeVariance, Visibility};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendFactError {
    UnpublishableClassifier(TypeName, UndeterminedType),
    IncompleteClassifier(TypeName),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UndeterminedType {
    Pending,
    Error,
}

/// The only semantic query a representation backend may make after common lowering.
///
/// Calls, properties, overloads, and source names are absent deliberately: checked IR already owns
/// every selected declaration identity and realization. The returned classifier is guaranteed not
/// to contain `Ty::Pending`, `Ty::Error`, source type references, or ordinary source-body payloads.
pub trait BackendClassifierSource {
    fn classifier(&self, classifier: TypeName) -> Option<Arc<BackendClassifierFact>>;
}

/// The exact classifier information representation backends may inspect after semantic checking.
/// Resolver candidate maps, constructors, constants, source keys, inline plans, contracts, and
/// parser-backed member identities cannot be represented here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendClassifierFact {
    pub access: crate::libraries::ClassifierAccess,
    pub is_kotlin: bool,
    pub source: bool,
    pub outer_instance: Option<TypeName>,
    pub kind: crate::libraries::TypeKind,
    pub is_abstract: bool,
    pub is_extensible: bool,
    pub supertypes: Box<[TypeName]>,
    /// Functions and property accessors in semantic declaration order.
    pub surface: Box<[BackendMemberFact]>,
    pub type_param_variances: Box<[TypeVariance]>,
    pub value_underlying: Option<Ty>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendMemberFact {
    pub name: BackendMemberName,
    pub physical_name: Option<Box<str>>,
    pub owner: Option<TypeName>,
    pub physical_params: Box<[Ty]>,
    pub params: Box<[Ty]>,
    pub ret: Ty,
    pub physical_ret: Ty,
    pub descriptor: Box<str>,
    pub realization: crate::libraries::MemberRealization,
    pub suspend: bool,
    pub abstract_member: bool,
    pub visibility: Visibility,
    pub param_names: Box<[Box<str>]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendMemberName {
    Declared(Box<str>),
    PropertyGetter(Box<str>),
    PropertySetter(Box<str>),
}

fn erase_backend_parameter(ty: Ty) -> Ty {
    ty.ty_param_bound().map_or(ty, Ty::non_null)
}

impl BackendClassifierFact {
    fn from_library(shape: &LibraryType) -> Self {
        let mut surface = Vec::new();
        for name in &shape.declared_callable_order {
            let Some(callables) = shape.declared_callables.get(name) else {
                continue;
            };
            surface.extend(
                callables
                    .functions()
                    .iter()
                    .map(BackendMemberFact::from_function),
            );
            for property in callables.properties() {
                surface.push(BackendMemberFact::from_callable_named(
                    &property.getter,
                    property.visibility,
                    &property.context_param_names,
                    BackendMemberName::PropertyGetter(property.name.as_str().into()),
                ));
                if let Some(setter) = &property.setter {
                    surface.push(BackendMemberFact::from_callable_named(
                        setter,
                        property.setter_visibility,
                        &property.context_param_names,
                        BackendMemberName::PropertySetter(property.name.as_str().into()),
                    ));
                }
            }
        }
        if surface.is_empty() {
            surface.extend(shape.members.iter().map(BackendMemberFact::from_member));
        }
        Self {
            access: shape.access,
            is_kotlin: shape.is_kotlin,
            source: shape.source_file.is_some(),
            outer_instance: shape.outer_instance,
            kind: shape.kind,
            is_abstract: shape.inheritance.is_abstract,
            is_extensible: shape.inheritance.is_extensible,
            supertypes: shape
                .supertypes
                .iter_ids()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            surface: surface.into_boxed_slice(),
            type_param_variances: shape
                .type_param_variances()
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            value_underlying: shape.value_underlying,
        }
    }

    pub fn is_interface(&self) -> bool {
        matches!(
            self.kind,
            crate::libraries::TypeKind::Interface | crate::libraries::TypeKind::Annotation
        )
    }

    pub fn is_annotation(&self) -> bool {
        self.kind == crate::libraries::TypeKind::Annotation
    }

    pub fn is_enum(&self) -> bool {
        self.kind == crate::libraries::TypeKind::Enum
    }
}

impl BackendMemberFact {
    fn from_function(function: &crate::libraries::FunctionInfo) -> Self {
        let callable = &function.callable;
        Self {
            name: BackendMemberName::Declared(callable.name.as_str().into()),
            physical_name: None,
            owner: Some(callable.owner),
            physical_params: callable.physical_params.clone().into_boxed_slice(),
            params: callable.params.clone().into_boxed_slice(),
            ret: callable.ret,
            physical_ret: callable.physical_ret,
            descriptor: callable.descriptor.as_str().into(),
            realization: callable.member_realization,
            suspend: function.flags.suspend,
            abstract_member: function.flags.is_abstract,
            visibility: function.visibility,
            param_names: function
                .call_sig
                .param_names
                .iter()
                .map(|name| name.as_str().into())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }

    fn from_member(member: &LibraryMember) -> Self {
        Self {
            name: BackendMemberName::Declared(member.name.as_str().into()),
            physical_name: member.physical_name.as_deref().map(Into::into),
            owner: member.owner,
            physical_params: member.physical_params.clone().into_boxed_slice(),
            params: member.params.clone().into_boxed_slice(),
            ret: member.ret,
            physical_ret: member.physical_ret,
            descriptor: member.descriptor.as_str().into(),
            realization: member.realization,
            suspend: member.suspend(),
            abstract_member: member.is_abstract(),
            visibility: member.visibility,
            param_names: member
                .call_sig
                .param_names
                .iter()
                .map(|name| name.as_str().into())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }

    fn from_callable_named(
        callable: &LibraryCallable,
        visibility: Visibility,
        param_names: &[String],
        name: BackendMemberName,
    ) -> Self {
        Self {
            name,
            physical_name: None,
            owner: Some(callable.owner),
            physical_params: callable.physical_params.clone().into_boxed_slice(),
            params: callable.params.clone().into_boxed_slice(),
            ret: callable.ret,
            physical_ret: callable.physical_ret,
            descriptor: callable.descriptor.as_str().into(),
            realization: callable.member_realization,
            suspend: callable.suspend,
            abstract_member: callable.is_abstract,
            visibility,
            param_names: param_names
                .iter()
                .map(|name| name.as_str().into())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }

    pub fn suspend(&self) -> bool {
        self.suspend
    }

    pub fn is_abstract(&self) -> bool {
        self.abstract_member
    }
}

/// Finalized current-module classifier records, frozen before Pass 2 starts.
pub struct BackendModuleFacts {
    classifiers: HashMap<TypeName, Arc<BackendClassifierFact>>,
    /// Stable identities of classifiers declared inside ordinary bodies. Their completed semantic
    /// shape belongs to the active Pass-2 IR file, never to the pre-Pass-2 module snapshot. Keeping
    /// only the identity prevents an accidental lookup of a same-named dependency classifier.
    body_local_classifiers: HashSet<TypeName>,
    source_value_classes: HashMap<TypeName, Ty>,
}

impl BackendModuleFacts {
    /// Build the complete current-module backend view from stable, pending-free declaration
    /// headers. No resolver/provider record or Pass-1 parser coordinate participates.
    pub(crate) fn from_resolved_index(
        index: &crate::fir::ResolvedModuleIndex,
    ) -> Result<Self, BackendFactError> {
        let mut classifiers = HashMap::new();
        let mut body_local_classifiers = HashSet::new();
        let mut source_value_classes = HashMap::new();
        for raw in 0..index.declaration_count() {
            let declaration = crate::fir::DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable declarations for a packed id"),
            );
            let Some(classifier) = index.classifier_header(declaration) else {
                continue;
            };
            if index.is_body_local_declaration(declaration) {
                body_local_classifiers.insert(classifier.classifier);
                continue;
            }
            let declaration_header = index.declaration_header(declaration).ok_or(
                BackendFactError::IncompleteClassifier(classifier.classifier),
            )?;
            let flags = declaration_header.flags;
            let kind = if flags.has(crate::fir::DeclarationFlags::ANNOTATION_CLASS) {
                crate::libraries::TypeKind::Annotation
            } else if flags.has(crate::fir::DeclarationFlags::ENUM) {
                crate::libraries::TypeKind::Enum
            } else if flags.has(crate::fir::DeclarationFlags::SINGLETON) {
                crate::libraries::TypeKind::Object
            } else if flags.has(crate::fir::DeclarationFlags::INTERFACE) {
                crate::libraries::TypeKind::Interface
            } else {
                crate::libraries::TypeKind::Class
            };
            let is_interface = matches!(
                kind,
                crate::libraries::TypeKind::Interface | crate::libraries::TypeKind::Annotation
            );
            let mut direct_supertypes = classifier
                .superclass
                .into_iter()
                .chain(classifier.interfaces.iter().copied())
                .filter_map(|ty| ty.get().non_null().obj_internal())
                .collect::<Vec<_>>();
            if direct_supertypes.is_empty()
                && classifier.classifier != crate::types::type_name("kotlin/Any")
            {
                direct_supertypes.push(crate::types::type_name("kotlin/Any"));
            }
            let outer_instance = flags
                .has(crate::fir::DeclarationFlags::INNER)
                .then(|| {
                    declaration_header
                        .owner
                        .and_then(|owner| index.classifier_header(owner))
                        .map(|owner| owner.classifier)
                })
                .flatten();
            let type_param_variances = index
                .classifier_type_arguments(declaration)
                .unwrap_or_default()
                .iter()
                .map(|parameter| {
                    index
                        .type_parameter_header(*parameter)
                        .ok_or(BackendFactError::IncompleteClassifier(
                            classifier.classifier,
                        ))
                        .map(|parameter| parameter.flags.variance())
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice();
            let mut surface = Vec::new();
            let mut value_underlying = None;
            for child_raw in 0..index.declaration_count() {
                let child = crate::fir::DeclarationId::from_raw(
                    u32::try_from(child_raw).expect("too many stable declarations for a packed id"),
                );
                let Some(anchor) = index.declaration_anchor(child) else {
                    continue;
                };
                if anchor.owner != Some(declaration) {
                    continue;
                }
                let Some(child_header) = index.declaration_header(child) else {
                    continue;
                };
                match anchor.kind {
                    crate::fir::DeclarationKind::Function => {
                        let signature = index.signature(child).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        let callable = index.callable_for_declaration(child).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        let mut parameters = signature
                            .parameters
                            .iter()
                            .map(|parameter| erase_backend_parameter(parameter.get()))
                            .collect::<Vec<_>>();
                        if let Some(receiver) = callable.shape.extension_receiver {
                            parameters.insert(
                                (callable.shape.context_parameter_count as usize)
                                    .min(parameters.len()),
                                receiver.get(),
                            );
                        }
                        let param_names = (0..index.callable_parameter_name_count(callable.id))
                            .filter_map(|ordinal| {
                                index
                                    .callable_parameter_name(callable.id, ordinal as u32)
                                    .map(Box::<str>::from)
                            })
                            .collect::<Vec<_>>()
                            .into_boxed_slice();
                        surface.push((
                            index.source_order(child).unwrap_or(u32::MAX),
                            BackendMemberFact {
                                name: BackendMemberName::Declared(
                                    index
                                        .declaration_name(child)
                                        .ok_or(BackendFactError::IncompleteClassifier(
                                            classifier.classifier,
                                        ))?
                                        .into(),
                                ),
                                physical_name: None,
                                owner: Some(classifier.classifier),
                                physical_params: parameters.clone().into_boxed_slice(),
                                params: parameters.into_boxed_slice(),
                                ret: signature.result.get(),
                                physical_ret: signature.result.get(),
                                descriptor: "".into(),
                                realization: crate::libraries::MemberRealization::Dispatch,
                                suspend: child_header
                                    .flags
                                    .has(crate::fir::DeclarationFlags::SUSPEND),
                                abstract_member: child_header
                                    .flags
                                    .has(crate::fir::DeclarationFlags::ABSTRACT),
                                visibility: child_header.visibility,
                                param_names,
                            },
                        ));
                    }
                    crate::fir::DeclarationKind::Property => {
                        let signature = index.signature(child).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        let property_id = index.property_for_declaration(child).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        let property = index.property(property_id).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        if flags.has(crate::fir::DeclarationFlags::VALUE)
                            && child_header
                                .flags
                                .has(crate::fir::DeclarationFlags::PROPERTY_PARAMETER)
                        {
                            value_underlying = Some(signature.result.get());
                        }
                        let name = index.declaration_name(child).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        let mut parameters = signature
                            .parameters
                            .iter()
                            .take(property.context_parameter_count as usize)
                            .map(|parameter| parameter.get())
                            .collect::<Vec<_>>();
                        if let Some(receiver) = property.extension_receiver {
                            parameters.push(receiver.get());
                        }
                        let abstract_member = child_header
                            .flags
                            .has(crate::fir::DeclarationFlags::ABSTRACT);
                        surface.push((
                            index.source_order(child).unwrap_or(u32::MAX),
                            BackendMemberFact {
                                name: BackendMemberName::PropertyGetter(name.into()),
                                physical_name: None,
                                owner: Some(classifier.classifier),
                                physical_params: parameters.clone().into_boxed_slice(),
                                params: parameters.clone().into_boxed_slice(),
                                ret: signature.result.get(),
                                physical_ret: signature.result.get(),
                                descriptor: "".into(),
                                realization: crate::libraries::MemberRealization::Dispatch,
                                suspend: false,
                                abstract_member,
                                visibility: child_header.visibility,
                                param_names: Box::default(),
                            },
                        ));
                        if property.mutable {
                            parameters.push(signature.result.get());
                            let setter_visibility = index
                                .owned_declaration(child, crate::fir::DeclarationKind::Accessor, 1)
                                .and_then(|setter| index.declaration_header(setter))
                                .map_or(child_header.visibility, |setter| setter.visibility);
                            surface.push((
                                index.source_order(child).unwrap_or(u32::MAX),
                                BackendMemberFact {
                                    name: BackendMemberName::PropertySetter(name.into()),
                                    physical_name: None,
                                    owner: Some(classifier.classifier),
                                    physical_params: parameters.clone().into_boxed_slice(),
                                    params: parameters.into_boxed_slice(),
                                    ret: Ty::Unit,
                                    physical_ret: Ty::Unit,
                                    descriptor: "".into(),
                                    realization: crate::libraries::MemberRealization::Dispatch,
                                    suspend: false,
                                    abstract_member,
                                    visibility: setter_visibility,
                                    param_names: Box::default(),
                                },
                            ));
                        }
                    }
                    _ => {}
                }
            }
            surface.sort_by_key(|(order, _)| *order);
            let fact = BackendClassifierFact {
                access: declaration_header.visibility.into(),
                is_kotlin: true,
                source: true,
                outer_instance,
                kind,
                is_abstract: flags.has(crate::fir::DeclarationFlags::ABSTRACT) || is_interface,
                is_extensible: !is_interface && !flags.has(crate::fir::DeclarationFlags::FINAL),
                supertypes: direct_supertypes.into_boxed_slice(),
                surface: surface
                    .into_iter()
                    .map(|(_, member)| member)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                type_param_variances,
                value_underlying,
            };
            if let Some(underlying) = value_underlying {
                source_value_classes.insert(classifier.classifier, underlying);
            }
            classifiers.insert(classifier.classifier, Arc::new(fact));
        }
        Ok(Self {
            classifiers,
            body_local_classifiers,
            source_value_classes,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_classifiers(
        source: impl IntoIterator<Item = (TypeName, LibraryType)>,
        body_local_classifiers: impl IntoIterator<Item = TypeName>,
    ) -> Result<Self, BackendFactError> {
        let mut classifiers = HashMap::new();
        let mut source_value_classes = HashMap::new();
        for (classifier, shape) in source {
            if classifiers.contains_key(&classifier) {
                continue;
            }
            validate_classifier(classifier, &shape)?;
            if let Some(underlying) = shape.value_underlying {
                source_value_classes.insert(classifier, underlying);
            }
            classifiers.insert(
                classifier,
                Arc::new(BackendClassifierFact::from_library(&shape)),
            );
        }
        Ok(Self {
            classifiers,
            body_local_classifiers: body_local_classifiers.into_iter().collect(),
            source_value_classes,
        })
    }

    pub fn classifier(&self, classifier: TypeName) -> Option<Arc<BackendClassifierFact>> {
        self.classifiers.get(&classifier).cloned()
    }

    pub fn classifiers(&self) -> impl Iterator<Item = (TypeName, &BackendClassifierFact)> {
        self.classifiers
            .iter()
            .map(|(classifier, shape)| (*classifier, shape.as_ref()))
    }

    pub fn is_body_local(&self, classifier: TypeName) -> bool {
        self.body_local_classifiers.contains(&classifier)
    }

    pub fn source_value_classes(&self) -> &HashMap<TypeName, Ty> {
        &self.source_value_classes
    }
}

/// A per-call view federating the frozen current module with dependency classifier metadata.
/// Dependency records are validated before they are returned, so even a malformed provider cannot
/// put an undetermined semantic type in a backend.
pub struct CheckedBackendClassifiers<'a> {
    module: &'a BackendModuleFacts,
    dependencies: &'a dyn SymbolSource,
}

impl<'a> CheckedBackendClassifiers<'a> {
    pub(crate) fn new(module: &'a BackendModuleFacts, dependencies: &'a dyn SymbolSource) -> Self {
        Self {
            module,
            dependencies,
        }
    }

    pub fn module(&self) -> &BackendModuleFacts {
        self.module
    }
}

impl BackendClassifierSource for CheckedBackendClassifiers<'_> {
    fn classifier(&self, classifier: TypeName) -> Option<Arc<BackendClassifierFact>> {
        if let Some(shape) = self.module.classifier(classifier) {
            return Some(shape);
        }
        if self.module.is_body_local(classifier) {
            return None;
        }
        let shape = self.dependencies.classifier(classifier)?;
        validate_classifier(classifier, &shape).unwrap_or_else(|error| {
            panic!(
                "dependency classifier crossed the backend boundary with invalid types: {error:?}"
            )
        });
        Some(Arc::new(BackendClassifierFact::from_library(&shape)))
    }
}

/// Checked adapter used by the legacy syntax-lowering path while it remains available to tests.
pub struct SymbolSourceClassifiers<'a> {
    source: &'a dyn SymbolSource,
}

impl<'a> SymbolSourceClassifiers<'a> {
    pub fn new(source: &'a dyn SymbolSource) -> Self {
        Self { source }
    }
}

impl BackendClassifierSource for SymbolSourceClassifiers<'_> {
    fn classifier(&self, classifier: TypeName) -> Option<Arc<BackendClassifierFact>> {
        let shape = self.source.classifier(classifier)?;
        validate_classifier(classifier, &shape).unwrap_or_else(|error| {
            panic!("classifier crossed the backend boundary with invalid types: {error:?}")
        });
        Some(Arc::new(BackendClassifierFact::from_library(&shape)))
    }
}

fn validate_classifier(classifier: TypeName, shape: &LibraryType) -> Result<(), BackendFactError> {
    let mut saw_pending = false;
    let mut saw_error = false;
    let mut visit = |ty: Ty| {
        saw_pending |= ty.mentions_pending();
        saw_error |= ty.mentions_error();
    };

    shape
        .supertype_templates
        .iter()
        .copied()
        .for_each(&mut visit);
    shape
        .constructors
        .iter()
        .chain(&shape.members)
        .chain(&shape.companion)
        .chain(shape.enum_entries_accessor.iter())
        .for_each(|member| visit_member(member, &mut visit));
    for callables in shape.declared_callables.values() {
        for function in callables.functions() {
            function.receiver.into_iter().for_each(&mut visit);
            function.ret.class.into_iter().for_each(&mut visit);
            visit_callable(&function.callable, &mut visit);
            function
                .generic_sig
                .iter()
                .for_each(|generic| visit_generic(generic, &mut visit));
            visit_call_sig(&function.call_sig, &mut visit);
        }
        for property in callables.properties() {
            property.receiver.into_iter().for_each(&mut visit);
            visit(property.ty);
            visit_callable(&property.getter, &mut visit);
            property
                .setter
                .iter()
                .for_each(|setter| visit_callable(setter, &mut visit));
            property
                .compile_time_constant
                .iter()
                .for_each(|constant| visit(constant.ty));
        }
    }
    shape
        .constants
        .values()
        .for_each(|constant| visit(constant.ty));
    shape.callable_signature.into_iter().for_each(&mut visit);
    shape
        .callable_signatures
        .iter()
        .copied()
        .for_each(&mut visit);
    shape.value_underlying.into_iter().for_each(&mut visit);
    shape
        .type_param_bounds()
        .iter()
        .flatten()
        .copied()
        .for_each(&mut visit);
    shape
        .named_parameter_lists
        .iter()
        .flat_map(|parameters| parameters.types.iter().copied())
        .for_each(&mut visit);

    if saw_pending {
        Err(BackendFactError::UnpublishableClassifier(
            classifier,
            UndeterminedType::Pending,
        ))
    } else if saw_error {
        Err(BackendFactError::UnpublishableClassifier(
            classifier,
            UndeterminedType::Error,
        ))
    } else {
        Ok(())
    }
}

fn visit_member(member: &LibraryMember, visit: &mut impl FnMut(Ty)) {
    member.physical_params.iter().copied().for_each(&mut *visit);
    member.params.iter().copied().for_each(&mut *visit);
    visit(member.ret);
    visit(member.physical_ret);
    member
        .generic_sig
        .iter()
        .for_each(|generic| visit_generic(generic, visit));
    visit_call_sig(&member.call_sig, visit);
    member.equality_bound.into_iter().for_each(&mut *visit);
    member.declared_ret.into_iter().for_each(&mut *visit);
    member
        .default_realization
        .iter()
        .for_each(|realization| visit_default_realization(realization, visit));
    member
        .inline_body_plan
        .iter()
        .for_each(|plan| visit_inline_plan(plan, visit));
}

fn visit_callable(callable: &LibraryCallable, visit: &mut impl FnMut(Ty)) {
    callable.params.iter().copied().for_each(&mut *visit);
    callable
        .physical_params
        .iter()
        .copied()
        .for_each(&mut *visit);
    visit(callable.ret);
    visit(callable.physical_ret);
    callable.vararg_elem.into_iter().for_each(&mut *visit);
    callable.source_receiver.into_iter().for_each(&mut *visit);
    callable
        .declared_params
        .iter()
        .flat_map(|parameters| parameters.iter().copied())
        .for_each(&mut *visit);
    callable.declared_ret.into_iter().for_each(&mut *visit);
    callable.equality_bound.into_iter().for_each(&mut *visit);
    callable
        .generic_sig
        .iter()
        .for_each(|generic| visit_generic(generic, visit));
    callable
        .default_realization
        .iter()
        .for_each(|realization| visit_default_realization(realization, visit));
    callable
        .inline_body_plan
        .iter()
        .for_each(|plan| visit_inline_plan(plan, visit));
}

fn visit_generic(generic: &GenericSig, visit: &mut impl FnMut(Ty)) {
    generic.receiver.into_iter().for_each(&mut *visit);
    generic.params.iter().copied().for_each(&mut *visit);
    visit(generic.ret);
    generic
        .formal_bounds
        .iter()
        .flatten()
        .copied()
        .for_each(&mut *visit);
}

fn visit_call_sig(call: &CallSig, visit: &mut impl FnMut(Ty)) {
    call.lambda_param_types
        .iter()
        .flatten()
        .copied()
        .for_each(&mut *visit);
    call.lambda_receivers
        .iter()
        .flatten()
        .copied()
        .for_each(&mut *visit);
}

fn visit_default_realization(realization: &DefaultCallRealization, visit: &mut impl FnMut(Ty)) {
    realization
        .real_params
        .iter()
        .copied()
        .for_each(&mut *visit);
    visit(realization.ret);
}

fn visit_inline_plan(plan: &InlineBodyPlan, visit: &mut impl FnMut(Ty)) {
    if let InlineBodyPlan::SuspendBeforeLambdaFinally { enter, cleanup, .. } = plan {
        visit_member(enter, visit);
        visit_member(cleanup, visit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::SourceMember;
    use crate::symbol_source::SymbolSource;

    #[test]
    fn backend_facts_reject_pending_inside_a_nested_member_type() {
        let classifier = crate::types::type_name("sample/Box");
        let mut shape = LibraryType::declaration_header();
        shape.members.push(LibraryMember::new(
            "read".to_owned(),
            Vec::new(),
            Ty::obj_args("sample/Result", &[Ty::nullable(Ty::Pending)]),
            String::new(),
        ));

        assert_eq!(
            BackendModuleFacts::from_classifiers([(classifier, shape)], []).err(),
            Some(BackendFactError::UnpublishableClassifier(
                classifier,
                UndeterminedType::Pending,
            ))
        );
    }

    #[test]
    fn backend_facts_reject_error_inside_value_class_metadata() {
        let classifier = crate::types::type_name("sample/Value");
        let mut shape = LibraryType::declaration_header();
        shape.value_underlying = Some(Ty::Error);

        assert_eq!(
            BackendModuleFacts::from_classifiers([(classifier, shape)], []).err(),
            Some(BackendFactError::UnpublishableClassifier(
                classifier,
                UndeterminedType::Error,
            ))
        );
    }

    #[test]
    fn backend_facts_project_only_the_exact_member_surface() {
        let classifier = crate::types::type_name("sample/Owner");
        let mut shape = LibraryType::declaration_header();
        let mut member = LibraryMember::new("run".to_owned(), Vec::new(), Ty::Unit, String::new());
        member.source_member = Some(SourceMember::Class {
            file: 4,
            owner: 8,
            method: 15,
        });
        member.inline_body_plan = Some(Box::new(InlineBodyPlan::InvokeLambda {
            lambda_parameter: 0,
            argument_parameters: Vec::new(),
            return_parameter: None,
        }));
        shape.members.push(member);

        let facts = BackendModuleFacts::from_classifiers([(classifier, shape)], []).unwrap();
        let frozen = facts.classifier(classifier).unwrap();
        assert_eq!(frozen.surface.len(), 1);
        assert_eq!(
            frozen.surface[0].name,
            BackendMemberName::Declared("run".into())
        );
        assert!(frozen.surface[0].params.is_empty());
        assert_eq!(frozen.surface[0].ret, Ty::Unit);
    }

    #[test]
    fn stable_index_projects_the_same_interface_backend_surface_as_module_symbols() {
        let source = r#"
            interface AuditI<T> {
                fun echo(value: T): T = value
                fun text(value: String): String = value
                val answer: Int get() = 42
            }
        "#;
        let mut diagnostics = crate::diag::DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &[crate::frontend::SourceInput::kotlin(source).with_file_stem("Audit")],
            Box::new(crate::libraries::EmptySymbolSource),
            &crate::features::LangFeatures::new(),
            &mut diagnostics,
        );
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let index = analysis
            .streamed
            .as_ref()
            .expect("streamed module")
            .module
            .index();
        let classifier = crate::types::type_name("AuditI");
        let stable = BackendModuleFacts::from_resolved_index(index)
            .unwrap()
            .classifier(classifier)
            .expect("stable classifier");
        let provider = crate::module_symbols::ModuleSymbols::new(&analysis.symbols);
        let legacy = provider.classifier(classifier).expect("module classifier");
        let legacy = BackendClassifierFact::from_library(&legacy);
        assert_eq!(*stable, legacy);
    }
}
