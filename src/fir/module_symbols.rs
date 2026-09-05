//! Finalized current-module declarations exposed to Pass-2 resolution.
//!
//! Stable classifiers, constructors, functions, properties, constants, enum entries, companions,
//! and top-level declarations are projected from [`ResolvedModuleIndex`]. This provider contains
//! no legacy source symbol table, AST declaration ids, source ranges, target accessor names, or
//! physical storage decisions.

use crate::libraries::{
    CallSig, Callables, FnFlags, FnKind, FunctionInfo, FunctionSet, GenericReturnPolicy,
    GenericSig, InlineKind, LibraryCallable, LibraryMember, MemberRealization, Origin, ParamList,
    PropKind, PropertyInfo, PropertySet, ResolvedSymbols,
};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{stored_value_ty, Ty, TypeName};

use super::{DeclarationFlags, DeclarationId, DeclarationKind, ResolvedModuleIndex};

/// Pass-2 current-module provider. Its type deliberately cannot retain the Pass-1 symbol table.
pub(crate) struct StreamedModuleSymbols<'a> {
    index: &'a ResolvedModuleIndex,
    source_file: Option<u32>,
    cache: StreamedModuleProjectionCacheRef<'a>,
}

enum StreamedModuleProjectionCacheRef<'a> {
    Owned(std::rc::Rc<StreamedModuleProjectionCache>),
    Borrowed(&'a StreamedModuleProjectionCache),
}

impl StreamedModuleProjectionCacheRef<'_> {
    fn get(&self) -> &StreamedModuleProjectionCache {
        match self {
            Self::Owned(cache) => cache,
            Self::Borrowed(cache) => cache,
        }
    }
}

/// Transient semantic projections shared by the bounded checkers of one Pass-2 source stream.
/// The cache contains no syntax, source coordinates, or temporary signature graph. Publishing a
/// body-local declaration changes the stable index size and invalidates every earlier projection.
#[derive(Default)]
pub(crate) struct StreamedModuleProjectionCache {
    declaration_count: std::cell::Cell<usize>,
    classifiers: std::cell::RefCell<
        std::collections::HashMap<TypeName, std::sync::Arc<crate::libraries::LibraryType>>,
    >,
}

impl StreamedModuleProjectionCache {
    fn prepare(&self, declaration_count: usize) {
        if self.declaration_count.get() == declaration_count {
            return;
        }
        self.classifiers.borrow_mut().clear();
        self.declaration_count.set(declaration_count);
    }
}

impl<'a> StreamedModuleSymbols<'a> {
    pub(crate) fn for_file(index: &'a ResolvedModuleIndex, source_file: u32) -> Self {
        Self {
            index,
            source_file: Some(source_file),
            cache: StreamedModuleProjectionCacheRef::Owned(std::rc::Rc::new(
                StreamedModuleProjectionCache::default(),
            )),
        }
    }

    pub(crate) fn for_file_with_cache(
        index: &'a ResolvedModuleIndex,
        source_file: u32,
        cache: &'a StreamedModuleProjectionCache,
    ) -> Self {
        cache.prepare(index.declaration_count());
        Self {
            index,
            source_file: Some(source_file),
            cache: StreamedModuleProjectionCacheRef::Borrowed(cache),
        }
    }

    pub(crate) fn declares_top_level(&self, name: &str) -> bool {
        self.index
            .declarations_named(name)
            .iter()
            .any(|declaration| {
                self.index
                    .declaration_header(*declaration)
                    .is_some_and(|header| {
                        header.kind == DeclarationKind::Function && header.owner.is_none()
                    })
            })
    }

    pub(crate) fn type_alias_expansion(&self, identity: TypeName) -> Option<(Vec<String>, Ty)> {
        let header = self.index.type_alias_by_identity(identity)?;
        Some((
            self.index.type_alias_formals(header.declaration),
            header.expansion.get(),
        ))
    }

    pub(crate) fn type_parameter_extra_bounds(&self, identity: &str) -> Vec<Ty> {
        let Some(parameter) = self.index.type_parameter_by_semantic_name(identity) else {
            return Vec::new();
        };
        self.index
            .type_parameter_header(parameter)
            .map(|header| {
                header
                    .bounds
                    .iter()
                    .skip(1)
                    .map(|bound| bound.ty.get())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn property_context_parameter_names(
        &self,
        declaration: DeclarationId,
        context_count: usize,
    ) -> Vec<String> {
        let property = self.index.property_for_declaration(declaration);
        (0..context_count)
            .map(|ordinal| {
                property
                    .and_then(|property| {
                        self.index
                            .property_context_parameter_name(property, ordinal as u32)
                    })
                    .unwrap_or("_")
                    .to_owned()
            })
            .collect()
    }

    pub(crate) fn annotation_retention(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::AnnotationRetention> {
        self.index.annotation_retention(classifier)
    }

    pub(crate) fn annotation_targets(
        &self,
        classifier: TypeName,
    ) -> crate::types::AnnotationTargets {
        self.index.annotation_targets(classifier)
    }

    /// Project one current-module classifier from finalized declaration headers.
    ///
    /// Every result is rebuilt from stable headers and declaration identities.
    fn stable_classifier(
        &self,
        internal: TypeName,
    ) -> Option<std::sync::Arc<crate::libraries::LibraryType>> {
        let cache = self.cache.get();
        cache.prepare(self.index.declaration_count());
        if let Some(classifier) = cache.classifiers.borrow().get(&internal) {
            return Some(classifier.clone());
        }
        let owner = self.index.classifier_declaration(internal)?;
        let classifier_header = self.index.classifier_header(owner)?;
        let declaration_header = self.index.declaration_header(owner)?;
        let anchor = self.index.declaration_anchor(owner)?;
        let flags = declaration_header.flags;
        let mut projected = crate::libraries::LibraryType::declaration_header();

        projected.is_kotlin = true;
        projected.access = declaration_header.visibility.into();
        projected.source_file = Some(anchor.source.raw());
        projected.stable_declaration = Some(owner);
        projected.is_nested = internal.nested_owner().is_some();
        projected.outer_instance = flags
            .has(DeclarationFlags::INNER)
            .then(|| {
                declaration_header
                    .owner
                    .and_then(|declaration| self.index.classifier_header(declaration))
                    .map(|classifier| classifier.classifier)
                    .or_else(|| internal.nested_owner())
            })
            .flatten();
        projected.kind = if flags.has(DeclarationFlags::ANNOTATION_CLASS) {
            crate::libraries::TypeKind::Annotation
        } else if flags.has(DeclarationFlags::SINGLETON) {
            crate::libraries::TypeKind::Object
        } else if flags.has(DeclarationFlags::ENUM) {
            crate::libraries::TypeKind::Enum
        } else if flags.has(DeclarationFlags::INTERFACE) {
            crate::libraries::TypeKind::Interface
        } else {
            crate::libraries::TypeKind::Class
        };
        projected.inheritance.is_abstract =
            flags.has(DeclarationFlags::ABSTRACT) || flags.has(DeclarationFlags::INTERFACE);
        projected.inheritance.is_extensible =
            !flags.has(DeclarationFlags::INTERFACE) && !flags.has(DeclarationFlags::FINAL);
        projected.sam_eligible = flags.has(DeclarationFlags::FUN_INTERFACE);
        let mut supertype_templates = classifier_header
            .superclass
            .iter()
            .chain(classifier_header.interfaces.iter())
            .map(|supertype| supertype.get())
            .collect::<Vec<_>>();
        if classifier_header.superclass.is_none() && internal != crate::types::wk::any() {
            // Kotlin's root class is implicit in source syntax, including for a class that lists
            // only interfaces. Publish that ordinary semantic edge from the source provider so the
            // core hierarchy finds Any members without a resolver or value-class special case.
            supertype_templates.push(Ty::obj_name(crate::types::wk::any()));
        }
        projected.callable_signatures = supertype_templates
            .iter()
            .copied()
            .filter(|supertype| matches!(supertype.non_null(), Ty::Fun(_)))
            .collect();
        projected.callable_signature = projected.callable_signatures.first().copied();
        projected.supertype_templates = supertype_templates;
        projected.supertypes = projected
            .supertype_templates
            .iter()
            .filter_map(|supertype| supertype.non_null().obj_internal())
            .collect::<Vec<_>>()
            .into();
        projected.sealed_subclasses = classifier_header.sealed_subclasses.to_vec().into();
        projected.companion_object =
            self.companion_classifier(owner)
                .map(|(declaration, classifier)| {
                    (
                        self.index
                            .declaration_name(declaration)
                            .unwrap_or("Companion")
                            .to_owned(),
                        classifier,
                    )
                });
        projected.constants.clear();
        projected.enum_entries.clear();
        for raw in 0..self.index.declaration_count() {
            let declaration = DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable module declarations"),
            );
            let Some(header) = self.index.declaration_header(declaration) else {
                continue;
            };
            if header.owner != Some(owner) {
                continue;
            }
            match header.kind {
                DeclarationKind::Property => {
                    if flags.has(DeclarationFlags::VALUE)
                        && header.flags.has(DeclarationFlags::PROPERTY_PARAMETER)
                    {
                        projected.value_underlying = self
                            .index
                            .signature(declaration)
                            .map(|signature| signature.result.get());
                        projected.value_underlying_property =
                            self.index.declaration_name(declaration).map(str::to_owned);
                    }
                    let Some((name, value)) = self
                        .index
                        .declaration_name(declaration)
                        .zip(self.index.compile_time_constant(declaration))
                    else {
                        continue;
                    };
                    projected.constants.insert(name.to_owned(), value.clone());
                }
                DeclarationKind::EnumEntry => {
                    if let Some(name) = self.index.declaration_name(declaration) {
                        projected.enum_entries.push(name.to_owned());
                    }
                }
                _ => {}
            }
        }

        let parameters = self
            .index
            .classifier_type_arguments(owner)
            .unwrap_or_default();
        let parameter_names = parameters
            .iter()
            .map(|parameter| {
                self.index
                    .type_parameter_semantic_name(*parameter)
                    .unwrap_or("")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        let parameter_bounds = parameters
            .iter()
            .map(|parameter| {
                self.index
                    .type_parameter_header(*parameter)
                    .map(|header| header.bounds.iter().map(|bound| bound.ty.get()).collect())
                    .unwrap_or_default()
            })
            .collect::<Vec<Vec<Ty>>>();
        let parameter_variances = parameters
            .iter()
            .map(|parameter| {
                self.index
                    .type_parameter_header(*parameter)
                    .map_or(crate::types::TypeVariance::Invariant, |header| {
                        header.flags.variance()
                    })
            })
            .collect::<Vec<_>>();
        projected.type_parameters = crate::types::TypeParameters::new(
            parameter_names.clone(),
            parameter_bounds.clone(),
            parameter_variances,
        );
        projected.own_type_parameter_count = self
            .index
            .classifier_own_type_parameter_count(owner)
            .unwrap_or_default() as usize;

        let mut declarations = (0..self.index.declaration_count())
            .filter_map(|raw| {
                let declaration = DeclarationId::from_raw(
                    u32::try_from(raw).expect("too many stable module declarations"),
                );
                let anchor = self.index.declaration_anchor(declaration)?;
                (anchor.kind == DeclarationKind::Constructor && anchor.owner == Some(owner))
                    .then_some((anchor.sibling, declaration))
            })
            .collect::<Vec<_>>();
        declarations.sort_unstable_by_key(|(sibling, _)| *sibling);
        projected.constructors.clear();
        projected
            .named_parameter_lists
            .retain(|parameters| parameters.annotation.is_some());
        let exposes_constructors = !flags.has(DeclarationFlags::INTERFACE)
            && !flags.has(DeclarationFlags::SINGLETON)
            && !flags.has(DeclarationFlags::ENUM);
        for (_, declaration) in declarations.into_iter().filter(|_| exposes_constructors) {
            let Some(signature) = self.index.signature(declaration) else {
                continue;
            };
            let Some(callable) = self.index.callable_for_declaration(declaration) else {
                continue;
            };
            let parameters = signature
                .parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect::<Vec<_>>();
            let (call_sig, reified) = self.callable_shape(callable.id, &parameters);
            let mut constructor = LibraryMember::new(
                "<init>".to_owned(),
                parameters.clone(),
                Ty::Unit,
                String::new(),
            );
            constructor.owner = Some(internal);
            constructor.call_sig = call_sig.clone();
            constructor.context_count = callable.shape.context_parameter_count as usize;
            constructor.reified = reified;
            constructor.visibility = self
                .index
                .declaration_header(declaration)
                .map_or(declaration_header.visibility, |header| header.visibility);
            constructor.annotations = self.index.declaration_annotations(declaration).to_vec();
            constructor.stable_declaration = Some(declaration);
            if !parameter_names.is_empty() {
                let type_arguments = parameters_for_classifier(&parameter_names, &parameter_bounds);
                constructor.generic_sig = Some(GenericSig {
                    formals: parameter_names.clone(),
                    formal_bounds: parameter_bounds.clone(),
                    receiver: None,
                    params: parameters.clone(),
                    ret: Ty::obj_args_name(internal, &type_arguments),
                    return_policy: GenericReturnPolicy::Exact,
                });
            }
            projected.named_parameter_lists.push(ParamList {
                visibility: constructor.visibility,
                names: call_sig.param_names.clone(),
                defaults: call_sig.param_defaults.clone(),
                types: parameters,
                recv_fun: call_sig.lambda_receiver_params.clone(),
                vararg: call_sig.vararg_index,
                annotation: None,
            });
            projected.constructors.push(constructor);
        }
        projected.inheritance.has_no_arg_constructor = projected
            .constructors
            .iter()
            .any(|constructor| constructor.call_sig.required == 0);

        let mut direct = (0..self.index.declaration_count())
            .filter_map(|raw| {
                let declaration = DeclarationId::from_raw(
                    u32::try_from(raw).expect("too many stable module declarations"),
                );
                let header = self.index.declaration_header(declaration)?;
                (header.owner == Some(owner)
                    && matches!(
                        header.kind,
                        DeclarationKind::Function | DeclarationKind::Property
                    ))
                .then_some((
                    self.index.source_order(declaration).unwrap_or(u32::MAX),
                    header.kind,
                    self.index.declaration_name(declaration)?.to_owned(),
                ))
            })
            .collect::<Vec<_>>();
        direct.sort_by_key(|(order, _, _)| *order);
        let mut direct_function_names = std::collections::HashSet::new();
        let mut direct_property_names = std::collections::HashSet::new();
        let mut ordered_names = Vec::new();
        for (_, kind, name) in direct {
            match kind {
                DeclarationKind::Function => {
                    direct_function_names.insert(name.clone());
                }
                DeclarationKind::Property => {
                    direct_property_names.insert(name.clone());
                }
                _ => unreachable!("direct callable inventory contains only functions/properties"),
            }
            if !ordered_names.contains(&name) {
                ordered_names.push(name);
            }
        }
        for name in &ordered_names {
            let (stable_functions, stable_members) = self.member_functions(
                owner,
                internal,
                flags.has(DeclarationFlags::INTERFACE),
                name,
            );
            projected.members.extend(stable_members);
            let functions = FunctionSet {
                overloads: direct_function_names
                    .contains(name)
                    .then_some(stable_functions)
                    .unwrap_or_default(),
            };
            let properties = PropertySet {
                overloads: direct_property_names
                    .contains(name)
                    .then(|| {
                        self.member_properties(
                            owner,
                            internal,
                            flags.has(DeclarationFlags::INTERFACE),
                            name,
                        )
                    })
                    .unwrap_or_default(),
            };
            let callables = Callables::from_parts(functions, properties);
            if !matches!(callables, Callables::None) {
                projected.insert_declared_callables(name.clone(), callables);
            }
        }
        let projected = std::sync::Arc::new(projected);
        cache
            .classifiers
            .borrow_mut()
            .insert(internal, projected.clone());
        Some(projected)
    }

    fn semantic_callable(
        name: &str,
        parameters: Vec<Ty>,
        result: Ty,
        receiver: Option<Ty>,
        context_count: usize,
    ) -> LibraryCallable {
        let declared_params = Some(parameters.clone().into_boxed_slice());
        LibraryCallable {
            external_identity: None,
            external_property_identity: None,
            owner: TypeName::ROOT,
            name: name.to_owned(),
            reflection_name: Some(name.to_owned()),
            compiler_intrinsic: None,
            inline_body_plan: None,
            plugin_expression: None,
            descriptor: String::new(),
            physical_params: parameters.clone(),
            params: parameters,
            ret: result,
            physical_ret: result,
            suspend: false,
            is_abstract: false,
            owner_is_interface: false,
            member_realization: MemberRealization::Dispatch,
            inline: InlineKind::None,
            default_call: false,
            vararg_elem: None,
            vararg_index: None,
            signature: None,
            origin: Origin::Module {
                facade: TypeName::ROOT,
            },
            source_receiver: receiver,
            declared_params,
            context_count,
            contract: None,
            equality_bound: None,
            generic_sig: None,
            singleton_dispatch: None,
            default_realization: None,
            constructor_realization: None,
            declared_ret: None,
        }
    }

    /// Preserve declaration existence while checking an already-invalid module. The unresolved
    /// type is created only in this transient provider projection; it is never stored in the
    /// resolved index and an invalid module never reaches FIR, lowering, metadata, or a backend.
    fn failed_property_projection(
        &self,
        declaration: DeclarationId,
        name: &str,
        kind: PropKind,
        owner: TypeName,
        receiver: Option<Ty>,
        context_count: usize,
        mutable: bool,
    ) -> PropertyInfo {
        let mut parameters = vec![Ty::Error; context_count];
        if receiver.is_some() {
            let receiver_index = if matches!(kind, PropKind::MemberExtension) {
                context_count
            } else {
                0
            };
            parameters.insert(receiver_index, Ty::Error);
        }
        let mut getter =
            Self::semantic_callable(name, parameters.clone(), Ty::Error, receiver, context_count);
        getter.owner = owner;
        let setter = mutable.then(|| {
            parameters.push(Ty::Error);
            let mut setter =
                Self::semantic_callable(name, parameters, Ty::Unit, receiver, context_count);
            setter.owner = owner;
            setter
        });
        let header = self
            .index
            .declaration_header(declaration)
            .expect("a failed property projection requires its stable declaration header");
        let setter_visibility = self
            .index
            .owned_declaration(declaration, DeclarationKind::Accessor, 1)
            .and_then(|setter| self.index.declaration_header(setter))
            .map_or(header.visibility, |setter| setter.visibility);
        PropertyInfo {
            name: name.to_owned(),
            kind,
            receiver,
            formals: Vec::new(),
            ty: Ty::Error,
            context_count,
            context_param_names: self.property_context_parameter_names(declaration, context_count),
            getter,
            setter,
            setter_visibility,
            is_const: header.flags.has(DeclarationFlags::CONST),
            compile_time_constant: None,
            visibility: header.visibility,
            owner,
            receiver_rank: 0,
            source_key: None,
            stable_declaration: Some(declaration),
            getter_declaration: None,
            setter_declaration: None,
            source_member: None,
            accessor_derived: false,
        }
    }

    /// Preserve a failed callable's declaration and source arity during diagnostic recovery. As
    /// with [`Self::failed_property_projection`], `Ty::Error` exists only in this transient symbol
    /// view of an already-invalid module; no failed signature is inserted into the finalized index
    /// or allowed to reach checked FIR and lowering.
    fn failed_function_projection(
        &self,
        declaration: DeclarationId,
        name: &str,
        kind: FnKind,
        owner: TypeName,
        receiver: Option<Ty>,
        owner_is_interface: bool,
    ) -> Option<FunctionInfo> {
        let header = self.index.declaration_header(declaration)?;
        let callable_header = self.index.callable_for_declaration(declaration)?;
        let parameter_count = self.index.callable_parameter_name_count(callable_header.id);
        let parameters = vec![Ty::Error; parameter_count];
        let context_count = callable_header.shape.context_parameter_count as usize;
        if context_count > parameters.len() {
            return None;
        }
        let (call_sig, reified) = self.callable_shape(callable_header.id, &parameters);
        let mut realized_parameters = parameters.clone();
        if let Some(receiver) = receiver {
            realized_parameters.insert(context_count, receiver);
        }
        let mut callable = Self::semantic_callable(
            name,
            realized_parameters,
            Ty::Error,
            receiver,
            context_count,
        );
        callable.owner = owner;
        callable.origin = Origin::Module { facade: owner };
        callable.owner_is_interface = owner_is_interface;
        callable.suspend = header.flags.has(DeclarationFlags::SUSPEND);
        callable.is_abstract = header.flags.has(DeclarationFlags::ABSTRACT);

        let mut function = FunctionInfo::plain(kind, receiver, callable);
        function.flags = FnFlags {
            inline: InlineKind::None,
            reified,
            suspend: header.flags.has(DeclarationFlags::SUSPEND),
            operator: header.flags.has(DeclarationFlags::OPERATOR),
            infix: header.flags.has(DeclarationFlags::INFIX),
            is_abstract: header.flags.has(DeclarationFlags::ABSTRACT),
            is_final: header.flags.has(DeclarationFlags::FINAL),
        };
        function.visibility = header.visibility;
        function.overload_rank = self.index.source_order(declaration).unwrap_or(u32::MAX);
        function.call_sig = call_sig;
        function.default_values = vec![None; parameter_count];
        function.context_count = context_count;
        function.stable_declaration = Some(declaration);
        Some(function)
    }

    fn declaration_formals(&self, declaration: DeclarationId) -> (Vec<String>, Vec<Vec<Ty>>) {
        let mut formals = Vec::new();
        let mut bounds = Vec::new();
        for ordinal in 0.. {
            let Some(parameter) = self.index.type_parameter(declaration, ordinal) else {
                break;
            };
            let Some(header) = self.index.type_parameter_header(parameter) else {
                break;
            };
            formals.push(
                self.index
                    .type_parameter_semantic_name(parameter)
                    .unwrap_or("")
                    .to_owned(),
            );
            bounds.push(header.bounds.iter().map(|bound| bound.ty.get()).collect());
        }
        (formals, bounds)
    }

    fn callable_shape(&self, callable: super::CallableId, parameters: &[Ty]) -> (CallSig, bool) {
        let mut names = Vec::with_capacity(parameters.len());
        let mut defaults = Vec::with_capacity(parameters.len());
        let mut exact = Vec::with_capacity(parameters.len());
        let mut no_infer = Vec::with_capacity(parameters.len());
        let mut implicit_integer_coercion = Vec::with_capacity(parameters.len());
        let mut vararg_index = None;
        for ordinal in 0..parameters.len() {
            let ordinal_u32 = u32::try_from(ordinal).expect("too many callable parameters");
            let Some(parameter) = self.index.callable_parameter(callable, ordinal_u32) else {
                names.push("_".to_owned());
                defaults.push(false);
                exact.push(false);
                no_infer.push(false);
                implicit_integer_coercion.push(false);
                continue;
            };
            names.push(
                self.index
                    .callable_parameter_name(callable, ordinal_u32)
                    .unwrap_or("_")
                    .to_owned(),
            );
            let flags = parameter.flags();
            defaults.push(flags.has_default());
            exact.push(flags.is_exact());
            no_infer.push(flags.is_no_infer());
            implicit_integer_coercion.push(flags.has_implicit_integer_coercion());
            if flags.is_vararg() {
                vararg_index = Some(ordinal);
            }
        }
        let trailing_defaults = if vararg_index.is_some() {
            0
        } else {
            defaults
                .iter()
                .rev()
                .take_while(|default| **default)
                .count()
        };
        let lambda_param_types = parameters
            .iter()
            .map(|parameter| match parameter.non_null() {
                Ty::Fun(signature) => signature.params.clone(),
                _ => Vec::new(),
            })
            .collect::<Vec<_>>();
        let lambda_receivers = parameters
            .iter()
            .map(|parameter| {
                matches!(parameter.non_null(), Ty::Fun(signature) if signature.has_receiver)
            })
            .collect::<Vec<_>>();
        let lambda_context_counts = parameters
            .iter()
            .map(|parameter| match parameter.non_null() {
                Ty::Fun(signature) => signature.context_count,
                _ => 0,
            })
            .collect::<Vec<_>>();
        let mut shape = CallSig::source(
            names,
            defaults,
            lambda_param_types,
            lambda_receivers,
            lambda_context_counts,
            parameters.len().saturating_sub(trailing_defaults),
            vararg_index,
        );
        shape.exact_params = exact;
        shape.no_infer_params = no_infer;
        shape.implicit_integer_coercion = implicit_integer_coercion;
        let reified = self.index.callable(callable).is_some_and(|callable| {
            (0..)
                .map_while(|ordinal| self.index.type_parameter(callable.declaration, ordinal))
                .filter_map(|parameter| self.index.type_parameter_header(parameter))
                .any(|parameter| parameter.flags.is_reified())
        });
        (shape, reified)
    }

    /// Project direct source methods of one classifier from the finalized index. The returned
    /// `FunctionInfo` is the ordinary resolver surface; `LibraryMember` is the equivalent direct
    /// classifier surface used by hierarchy/capability queries. Both are built from the same stable
    /// declaration so they cannot disagree after the legacy `ClassSig` is released.
    fn member_functions(
        &self,
        owner: DeclarationId,
        internal: TypeName,
        is_interface: bool,
        name: &str,
    ) -> (Vec<FunctionInfo>, Vec<LibraryMember>) {
        let mut functions = Vec::new();
        let mut members = Vec::new();
        for &declaration in self.index.declarations_named(name) {
            let Some(header) = self.index.declaration_header(declaration) else {
                continue;
            };
            if header.kind != DeclarationKind::Function || header.owner != Some(owner) {
                continue;
            }
            let Some(callable_header) = self.index.callable_for_declaration(declaration) else {
                continue;
            };
            let Some(signature) = self.index.signature(declaration) else {
                if let Some(function) = self.failed_function_projection(
                    declaration,
                    name,
                    if callable_header.shape.extension_receiver.is_some() {
                        FnKind::Extension
                    } else {
                        FnKind::Member
                    },
                    internal,
                    callable_header
                        .shape
                        .extension_receiver
                        .map(|receiver| receiver.get()),
                    is_interface,
                ) {
                    functions.push(function);
                }
                continue;
            };
            let parameters = signature
                .parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect::<Vec<_>>();
            let result = signature.result.get();
            let context_count = callable_header.shape.context_parameter_count as usize;
            if context_count > parameters.len() {
                continue;
            }
            let receiver = callable_header
                .shape
                .extension_receiver
                .map(|receiver| receiver.get());
            let (call_sig, reified) = self.callable_shape(callable_header.id, &parameters);
            let behavior = self.index.callable_behavior(callable_header.id);
            let (formals, formal_bounds) = self.declaration_formals(declaration);
            let generic_sig = (!formals.is_empty()
                || receiver.is_some_and(Ty::mentions_ty_param)
                || parameters.iter().copied().any(Ty::mentions_ty_param)
                || result.mentions_ty_param())
            .then(|| GenericSig {
                formals,
                formal_bounds,
                receiver,
                params: parameters.clone(),
                ret: result,
                return_policy: GenericReturnPolicy::Exact,
            });

            let mut realized_parameters = parameters.clone();
            if let Some(receiver) = receiver {
                realized_parameters.insert(context_count, receiver);
            }
            let mut callable = Self::semantic_callable(
                name,
                realized_parameters.clone(),
                result,
                receiver,
                context_count,
            );
            callable.owner = internal;
            callable.origin = Origin::Module { facade: internal };
            callable.owner_is_interface = is_interface;
            callable.suspend = header.flags.has(DeclarationFlags::SUSPEND);
            callable.is_abstract = header.flags.has(DeclarationFlags::ABSTRACT);
            callable.inline =
                InlineKind::from_flags(callable_header.is_inline(), behavior.requires_splice);
            callable.plugin_expression = behavior.plugin_expression;
            callable.contract = self
                .index
                .contract(declaration)
                .map(|contract| contract.to_arc());
            callable.equality_bound = self
                .index
                .callable_equality_bound(callable_header.id)
                .map(|bound| bound.get());
            callable.generic_sig = generic_sig.clone().map(Box::new);

            let mut function = FunctionInfo::plain(
                if receiver.is_some() {
                    FnKind::Extension
                } else {
                    FnKind::Member
                },
                receiver,
                callable,
            );
            function.flags = FnFlags {
                inline: InlineKind::from_flags(
                    callable_header.is_inline(),
                    behavior.requires_splice,
                ),
                reified,
                suspend: header.flags.has(DeclarationFlags::SUSPEND),
                operator: header.flags.has(DeclarationFlags::OPERATOR),
                infix: header.flags.has(DeclarationFlags::INFIX),
                is_abstract: header.flags.has(DeclarationFlags::ABSTRACT),
                is_final: header.flags.has(DeclarationFlags::FINAL),
            };
            function.visibility = header.visibility;
            function.overload_rank = self.index.source_order(declaration).unwrap_or(u32::MAX);
            function.generic_sig = generic_sig.clone();
            function.projected_return_hazard = behavior.projected_return_hazard;
            function.call_sig = call_sig.clone();
            function.default_values = vec![None; parameters.len()];
            function.context_count = context_count;
            function.stable_declaration = Some(declaration);
            function.annotations = self.index.declaration_annotations(declaration).to_vec();
            functions.push(function);

            let mut member =
                LibraryMember::new(name.to_owned(), realized_parameters, result, String::new());
            member.owner = Some(internal);
            member.generic_sig = generic_sig;
            member.set_is_interface(is_interface);
            member.set_is_abstract(header.flags.has(DeclarationFlags::ABSTRACT));
            member.set_is_final(header.flags.has(DeclarationFlags::FINAL));
            member.set_suspend(header.flags.has(DeclarationFlags::SUSPEND));
            member.set_is_operator(header.flags.has(DeclarationFlags::OPERATOR));
            member.set_is_infix(header.flags.has(DeclarationFlags::INFIX));
            member.set_is_member_extension(receiver.is_some());
            member.inline =
                InlineKind::from_flags(callable_header.is_inline(), behavior.requires_splice);
            member.reified = reified;
            member.visibility = header.visibility;
            member.call_sig = call_sig;
            member.context_count = context_count;
            member.annotations = self.index.declaration_annotations(declaration).to_vec();
            member.contract = self
                .index
                .contract(declaration)
                .map(|contract| contract.to_arc());
            member.equality_bound = self
                .index
                .callable_equality_bound(callable_header.id)
                .map(|bound| bound.get());
            member.default_values = vec![None; parameters.len()];
            member.plugin_expression = behavior.plugin_expression;
            member.stable_declaration = Some(declaration);
            members.push(member);
        }
        functions.sort_by_key(|function| function.overload_rank);
        members.sort_by_key(|member| {
            member
                .stable_declaration
                .and_then(|declaration| self.index.source_order(declaration))
                .unwrap_or(u32::MAX)
        });
        (functions, members)
    }

    /// Project one source function by stable declaration identity. Enum-entry subclasses have no
    /// source classifier declaration of their own, so their methods are not reachable through an
    /// ordinary classifier member lookup; Pass 2 still needs the exact finalized callable shape
    /// when constructing that entry's lexical body scope.
    pub(crate) fn function_for_declaration(
        &self,
        declaration: DeclarationId,
        owner_internal: TypeName,
        owner_is_interface: bool,
    ) -> Option<FunctionInfo> {
        let header = self.index.declaration_header(declaration)?;
        let owner = header.owner?;
        let name = self.index.declaration_name(declaration)?;
        self.member_functions(owner, owner_internal, owner_is_interface, name)
            .0
            .into_iter()
            .find(|function| function.stable_declaration == Some(declaration))
    }

    fn member_properties(
        &self,
        owner: DeclarationId,
        internal: TypeName,
        is_interface: bool,
        name: &str,
    ) -> Vec<PropertyInfo> {
        let mut properties = Vec::new();
        for &declaration in self.index.declarations_named(name) {
            let Some(header) = self.index.declaration_header(declaration) else {
                continue;
            };
            if header.kind != DeclarationKind::Property || header.owner != Some(owner) {
                continue;
            }
            let property = self
                .index
                .property_for_declaration(declaration)
                .and_then(|property| self.index.property(property));
            let Some(signature) = self.index.signature(declaration) else {
                let receiver = property
                    .and_then(|property| property.extension_receiver)
                    .map(|receiver| receiver.get());
                properties.push(self.failed_property_projection(
                    declaration,
                    name,
                    if receiver.is_some() {
                        PropKind::MemberExtension
                    } else {
                        PropKind::Member
                    },
                    internal,
                    receiver,
                    property.map_or(0, |property| property.context_parameter_count as usize),
                    property.is_some_and(|property| property.mutable),
                ));
                continue;
            };
            let Some(property) = property else {
                continue;
            };
            let context_count = property.context_parameter_count as usize;
            if context_count > signature.parameters.len() {
                continue;
            }
            let context_parameters = signature.parameters[..context_count]
                .iter()
                .map(|parameter| parameter.get())
                .collect::<Vec<_>>();
            let receiver = property.extension_receiver.map(|receiver| receiver.get());
            let public_ty = signature.result.get();
            let (formals, formal_bounds) = self.declaration_formals(declaration);

            let mut getter_parameters = context_parameters.clone();
            if let Some(receiver) = receiver {
                getter_parameters.insert(context_count, receiver);
            }
            let mut getter = Self::semantic_callable(
                name,
                getter_parameters.clone(),
                public_ty,
                receiver,
                context_count,
            );
            getter.owner = internal;
            getter.origin = Origin::Module { facade: internal };
            getter.owner_is_interface = is_interface;
            getter.is_abstract = header.flags.has(DeclarationFlags::ABSTRACT);
            let mut setter = property.mutable.then(|| {
                let mut parameters = getter_parameters.clone();
                parameters.push(stored_value_ty(public_ty));
                let mut setter =
                    Self::semantic_callable(name, parameters, Ty::Unit, receiver, context_count);
                setter.owner = internal;
                setter.origin = Origin::Module { facade: internal };
                setter.owner_is_interface = is_interface;
                setter.is_abstract = header.flags.has(DeclarationFlags::ABSTRACT);
                setter
            });
            if !formals.is_empty()
                || receiver.is_some_and(Ty::mentions_ty_param)
                || context_parameters
                    .iter()
                    .copied()
                    .any(Ty::mentions_ty_param)
                || public_ty.mentions_ty_param()
            {
                let generic = GenericSig {
                    formals: formals.clone(),
                    formal_bounds,
                    receiver,
                    params: context_parameters.clone(),
                    ret: public_ty,
                    return_policy: GenericReturnPolicy::Exact,
                };
                getter.generic_sig = Some(Box::new(generic.clone()));
                if let Some(setter) = &mut setter {
                    let mut setter_generic = generic;
                    setter_generic.params.push(public_ty);
                    setter_generic.ret = Ty::Unit;
                    setter.generic_sig = Some(Box::new(setter_generic));
                }
            }
            let setter_visibility = self
                .index
                .owned_declaration(declaration, DeclarationKind::Accessor, 1)
                .and_then(|setter| self.index.declaration_header(setter))
                .map_or(header.visibility, |setter| setter.visibility);
            properties.push(PropertyInfo {
                name: name.to_owned(),
                kind: if receiver.is_some() {
                    PropKind::MemberExtension
                } else {
                    PropKind::Member
                },
                receiver,
                formals,
                // Property lookup exposes the declared Kotlin type. A narrower explicit backing
                // field is storage visible only while checking the declaring body's lexical
                // `field`/own-property access and is already published separately on PropertyHeader.
                ty: public_ty,
                context_count,
                context_param_names: self
                    .property_context_parameter_names(declaration, context_count),
                getter,
                setter,
                setter_visibility,
                is_const: header.flags.has(DeclarationFlags::CONST),
                compile_time_constant: self.index.compile_time_constant(declaration).cloned(),
                visibility: header.visibility,
                owner: internal,
                receiver_rank: 0,
                source_key: None,
                stable_declaration: Some(declaration),
                getter_declaration: None,
                setter_declaration: None,
                source_member: None,
                accessor_derived: false,
            });
        }
        properties.sort_by_key(|property| {
            property
                .stable_declaration
                .and_then(|declaration| self.index.source_order(declaration))
                .unwrap_or(u32::MAX)
        });
        properties
    }

    fn companion_classifier(&self, owner: DeclarationId) -> Option<(DeclarationId, TypeName)> {
        (0..self.index.declaration_count()).find_map(|raw| {
            let declaration = DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable module declarations"),
            );
            let header = self.index.declaration_header(declaration)?;
            (header.kind == DeclarationKind::Classifier
                && header.owner == Some(owner)
                && header.flags.has(DeclarationFlags::COMPANION))
            .then(|| {
                self.index
                    .classifier_header(declaration)
                    .map(|classifier| (declaration, classifier.classifier))
            })
            .flatten()
        })
    }

    /// Project declarations accessed through a classifier value (`Limits.MAX`, `C.make()`) from
    /// the stable companion classifier. The declaration remains owned by the companion and carries
    /// an explicit singleton dispatch; the outer classifier is only the source lookup namespace.
    fn associated_companion_callables(
        &self,
        owner: TypeName,
        name: &str,
    ) -> (Vec<FunctionInfo>, Vec<PropertyInfo>) {
        let Some(owner_declaration) = self.index.classifier_declaration(owner) else {
            return (Vec::new(), Vec::new());
        };
        let Some((companion_declaration, companion)) = self.companion_classifier(owner_declaration)
        else {
            return (Vec::new(), Vec::new());
        };
        let singleton = crate::libraries::SingletonDispatch {
            classifier: companion,
        };
        let (mut functions, _) =
            self.member_functions(companion_declaration, companion, false, name);
        for function in &mut functions {
            if function.kind == FnKind::Member {
                function.kind = FnKind::TopLevel;
                function.receiver = None;
            }
            function.callable.singleton_dispatch = Some(Box::new(singleton.clone()));
        }
        let mut properties = self.member_properties(companion_declaration, companion, false, name);
        for property in &mut properties {
            match property.kind {
                PropKind::Member => {
                    property.kind = PropKind::TopLevel;
                    property.receiver = None;
                }
                PropKind::MemberExtension => property.kind = PropKind::Extension,
                PropKind::Extension | PropKind::TopLevel => {}
            }
            property.getter.singleton_dispatch = Some(Box::new(singleton.clone()));
            if let Some(setter) = &mut property.setter {
                setter.singleton_dispatch = Some(Box::new(singleton.clone()));
            }
        }
        (functions, properties)
    }

    fn top_level_functions(&self, namespace: SymbolNamespace, name: &str) -> Vec<FunctionInfo> {
        let package = match namespace {
            SymbolNamespace::Package(package) => Some(package),
            SymbolNamespace::Classifier(_) => None,
        };
        let associated_owner = match namespace {
            SymbolNamespace::Classifier(owner) => Some(owner),
            SymbolNamespace::Package(_) => None,
        };
        let mut functions = Vec::new();
        for &declaration in self.index.declarations_named(name) {
            let Some(header) = self.index.declaration_header(declaration) else {
                continue;
            };
            if header.kind != DeclarationKind::Function || header.owner.is_some() {
                continue;
            }
            let Some(anchor) = self.index.declaration_anchor(declaration) else {
                continue;
            };
            let declaration_package = self
                .index
                .source_package(anchor.source)
                .unwrap_or(TypeName::ROOT);
            let Some(callable_header) = self.index.callable_for_declaration(declaration) else {
                continue;
            };
            let receiver = callable_header
                .shape
                .extension_receiver
                .map(|receiver| receiver.get());
            let companion_extension = header.flags.has(DeclarationFlags::COMPANION);
            let imported_associated = associated_owner.is_some_and(|owner| {
                companion_extension
                    && receiver.and_then(|receiver| receiver.non_null().obj_internal())
                        == Some(owner)
            });
            if package != Some(declaration_package) && !imported_associated {
                continue;
            }
            let Some(signature) = self.index.signature(declaration) else {
                let selected_receiver = (!imported_associated).then_some(receiver).flatten();
                let kind = if imported_associated || receiver.is_none() {
                    FnKind::TopLevel
                } else {
                    FnKind::Extension
                };
                if let Some(function) = self.failed_function_projection(
                    declaration,
                    name,
                    kind,
                    TypeName::ROOT,
                    selected_receiver,
                    false,
                ) {
                    functions.push(function);
                }
                continue;
            };
            let parameters = signature
                .parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect::<Vec<_>>();
            let result = signature.result.get();
            let context_count = callable_header.shape.context_parameter_count as usize;
            if context_count > parameters.len() {
                continue;
            }
            let (call_sig, reified) = self.callable_shape(callable_header.id, &parameters);
            let behavior = self.index.callable_behavior(callable_header.id);
            let (formals, formal_bounds) = self.declaration_formals(declaration);
            let generic_sig = (!formals.is_empty()).then(|| GenericSig {
                formals,
                formal_bounds,
                receiver,
                params: parameters.clone(),
                ret: result,
                return_policy: GenericReturnPolicy::Exact,
            });
            let mut realized_parameters = parameters.clone();
            if let Some(receiver) = receiver {
                realized_parameters.insert(context_count.min(realized_parameters.len()), receiver);
            }
            let selected_receiver = (!imported_associated).then_some(receiver).flatten();
            let kind = if imported_associated {
                FnKind::TopLevel
            } else if receiver.is_some() {
                FnKind::Extension
            } else {
                FnKind::TopLevel
            };
            let mut callable =
                Self::semantic_callable(name, realized_parameters, result, receiver, context_count);
            callable.suspend = header.flags.has(DeclarationFlags::SUSPEND);
            callable.is_abstract = header.flags.has(DeclarationFlags::ABSTRACT);
            callable.inline =
                InlineKind::from_flags(callable_header.is_inline(), behavior.requires_splice);
            callable.plugin_expression = behavior.plugin_expression;
            callable.contract = self
                .index
                .contract(declaration)
                .map(|contract| contract.to_arc());
            callable.equality_bound = self
                .index
                .callable_equality_bound(callable_header.id)
                .map(|bound| bound.get());
            callable.generic_sig = generic_sig.clone().map(Box::new);
            if companion_extension && receiver.is_some() {
                let receiver_index = context_count.min(callable.physical_params.len());
                callable.physical_params.remove(receiver_index);
            }
            if imported_associated && receiver.is_some() {
                let receiver_index = context_count.min(callable.params.len());
                callable.params.remove(receiver_index);
            }
            let any = Ty::obj("kotlin/Any");
            let receiver_rank = if receiver.is_some_and(|receiver| {
                receiver.non_null().is_ty_param() || receiver.non_null() == any
            }) {
                1
            } else {
                0
            };
            let mut function = FunctionInfo::plain(kind, selected_receiver, callable);
            function.companion_extension = companion_extension;
            function.flags = FnFlags {
                inline: InlineKind::from_flags(
                    callable_header.is_inline(),
                    behavior.requires_splice,
                ),
                reified,
                suspend: header.flags.has(DeclarationFlags::SUSPEND),
                operator: header.flags.has(DeclarationFlags::OPERATOR),
                infix: header.flags.has(DeclarationFlags::INFIX),
                is_abstract: header.flags.has(DeclarationFlags::ABSTRACT),
                is_final: header.flags.has(DeclarationFlags::FINAL),
            };
            function.visibility = header.visibility;
            function.receiver_rank = receiver_rank;
            function.overload_rank = self.index.source_order(declaration).unwrap_or(u32::MAX);
            function.generic_sig = generic_sig;
            function.projected_return_hazard = behavior.projected_return_hazard;
            function.call_sig = call_sig;
            function.default_values = vec![None; parameters.len()];
            function.context_count = context_count;
            function.stable_declaration = Some(declaration);
            function.annotations = self.index.declaration_annotations(declaration).to_vec();
            functions.push(function);
        }
        functions.sort_by_key(|function| function.overload_rank);
        functions
    }

    fn top_level_properties(&self, namespace: SymbolNamespace, name: &str) -> Vec<PropertyInfo> {
        let package = match namespace {
            SymbolNamespace::Package(package) => Some(package),
            SymbolNamespace::Classifier(_) => None,
        };
        let associated_owner = match namespace {
            SymbolNamespace::Classifier(owner) => Some(owner),
            SymbolNamespace::Package(_) => None,
        };
        let mut properties = Vec::new();
        for &declaration in self.index.declarations_named(name) {
            let Some(header) = self.index.declaration_header(declaration) else {
                continue;
            };
            if header.kind != DeclarationKind::Property || header.owner.is_some() {
                continue;
            }
            let Some(anchor) = self.index.declaration_anchor(declaration) else {
                continue;
            };
            let declaration_package = self
                .index
                .source_package(anchor.source)
                .unwrap_or(TypeName::ROOT);
            let property = self
                .index
                .property_for_declaration(declaration)
                .and_then(|property| self.index.property(property));
            let receiver = property
                .and_then(|property| property.extension_receiver)
                .map(|receiver| receiver.get());
            let companion_extension = header.flags.has(DeclarationFlags::COMPANION);
            let imported_associated = associated_owner.is_some_and(|owner| {
                companion_extension
                    && receiver.and_then(|receiver| receiver.non_null().obj_internal())
                        == Some(owner)
            });
            if !package.is_some_and(|package| package == declaration_package)
                && !imported_associated
            {
                continue;
            }
            if header.visibility.is_private() && self.source_file != Some(anchor.source.raw()) {
                continue;
            }
            let Some(signature) = self.index.signature(declaration) else {
                properties.push(self.failed_property_projection(
                    declaration,
                    name,
                    if imported_associated {
                        PropKind::TopLevel
                    } else if receiver.is_some() {
                        PropKind::Extension
                    } else {
                        PropKind::TopLevel
                    },
                    TypeName::ROOT,
                    (!imported_associated).then_some(receiver).flatten(),
                    property.map_or(0, |property| property.context_parameter_count as usize),
                    property.is_some_and(|property| property.mutable),
                ));
                continue;
            };
            let Some(property) = property else {
                continue;
            };
            let context_count = property.context_parameter_count as usize;
            if context_count > signature.parameters.len() {
                continue;
            }
            let context_parameters = signature.parameters[..context_count]
                .iter()
                .map(|parameter| parameter.get())
                .collect::<Vec<_>>();
            let public_ty = signature.result.get();
            let read_ty = (receiver.is_none() && self.source_file == Some(anchor.source.raw()))
                .then(|| property.storage_type.map(|storage| storage.get()))
                .flatten()
                .unwrap_or(public_ty);
            let (formals, formal_bounds) = self.declaration_formals(declaration);

            // These temporary handles describe only semantic parameter/result layout. Their name is
            // the Kotlin property identity; a target backend realizes getter/setter spelling from
            // the stable PropertyId after checked FIR has selected the declaration.
            let mut getter_parameters = receiver.into_iter().collect::<Vec<_>>();
            getter_parameters.extend(context_parameters.iter().copied());
            let mut getter = Self::semantic_callable(
                name,
                getter_parameters.clone(),
                public_ty,
                receiver,
                context_count,
            );
            let mut setter = property.mutable.then(|| {
                let mut parameters = getter_parameters.clone();
                parameters.push(stored_value_ty(public_ty));
                Self::semantic_callable(name, parameters, Ty::Unit, receiver, context_count)
            });
            if !formals.is_empty() {
                let generic = GenericSig {
                    formals: formals.clone(),
                    formal_bounds,
                    receiver,
                    params: context_parameters.clone(),
                    ret: public_ty,
                    return_policy: GenericReturnPolicy::Exact,
                };
                getter.generic_sig = Some(Box::new(generic.clone()));
                if let Some(setter) = &mut setter {
                    let mut setter_generic = generic;
                    setter_generic.params.push(public_ty);
                    setter_generic.ret = Ty::Unit;
                    setter.generic_sig = Some(Box::new(setter_generic));
                }
            }
            if companion_extension {
                if receiver.is_some() && !getter.physical_params.is_empty() {
                    getter.physical_params.remove(0);
                }
                if let Some(setter) = &mut setter {
                    if receiver.is_some() && !setter.physical_params.is_empty() {
                        setter.physical_params.remove(0);
                    }
                }
            }
            if imported_associated {
                if receiver.is_some() && !getter.params.is_empty() {
                    getter.params.remove(0);
                }
                if let Some(setter) = &mut setter {
                    if receiver.is_some() && !setter.params.is_empty() {
                        setter.params.remove(0);
                    }
                }
            }
            let setter_visibility = self
                .index
                .owned_declaration(declaration, DeclarationKind::Accessor, 1)
                .and_then(|setter| self.index.declaration_header(setter))
                .map_or(header.visibility, |setter| setter.visibility);
            properties.push(PropertyInfo {
                name: name.to_owned(),
                kind: if imported_associated {
                    PropKind::TopLevel
                } else if receiver.is_some() {
                    PropKind::Extension
                } else {
                    PropKind::TopLevel
                },
                receiver: (!imported_associated).then_some(receiver).flatten(),
                formals,
                ty: read_ty,
                context_count,
                context_param_names: self
                    .property_context_parameter_names(declaration, context_count),
                getter,
                setter,
                setter_visibility,
                is_const: header.flags.has(DeclarationFlags::CONST),
                compile_time_constant: self.index.compile_time_constant(declaration).cloned(),
                visibility: header.visibility,
                owner: TypeName::ROOT,
                receiver_rank: 0,
                source_key: None,
                stable_declaration: Some(declaration),
                getter_declaration: None,
                setter_declaration: None,
                source_member: None,
                accessor_derived: false,
            });
        }
        properties.sort_by_key(|property| {
            property
                .stable_declaration
                .and_then(|declaration| self.index.source_order(declaration))
                .unwrap_or(u32::MAX)
        });
        properties
    }
}

impl SymbolSource for StreamedModuleSymbols<'_> {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        self.index.source_package_child_exists(parent, name)
    }

    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
        let stable_classifier_name = namespace
            .existing_classifier(name)
            .filter(|internal| self.index.classifier_declaration(*internal).is_some());
        let classifier =
            stable_classifier_name.and_then(|internal| self.stable_classifier(internal));
        let mut functions = FunctionSet::default();
        let mut properties = PropertySet::default();
        functions
            .overloads
            .extend(self.top_level_functions(namespace, name));
        properties
            .overloads
            .extend(self.top_level_properties(namespace, name));
        if let SymbolNamespace::Classifier(owner) = namespace {
            let (associated_functions, associated_properties) =
                self.associated_companion_callables(owner, name);
            functions.overloads.extend(associated_functions);
            properties.overloads.extend(associated_properties);
        }
        let callables = Callables::from_parts(
            FunctionSet {
                overloads: functions.overloads,
            },
            PropertySet {
                overloads: properties.overloads,
            },
        );
        std::rc::Rc::new(ResolvedSymbols {
            classifier_name: stable_classifier_name,
            classifier,
            callables,
            importable_declaration: stable_classifier_name.is_some(),
        })
    }
}

fn parameters_for_classifier(names: &[String], bounds: &[Vec<Ty>]) -> Vec<Ty> {
    names
        .iter()
        .enumerate()
        .map(|(ordinal, name)| {
            Ty::ty_param(
                name,
                bounds
                    .get(ordinal)
                    .and_then(|bounds| bounds.first())
                    .copied()
                    .unwrap_or_else(|| Ty::obj("kotlin/Any")),
            )
        })
        .collect()
}
