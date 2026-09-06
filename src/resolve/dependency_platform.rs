use crate::libraries::{
    Callables, FunctionInfo, FunctionSet, LibraryMember, LibraryType, PropKind, PropertyInfo,
    PropertySet, ResolvedSymbols, SemanticPlatform, SemanticSupertype,
};
use crate::module_symbols::ModuleSymbols;
use crate::name_tree::FxHashMap;
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{Ty, TypeName, Visibility};
use std::rc::Rc;

use super::SymbolTable;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceTypeAccess {
    /// Neither the requested classifier nor a restrictive source owner claims this JVM path, so
    /// the compiled platform remains authoritative.
    Absent,
    /// The source declares the classifier and every lexical owner is public. The declaration's
    /// own visibility is retained because protected nested classifiers remain available through
    /// inheritance even though ordinary dependency lookup must not expose them.
    Declared(Visibility),
    /// A non-public source owner claims the path before the requested classifier. This includes an
    /// absent compiled descendant below that owner: falling through to an equally named platform
    /// descendant would cross the source visibility boundary. Retaining the owner's visibility
    /// also keeps the classifier record's access metadata consistent with its declaration path.
    HiddenByOwner(Visibility),
}

impl SourceTypeAccess {
    /// Visibility claimed by the source side of the federation. For an owner-blocked path this is
    /// deliberately the blocking owner's visibility, not a public platform leaf hidden behind it.
    fn source_visibility(self) -> Option<Visibility> {
        match self {
            Self::Declared(visibility) | Self::HiddenByOwner(visibility) => Some(visibility),
            Self::Absent => None,
        }
    }
}

pub(crate) struct DependencyPlatform {
    platform: Box<dyn SemanticPlatform>,
    symbols: SymbolTable,
    /// Memoized merges over the immutable platform and source symbol tables.
    symbols_memo:
        std::cell::RefCell<FxHashMap<SymbolNamespace, FxHashMap<String, Rc<ResolvedSymbols>>>>,
}

impl DependencyPlatform {
    pub(crate) fn new(platform: Box<dyn SemanticPlatform>, symbols: SymbolTable) -> Self {
        DependencyPlatform {
            platform,
            symbols,
            symbols_memo: Default::default(),
        }
    }

    fn source(&self) -> ModuleSymbols<'_> {
        ModuleSymbols::new(&self.symbols)
    }

    fn source_alias_expansion(
        &self,
        identity: TypeName,
    ) -> Option<crate::libraries::AliasExpansion> {
        let (formals, expansion) = self.symbols.source_alias_expansions.get(&identity)?;
        let target = expansion.non_null().kotlin_class_internal().or_else(|| {
            expansion
                .fun_arity()
                .and_then(|arity| self.platform.function_type(usize::from(arity)))
                .and_then(Ty::obj_internal)
        })?;
        let expansion_spelling = self
            .symbols
            .alias_expansion_spellings
            .get(&identity)
            .map(|(spelling, _, _)| spelling.clone())
            .unwrap_or_default();
        Some(crate::libraries::AliasExpansion {
            identity,
            target,
            formals: formals.clone(),
            expansion: *expansion,
            expansion_spelling,
        })
    }

    fn source_type_access(&self, internal: TypeName) -> SourceTypeAccess {
        let source = self.source();
        let leaf_visibility = source
            .classifier(internal)
            .map(|classifier| classifier.access.visibility());
        let mut enclosing = internal.nested_owner();
        while let Some(owner) = enclosing {
            match source
                .classifier(owner)
                .map(|classifier| classifier.access.visibility())
            {
                Some(Visibility::Public) => {}
                Some(visibility) => return SourceTypeAccess::HiddenByOwner(visibility),
                // A source-declared nested classifier must have a complete lexical owner chain.
                // If a partial index ever violates that invariant, fail closed instead of exposing
                // a same-named platform class through the gap.
                None if leaf_visibility.is_some() => {
                    return SourceTypeAccess::HiddenByOwner(Visibility::Private);
                }
                None => {}
            }
            enclosing = owner.nested_owner();
        }
        leaf_visibility.map_or(SourceTypeAccess::Absent, SourceTypeAccess::Declared)
    }
}

fn same_member(left: &LibraryMember, right: &LibraryMember) -> bool {
    left.name == right.name && left.params == right.params
}

fn same_function(
    platform: &dyn SemanticPlatform,
    left: &FunctionInfo,
    right: &FunctionInfo,
) -> bool {
    left.kind == right.kind
        && left.callable.name == right.callable.name
        && left.callable.suspend == right.callable.suspend
        && left.callable.params.len() == right.callable.params.len()
        && left
            .callable
            .params
            .iter()
            .zip(&right.callable.params)
            .all(
                |(&left, &right)| match (left.non_null(), right.non_null()) {
                    (Ty::Fun(left), Ty::Fun(right)) => {
                        left.params.len() == right.params.len() && left.suspend == right.suspend
                    }
                    _ => {
                        platform.library_value_form(left.erased_recv())
                            == platform.library_value_form(right.erased_recv())
                    }
                },
            )
}

fn property_covers(
    platform: &dyn SemanticPlatform,
    primary: &PropertyInfo,
    source: &PropertyInfo,
) -> bool {
    if primary.kind != source.kind
        || primary.receiver != source.receiver
        || primary.visibility != Visibility::Public
        || (source.setter.is_some() && primary.setter.is_none())
    {
        return false;
    }
    primary.kind != PropKind::Member
        || (primary.owner == source.owner
            && platform
                .classifier(primary.owner)
                .is_some_and(|owner| owner.is_public()))
}

fn same_property_declaration(left: &PropertyInfo, right: &PropertyInfo) -> bool {
    left.kind == right.kind && left.receiver == right.receiver && left.owner == right.owner
}

fn merge_functions(
    platform: &dyn SemanticPlatform,
    mut primary: FunctionSet,
    source: FunctionSet,
) -> FunctionSet {
    for candidate in source.overloads {
        if let Some(existing) = primary
            .overloads
            .iter()
            .position(|existing| same_function(platform, existing, &candidate))
        {
            crate::trace_compiler!(
                "resolve",
                "merge dependency function owner={:?} name={} primary_flags={:?} source_flags={:?}",
                candidate.callable.owner,
                candidate.callable.name,
                primary.overloads[existing].flags,
                candidate.flags,
            );
            // Keep the primary provider's physical callable identity, descriptor, and realization,
            // but restore declaration semantics from the dependency source. A compiled dependency
            // projection may omit Kotlin-only capabilities such as `operator`/`inline`; dropping the
            // matching source candidate made ordinary calls work while language conventions (most
            // visibly delegated properties) disappeared.
            let existing = &mut primary.overloads[existing];
            existing.companion_extension = candidate.companion_extension;
            existing.receiver = candidate.receiver.or(existing.receiver);
            existing.flags = candidate.flags;
            existing.visibility = candidate.visibility;
            existing.generic_sig = candidate
                .generic_sig
                .clone()
                .or(existing.generic_sig.clone());
            existing.projected_return_hazard = candidate.projected_return_hazard;
            existing.call_sig = candidate.call_sig.clone();
            existing.default_values = candidate.default_values.clone();
            existing.context_count = candidate.context_count;
            existing.annotations = candidate.annotations.clone();
            existing.callable.inline = candidate.flags.inline;
            existing.callable.suspend = candidate.flags.suspend;
            existing.callable.is_abstract = candidate.flags.is_abstract;
            existing.callable.context_count = candidate.context_count;
            existing.callable.equality_bound = candidate
                .callable
                .equality_bound
                .or(existing.callable.equality_bound);
            existing.callable.source_receiver = candidate
                .callable
                .source_receiver
                .or(existing.callable.source_receiver);
            if let Some(generic) = &candidate.generic_sig {
                existing.callable.generic_sig = Some(Box::new(generic.clone()));
            }
        } else {
            primary.overloads.push(candidate);
        }
    }
    primary
}

fn merge_properties(
    platform: &dyn SemanticPlatform,
    mut primary: PropertySet,
    source: PropertySet,
) -> PropertySet {
    for candidate in source.overloads {
        if primary
            .overloads
            .iter()
            .any(|existing| property_covers(platform, existing, &candidate))
        {
            continue;
        }
        primary
            .overloads
            .retain(|existing| !same_property_declaration(existing, &candidate));
        primary.overloads.push(candidate);
    }
    primary
}

fn merge_type(
    platform: &dyn SemanticPlatform,
    mut primary: LibraryType,
    source: LibraryType,
) -> LibraryType {
    // The source signature is the semantic declaration and therefore owns its declaration order;
    // the platform copy contributes physical realization. Preserve any platform-only direct
    // declarations afterwards in their own provider order without consulting either hash table.
    let primary_order = std::mem::take(&mut primary.declared_callable_order);
    primary.declared_callable_order = source.declared_callable_order.clone();
    for name in primary_order {
        if !primary.declared_callable_order.contains(&name) {
            primary.declared_callable_order.push(name);
        }
    }
    primary.access = source.access;
    primary.source_file = source.source_file;
    if !source.type_params.is_empty() {
        primary.type_parameters = source.type_parameters.clone();
    }
    // Enum entries are source declarations, not physical fields or accessor artifacts. The stable
    // source classifier therefore owns the complete ordered entry list, including the meaningful
    // empty list of an enum with no entries. The primary provider contributes only the optional
    // physical `entries` accessor below.
    primary.enum_entries = source.enum_entries.clone();
    if primary.enum_entries_accessor.is_none() {
        primary.enum_entries_accessor = source.enum_entries_accessor.clone();
    }
    for candidate in source.constructors {
        if !primary
            .constructors
            .iter()
            .any(|existing| same_member(existing, &candidate))
        {
            primary.constructors.push(candidate);
        }
    }
    for candidate in source.members {
        if !primary
            .members
            .iter()
            .any(|existing| same_member(existing, &candidate))
        {
            primary.members.push(candidate);
        }
    }
    for candidate in source.companion {
        if !primary
            .companion
            .iter()
            .any(|existing| same_member(existing, &candidate))
        {
            primary.companion.push(candidate);
        }
    }
    for (name, source) in source.declared_callables {
        let (primary_functions, primary_properties) = primary
            .declared_callables
            .remove(&name)
            .unwrap_or_default()
            .into_parts();
        let (source_functions, source_properties) = source.into_parts();
        primary.declared_callables.insert(
            name,
            Callables::from_parts(
                merge_functions(platform, primary_functions, source_functions),
                merge_properties(platform, primary_properties, source_properties),
            ),
        );
    }
    primary
}

impl SymbolSource for DependencyPlatform {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        self.source().package_exists(parent, name) || self.platform.package_exists(parent, name)
    }

    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
        if let Some(merged) = self
            .symbols_memo
            .borrow()
            .get(&namespace)
            .and_then(|symbols| symbols.get(name))
        {
            return merged.clone();
        }
        let primary = self.platform.symbols(namespace, name);
        let source_alias = namespace
            .existing_classifier(name)
            .and_then(|identity| self.source_alias_expansion(identity));
        let source = source_alias.as_ref().map_or_else(
            || self.source().symbols(namespace, name),
            |alias| {
                let classifier = self.platform.classifier(alias.target).map(|classifier| {
                    let mut classifier = (*classifier).clone();
                    classifier.alias_target = Some(alias.target);
                    std::sync::Arc::new(classifier)
                });
                Rc::new(ResolvedSymbols {
                    classifier_name: Some(alias.target),
                    classifier,
                    importable_declaration: true,
                    ..ResolvedSymbols::default()
                })
            },
        );
        let source_access = if source_alias.is_some() {
            SourceTypeAccess::Declared(Visibility::Public)
        } else {
            source
                .classifier_name
                .or(primary.classifier_name)
                .map_or(SourceTypeAccess::Absent, |name| {
                    self.source_type_access(name)
                })
        };
        let source_classifier = match source_access {
            SourceTypeAccess::Declared(_) | SourceTypeAccess::HiddenByOwner(_) => {
                source.classifier.as_ref()
            }
            SourceTypeAccess::Absent => None,
        };
        let mut classifier = match (&primary.classifier, source_classifier) {
            (Some(primary), Some(source)) => Some(std::sync::Arc::new(merge_type(
                self.platform.as_ref(),
                (**primary).clone(),
                (**source).clone(),
            ))),
            (Some(primary), None) => Some(primary.clone()),
            (None, Some(source)) => Some(source.clone()),
            (None, None) => None,
        };
        // Preserve the inaccessible declaration in the record. Core reports accessibility; deleting
        // it here would turn a precise visibility error into a false unresolved reference.
        if let (Some(shape), Some(visibility)) = (&classifier, source_access.source_visibility()) {
            let mut shape = (**shape).clone();
            shape.access = visibility.into();
            classifier = Some(std::sync::Arc::new(shape));
        }
        let (primary_functions, primary_properties) = primary.callables.clone().into_parts();
        let (source_functions, source_properties) = source.callables.clone().into_parts();
        let merged = Rc::new(ResolvedSymbols {
            classifier_name: primary.classifier_name.or(source.classifier_name),
            classifier,
            callables: Callables::from_parts(
                merge_functions(self.platform.as_ref(), primary_functions, source_functions),
                merge_properties(
                    self.platform.as_ref(),
                    primary_properties,
                    source_properties,
                ),
            ),
            importable_declaration: primary.importable_declaration || source.importable_declaration,
        });
        if !merged.is_empty() {
            self.symbols_memo
                .borrow_mut()
                .entry(namespace)
                .or_default()
                .insert(name.to_string(), merged.clone());
        }
        merged
    }
}

impl SemanticPlatform for DependencyPlatform {
    fn function_type(&self, arity: usize) -> Option<Ty> {
        self.platform.function_type(arity)
    }

    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        self.platform.value_underlying(ty).or_else(|| {
            ty.obj_internal().and_then(|internal| {
                self.classifier(internal)
                    .and_then(|shape| shape.value_underlying)
            })
        })
    }

    fn classifier_associated_property(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<crate::libraries::PropertyInfo> {
        match self.source_type_access(internal) {
            SourceTypeAccess::Absent => {
                self.platform.classifier_associated_property(internal, name)
            }
            SourceTypeAccess::Declared(_) | SourceTypeAccess::HiddenByOwner(_) => None,
        }
    }

    fn inherits_classifier_callables(&self, internal: TypeName) -> bool {
        self.platform.inherits_classifier_callables(internal)
    }

    fn top_level_associated_property(
        &self,
        package: TypeName,
        name: &str,
    ) -> Option<crate::libraries::PropertyInfo> {
        self.platform.top_level_associated_property(package, name)
    }

    fn external_property_diagnostic_label(
        &self,
        property: crate::fir::ExternalPropertyId,
        name: &str,
        ty: Ty,
    ) -> Option<String> {
        self.platform
            .external_property_diagnostic_label(property, name, ty)
    }

    fn library_value_form(&self, ty: Ty) -> Ty {
        self.platform.library_value_form(ty)
    }

    fn library_value_form_name(&self, internal: TypeName) -> TypeName {
        self.platform.library_value_form_name(internal)
    }

    fn canonical_source_type_name(&self, internal: TypeName) -> TypeName {
        self.platform.canonical_source_type_name(internal)
    }

    fn type_alias_expansion(&self, internal: TypeName) -> Option<crate::libraries::AliasExpansion> {
        self.source_alias_expansion(internal)
            .or_else(|| self.platform.type_alias_expansion(internal))
    }

    fn is_default_library_owner(&self, internal: TypeName) -> bool {
        self.platform.is_default_library_owner(internal)
    }

    fn is_erased_contract_callable(&self, callable: &crate::libraries::LibraryCallable) -> bool {
        self.platform.is_erased_contract_callable(callable)
    }

    fn boxed_primitive(&self, ty: Ty) -> Option<Ty> {
        self.platform.boxed_primitive(ty)
    }

    fn extension_receiver_rank(&self, recv: Ty, declared: Ty) -> Option<u32> {
        self.platform.extension_receiver_rank(recv, declared)
    }

    fn function_like_arity(&self, ty: Ty) -> Option<usize> {
        self.platform.function_like_arity(ty)
    }

    fn property_reference_type(&self, arity: usize, mutable: bool, args: &[Ty]) -> Option<Ty> {
        self.platform.property_reference_type(arity, mutable, args)
    }

    fn function_reference_type(&self, function: Ty) -> Option<Ty> {
        self.platform.function_reference_type(function)
    }

    fn class_literal_type(&self) -> Option<Ty> {
        self.platform.class_literal_type()
    }

    fn intrinsic_property(&self, receiver: Ty, name: &str) -> Option<LibraryMember> {
        self.platform.intrinsic_property(receiver, name)
    }

    fn implicit_common_supertypes(&self, types: &[Ty]) -> Vec<SemanticSupertype> {
        self.platform.implicit_common_supertypes(types)
    }

    fn platform_default_import_packages(&self) -> &'static [&'static str] {
        self.platform.platform_default_import_packages()
    }

    fn physical_property_getter_names(&self, property: &str) -> Vec<String> {
        self.platform.physical_property_getter_names(property)
    }

    fn inherited_accessor_properties(
        &self,
        source: &dyn crate::symbol_source::SymbolSource,
        receiver: Ty,
        property: &str,
    ) -> crate::libraries::PropertySet {
        self.platform
            .inherited_accessor_properties(source, receiver, property)
    }

    fn builtin_type_internal(&self, simple_name: &str) -> Option<String> {
        self.platform.builtin_type_internal(simple_name)
    }

    fn mapped_interface_members(
        &self,
        supertype: Ty,
    ) -> Vec<crate::libraries::MappedInterfaceMember> {
        self.platform.mapped_interface_members(supertype)
    }

    fn signature_formal_names(&self, signature: &str) -> Vec<String> {
        self.platform.signature_formal_names(signature)
    }

    fn iterable_element_type(&self, internal: &str) -> Option<Ty> {
        self.platform.iterable_element_type(internal)
    }

    fn iterable_element_type_name(&self, internal: TypeName) -> Option<Ty> {
        self.platform.iterable_element_type_name(internal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{Origin, TypeKind};
    use std::collections::HashMap;

    #[derive(Default)]
    struct TypeVisibility {
        public: HashMap<TypeName, bool>,
        inherits_classifier_callables: bool,
    }

    impl SymbolSource for TypeVisibility {
        fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
            let classifier_name = namespace.existing_classifier(name);
            let classifier = classifier_name
                .and_then(|name| self.public.get(&name))
                .map(|&is_public| std::sync::Arc::new(type_shape(is_public)));
            Rc::new(ResolvedSymbols {
                classifier_name: classifier.as_ref().and(classifier_name),
                classifier,
                ..ResolvedSymbols::default()
            })
        }
    }

    impl SemanticPlatform for TypeVisibility {
        fn inherits_classifier_callables(&self, _internal: TypeName) -> bool {
            self.inherits_classifier_callables
        }
    }

    fn type_shape(is_public: bool) -> LibraryType {
        LibraryType {
            is_kotlin: true,
            access: if is_public {
                crate::libraries::ClassifierAccess::Public
            } else {
                crate::libraries::ClassifierAccess::Private
            },
            source_file: None,
            stable_declaration: None,
            is_nested: false,
            outer_instance: None,
            kind: TypeKind::Class,
            inheritance: Default::default(),
            supertypes: Default::default(),
            supertype_templates: Vec::new(),
            constructors: Vec::new(),
            hidden_member_properties: Default::default(),
            declared_callables: HashMap::new(),
            declared_callable_order: Vec::new(),
            members: Vec::new(),
            companion: Vec::new(),
            constants: HashMap::new(),
            sam_eligible: false,
            callable_signature: None,
            callable_signatures: Vec::new(),
            companion_object: None,
            value_underlying: None,
            value_underlying_property: None,
            alias_target: None,
            type_parameters: crate::types::TypeParameters::default(),
            own_type_parameter_count: 0,
            sealed_subclasses: Default::default(),
            enum_entries: Vec::new(),
            enum_entries_accessor: None,
            named_parameter_lists: Vec::new(),
            retention: None,
            annotation_targets: None,
        }
    }

    fn property(
        owner: TypeName,
        visibility: Visibility,
        mutable: bool,
        origin: Origin,
    ) -> PropertyInfo {
        let mut getter = crate::libraries::LibraryCallable::library(
            owner,
            "getValue",
            Vec::new(),
            Ty::Int,
            Ty::Int,
            "()I",
        );
        getter.origin = origin.clone();
        let setter = mutable.then(|| {
            let mut setter = crate::libraries::LibraryCallable::library(
                owner,
                "setValue",
                vec![Ty::Int],
                Ty::Unit,
                Ty::Unit,
                "(I)V",
            );
            setter.origin = origin;
            setter
        });
        PropertyInfo {
            name: "value".to_string(),
            kind: PropKind::Member,
            receiver: Some(Ty::obj_name(owner)),
            formals: Vec::new(),
            ty: Ty::Int,
            context_count: 0,
            context_param_names: Vec::new(),
            getter,
            setter,
            setter_visibility: visibility,
            is_const: false,
            compile_time_constant: None,
            visibility,
            owner,
            receiver_rank: 0,
            source_key: None,
            stable_declaration: None,
            getter_declaration: None,
            setter_declaration: None,
            source_member: None,
            accessor_derived: false,
        }
    }

    #[test]
    fn symbols_reuses_the_merged_record_for_a_repeated_query() {
        let fqn = crate::types::type_name("demo/twice");
        let mut primary = TypeVisibility::default();
        primary.public.insert(fqn, true);
        let platform =
            DependencyPlatform::new(Box::new(primary), crate::resolve::SymbolTable::default());
        let namespace = SymbolNamespace::Package(crate::types::type_name("demo"));
        let first = platform.symbols(namespace, "twice");
        let second = platform.symbols(namespace, "twice");
        assert!(
            !first.is_empty(),
            "the test must exercise a positive memo entry"
        );
        assert!(
            Rc::ptr_eq(&first, &second),
            "repeated queries must not re-merge the namespace record"
        );
    }

    #[test]
    fn dependency_wrapper_preserves_foreign_classifier_inheritance_capability() {
        let classifier = crate::types::type_name("foreign/Derived");
        let platform = DependencyPlatform::new(
            Box::new(TypeVisibility {
                inherits_classifier_callables: true,
                ..TypeVisibility::default()
            }),
            crate::resolve::SymbolTable::default(),
        );

        assert!(platform.inherits_classifier_callables(classifier));
    }

    #[test]
    fn source_property_replaces_incomplete_primary_metadata() {
        let owner = crate::types::type_name("demo/Owner");
        let mut platform = TypeVisibility::default();
        platform.public.insert(owner, true);
        let source = property(
            owner,
            Visibility::Public,
            true,
            Origin::Module { facade: owner },
        );

        for primary in [
            property(owner, Visibility::Private, true, Origin::Library),
            property(owner, Visibility::Public, false, Origin::Library),
        ] {
            let merged = merge_properties(
                &platform,
                PropertySet {
                    overloads: vec![primary],
                },
                PropertySet {
                    overloads: vec![source.clone()],
                },
            );
            assert_eq!(merged.overloads.len(), 1);
            assert!(merged.overloads[0].setter.is_some());
            assert!(matches!(
                merged.overloads[0].getter.origin,
                Origin::Module { .. }
            ));
        }
    }

    #[test]
    fn public_primary_property_covers_source_metadata() {
        let owner = crate::types::type_name("demo/Owner");
        let mut platform = TypeVisibility::default();
        platform.public.insert(owner, true);
        let merged = merge_properties(
            &platform,
            PropertySet {
                overloads: vec![property(owner, Visibility::Public, true, Origin::Library)],
            },
            PropertySet {
                overloads: vec![property(
                    owner,
                    Visibility::Public,
                    true,
                    Origin::Module { facade: owner },
                )],
            },
        );

        assert_eq!(merged.overloads.len(), 1);
        assert!(matches!(merged.overloads[0].getter.origin, Origin::Library));
    }

    #[test]
    fn matching_source_function_restores_kotlin_declaration_capabilities() {
        let owner = crate::types::type_name("demo/Delegate");
        let mut member = LibraryMember::new(
            "getValue".to_string(),
            vec![Ty::nullable(Ty::obj("kotlin/Any")), Ty::obj("kotlin/Any")],
            Ty::Int,
            "(Ljava/lang/Object;Ljava/lang/Object;)I".to_string(),
        );
        member.owner = Some(owner);
        let primary =
            FunctionInfo::classifier_member(crate::libraries::FnKind::Member, owner, member);
        let mut source = primary.clone();
        source.flags.operator = true;
        source.flags.inline = crate::libraries::InlineKind::CanInline;
        source.callable.inline = crate::libraries::InlineKind::CanInline;

        let merged = merge_functions(
            &TypeVisibility::default(),
            FunctionSet {
                overloads: vec![primary],
            },
            FunctionSet {
                overloads: vec![source],
            },
        );

        assert_eq!(merged.overloads.len(), 1);
        assert!(merged.overloads[0].flags.operator);
        assert_eq!(
            merged.overloads[0].flags.inline,
            crate::libraries::InlineKind::CanInline,
        );
        assert_eq!(
            merged.overloads[0].callable.inline,
            crate::libraries::InlineKind::CanInline,
        );
        assert_eq!(
            merged.overloads[0].callable.descriptor,
            "(Ljava/lang/Object;Ljava/lang/Object;)I",
        );
    }

    #[test]
    fn property_on_non_public_primary_type_uses_source_metadata() {
        let owner = crate::types::type_name("demo/Owner");
        let mut platform = TypeVisibility::default();
        platform.public.insert(owner, false);
        let merged = merge_properties(
            &platform,
            PropertySet {
                overloads: vec![property(owner, Visibility::Public, false, Origin::Library)],
            },
            PropertySet {
                overloads: vec![property(
                    owner,
                    Visibility::Public,
                    false,
                    Origin::Module { facade: owner },
                )],
            },
        );

        assert_eq!(merged.overloads.len(), 1);
        assert!(matches!(
            merged.overloads[0].getter.origin,
            Origin::Module { .. }
        ));
    }

    #[test]
    fn dependency_projection_preserves_protected_members_for_core_access_checks() {
        let owner = crate::types::type_name("demo/Base");
        let mut classifier = type_shape(true);
        let mut member =
            LibraryMember::new("secret".to_string(), Vec::new(), Ty::Int, "()I".to_string());
        member.owner = Some(owner);
        member.visibility = Visibility::Protected;
        classifier.members.push(member.clone());

        let mut function = crate::libraries::FunctionInfo::classifier_member(
            crate::libraries::FnKind::Member,
            owner,
            member,
        );
        function.visibility = Visibility::Protected;
        let callables = Callables::Functions(FunctionSet {
            overloads: vec![function],
        });
        classifier
            .declared_callables
            .insert("secret".to_string(), callables);
        let merged = merge_type(&TypeVisibility::default(), type_shape(true), classifier);
        assert_eq!(merged.members.len(), 1);
        assert_eq!(merged.members[0].visibility, Visibility::Protected);
        assert_eq!(
            merged.declared_callables["secret"].functions()[0].visibility,
            Visibility::Protected
        );
    }

    #[test]
    fn source_enum_entries_are_the_authoritative_declaration_list() {
        let mut primary = type_shape(true);
        primary.kind = TypeKind::Enum;
        primary.enum_entries = vec!["STALE".to_string()];

        let mut source = type_shape(true);
        source.kind = TypeKind::Enum;
        source.enum_entries = vec!["FIRST".to_string(), "SECOND".to_string()];

        let merged = merge_type(&TypeVisibility::default(), primary, source);

        assert_eq!(merged.enum_entries, ["FIRST", "SECOND"]);
    }
}
