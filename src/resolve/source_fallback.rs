use crate::libraries::{
    CallSig, Callables, FunctionInfo, FunctionSet, LibraryMember, LibraryType, PropKind,
    PropertyInfo, PropertySet, ResolvedSymbols, SemanticPlatform, SemanticSupertype,
    StaticFieldRef,
};
use crate::module_symbols::ModuleSymbols;
use crate::name_tree::FxHashMap;
use crate::symbol_source::{InheritanceShape, SymbolSource};
use crate::types::{Ty, TypeName, Visibility};
use std::collections::HashMap;
use std::rc::Rc;

use super::SymbolTable;

pub(crate) struct SourceFallbackPlatform {
    platform: Box<dyn SemanticPlatform>,
    symbols: SymbolTable,
    /// Memoized merges over the immutable platform and source symbol tables.
    symbols_memo: std::cell::RefCell<FxHashMap<TypeName, Rc<ResolvedSymbols>>>,
    types_memo: std::cell::RefCell<FxHashMap<TypeName, Option<Rc<LibraryType>>>>,
    members_memo: std::cell::RefCell<FxHashMap<Ty, HashMap<String, FunctionSet>>>,
    props_memo: std::cell::RefCell<FxHashMap<Ty, HashMap<String, PropertySet>>>,
    supertypes_memo: std::cell::RefCell<FxHashMap<Ty, Vec<Ty>>>,
    shape_memo: std::cell::RefCell<FxHashMap<TypeName, Option<InheritanceShape>>>,
}

impl SourceFallbackPlatform {
    pub(crate) fn new(platform: Box<dyn SemanticPlatform>, symbols: SymbolTable) -> Self {
        SourceFallbackPlatform {
            platform,
            symbols,
            symbols_memo: Default::default(),
            types_memo: Default::default(),
            members_memo: Default::default(),
            props_memo: Default::default(),
            supertypes_memo: Default::default(),
            shape_memo: Default::default(),
        }
    }

    fn source(&self) -> ModuleSymbols<'_> {
        ModuleSymbols::new(&self.symbols)
    }

    fn public_source_type_name(&self, internal: TypeName) -> Option<Rc<LibraryType>> {
        self.source()
            .resolve_type_name(internal)
            .filter(|shape| shape.is_public)
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

fn complete_slots<T>(primary: &mut Vec<T>, fallback: Vec<T>, param_count: usize) {
    if primary.len() != param_count && fallback.len() == param_count {
        *primary = fallback;
    }
}

fn merge_call_shape(primary: &mut CallSig, fallback: CallSig, param_count: usize) {
    let primary_arity_known = primary.param_names.len() == param_count
        || primary.param_defaults.len() == param_count
        || primary.required > 0
        || primary.vararg_index.is_some();
    let CallSig {
        param_names,
        param_defaults,
        lambda_param_types,
        lambda_receivers,
        lambda_receiver_params,
        lambda_context_counts,
        lambda_materialized,
        platform_nullable_params,
        required,
        vararg,
        vararg_index,
    } = fallback;

    complete_slots(&mut primary.param_names, param_names, param_count);
    complete_slots(&mut primary.param_defaults, param_defaults, param_count);
    complete_slots(
        &mut primary.platform_nullable_params,
        platform_nullable_params,
        param_count,
    );

    if primary.lambda_param_types.len() != param_count {
        complete_slots(
            &mut primary.lambda_param_types,
            lambda_param_types,
            param_count,
        );
    } else if lambda_param_types.len() == param_count {
        for (current, fallback) in primary
            .lambda_param_types
            .iter_mut()
            .zip(lambda_param_types)
        {
            if current.is_empty() {
                *current = fallback;
            }
        }
    }

    if primary.lambda_receivers.len() != param_count {
        primary.lambda_receivers.resize(param_count, None);
    }
    if lambda_receivers.len() == param_count {
        for (current, fallback) in primary.lambda_receivers.iter_mut().zip(lambda_receivers) {
            if current.is_none() {
                *current = fallback;
            }
        }
    }

    if primary.lambda_receiver_params.len() != param_count {
        primary.lambda_receiver_params.resize(param_count, false);
    }
    if lambda_receiver_params.len() == param_count {
        for (current, fallback) in primary
            .lambda_receiver_params
            .iter_mut()
            .zip(lambda_receiver_params)
        {
            *current |= fallback;
        }
    }

    if primary.lambda_context_counts.len() != param_count {
        primary.lambda_context_counts.resize(param_count, 0);
    }
    if lambda_context_counts.len() == param_count {
        for (current, fallback) in primary
            .lambda_context_counts
            .iter_mut()
            .zip(lambda_context_counts)
        {
            if *current == 0 {
                *current = fallback;
            }
        }
    }

    if primary.lambda_materialized.len() != param_count {
        primary.lambda_materialized.resize(param_count, false);
    }
    if lambda_materialized.len() == param_count {
        for (current, fallback) in primary
            .lambda_materialized
            .iter_mut()
            .zip(lambda_materialized)
        {
            *current |= fallback;
        }
    }

    if !primary_arity_known {
        primary.required = required;
    }
    if primary.vararg_index.is_none() {
        primary.vararg_index = vararg_index;
    }
    primary.vararg |= vararg;
}

fn merge_function_shape(mut primary: FunctionInfo, fallback: FunctionInfo) -> FunctionInfo {
    let param_count = primary
        .callable
        .params
        .len()
        .saturating_sub(usize::from(primary.is_extension()));
    let valid_signature = primary.generic_sig.as_ref().is_some_and(|signature| {
        signature.params.len() == param_count
            && signature.ret != Ty::Error
            && (!primary.is_extension() || signature.receiver.is_some())
    });
    if !valid_signature {
        primary.generic_sig = Some(fallback.semantic_signature().into_owned());
    }
    merge_call_shape(&mut primary.call_sig, fallback.call_sig, param_count);
    if primary.context_count == 0 {
        primary.context_count = fallback.context_count;
    }
    primary
}

fn property_covers(
    platform: &dyn SemanticPlatform,
    primary: &PropertyInfo,
    fallback: &PropertyInfo,
) -> bool {
    if primary.kind != fallback.kind
        || primary.receiver != fallback.receiver
        || primary.visibility != Visibility::Public
        || (fallback.setter.is_some() && primary.setter.is_none())
    {
        return false;
    }
    primary.kind != PropKind::Member
        || (primary.owner == fallback.owner
            && platform
                .resolve_type_name(primary.owner)
                .is_some_and(|owner| owner.is_public))
}

fn same_property_declaration(left: &PropertyInfo, right: &PropertyInfo) -> bool {
    left.kind == right.kind && left.receiver == right.receiver && left.owner == right.owner
}

fn merge_functions(
    platform: &dyn SemanticPlatform,
    mut primary: FunctionSet,
    fallback: FunctionSet,
) -> FunctionSet {
    for candidate in fallback.overloads {
        if let Some(index) = primary
            .overloads
            .iter()
            .position(|existing| same_function(platform, existing, &candidate))
        {
            let existing = primary.overloads[index].clone();
            primary.overloads[index] = merge_function_shape(existing, candidate);
        } else {
            primary.overloads.push(candidate);
        }
    }
    primary
}

fn public_functions(mut functions: FunctionSet) -> FunctionSet {
    functions
        .overloads
        .retain(|function| function.visibility == Visibility::Public);
    functions
}

fn public_properties(mut properties: PropertySet) -> PropertySet {
    properties
        .overloads
        .retain(|property| property.visibility == Visibility::Public);
    properties
}

fn merge_properties(
    platform: &dyn SemanticPlatform,
    mut primary: PropertySet,
    fallback: PropertySet,
) -> PropertySet {
    for candidate in fallback.overloads {
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

fn split_callables(callables: Callables) -> (FunctionSet, PropertySet) {
    match callables {
        Callables::None => (FunctionSet::default(), PropertySet::default()),
        Callables::Functions(functions) => (functions, PropertySet::default()),
        Callables::Properties(properties) => (FunctionSet::default(), properties),
        Callables::Both {
            functions,
            properties,
        } => (functions, properties),
    }
}

fn join_callables(functions: FunctionSet, properties: PropertySet) -> Callables {
    match (
        functions.overloads.is_empty(),
        properties.overloads.is_empty(),
    ) {
        (true, true) => Callables::None,
        (false, true) => Callables::Functions(functions),
        (true, false) => Callables::Properties(properties),
        (false, false) => Callables::Both {
            functions,
            properties,
        },
    }
}

fn merge_type(mut primary: LibraryType, fallback: LibraryType) -> LibraryType {
    primary.is_public |= fallback.is_public;
    for candidate in fallback.constructors {
        if !primary
            .constructors
            .iter()
            .any(|existing| same_member(existing, &candidate))
        {
            primary.constructors.push(candidate);
        }
    }
    for candidate in fallback.members {
        if !primary
            .members
            .iter()
            .any(|existing| same_member(existing, &candidate))
        {
            primary.members.push(candidate);
        }
    }
    for candidate in fallback.companion {
        if !primary
            .companion
            .iter()
            .any(|existing| same_member(existing, &candidate))
        {
            primary.companion.push(candidate);
        }
    }
    primary
}

impl SymbolSource for SourceFallbackPlatform {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        if let Some(hit) = self.supertypes_memo.borrow().get(&ty) {
            return hit.clone();
        }
        let supertypes = if ty
            .obj_internal()
            .is_some_and(|internal| self.public_source_type_name(internal).is_some())
        {
            self.source().direct_supertypes(ty)
        } else {
            self.platform.direct_supertypes(ty)
        };
        self.supertypes_memo
            .borrow_mut()
            .insert(ty, supertypes.clone());
        supertypes
    }

    fn member_overloads(&self, recv: Ty, name: &str) -> FunctionSet {
        if let Some(hit) = self
            .members_memo
            .borrow()
            .get(&recv)
            .and_then(|by_name| by_name.get(name))
        {
            return hit.clone();
        }
        let merged = merge_functions(
            self.platform.as_ref(),
            self.platform.member_overloads(recv, name),
            public_functions(self.source().member_overloads(recv, name)),
        );
        self.members_memo
            .borrow_mut()
            .entry(recv)
            .or_default()
            .insert(name.to_string(), merged.clone());
        merged
    }

    fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
        let source = self.source();
        let visibility = source.classifier_visibility(crate::types::type_name(internal));
        if visibility.is_some_and(|visibility| visibility != Visibility::Public) {
            return None;
        }
        let fallback = source
            .resolve_type(internal)
            .filter(|shape| shape.is_public);
        match (self.platform.resolve_type(internal), fallback) {
            (Some(primary), Some(fallback)) => Some(merge_type(primary, fallback)),
            (Some(primary), None) => Some(primary),
            (None, Some(fallback)) => Some(fallback),
            (None, None) => None,
        }
    }

    fn resolve_type_name(&self, internal: TypeName) -> Option<Rc<LibraryType>> {
        if let Some(merged) = self.types_memo.borrow().get(&internal) {
            return merged.clone();
        }
        let source = self.source();
        let visibility = source.classifier_visibility(internal);
        if visibility.is_some_and(|visibility| visibility != Visibility::Public) {
            self.types_memo.borrow_mut().insert(internal, None);
            return None;
        }
        let fallback = self.public_source_type_name(internal);
        let merged = match (self.platform.resolve_type_name(internal), fallback) {
            (Some(primary), Some(fallback)) => {
                Some(Rc::new(merge_type((*primary).clone(), (*fallback).clone())))
            }
            (Some(primary), None) => Some(primary),
            (None, Some(fallback)) => Some(fallback),
            (None, None) => None,
        };
        self.types_memo
            .borrow_mut()
            .insert(internal, merged.clone());
        merged
    }

    fn classifier_visibility(&self, internal: TypeName) -> Option<Visibility> {
        self.source()
            .classifier_visibility(internal)
            .or_else(|| self.platform.classifier_visibility(internal))
    }

    fn classifier_access(
        &self,
        internal: TypeName,
    ) -> Option<crate::symbol_source::ClassifierAccess> {
        if self.source().classifier_visibility(internal).is_some() {
            self.source().classifier_access(internal)
        } else {
            self.platform.classifier_access(internal)
        }
    }

    fn classifier_accessible_from_package(
        &self,
        internal: TypeName,
        accessor_package: TypeName,
    ) -> bool {
        if let Some(visibility) = self.source().classifier_visibility(internal) {
            return visibility == Visibility::Public;
        }
        self.platform
            .classifier_accessible_from_package(internal, accessor_package)
    }

    fn inherited_classifier_shape(
        &self,
        internal: TypeName,
        inheritor: TypeName,
    ) -> Option<Rc<LibraryType>> {
        if let Some(visibility) = self.source().classifier_visibility(internal) {
            return match visibility {
                Visibility::Public | Visibility::Protected => {
                    self.source().resolve_type_name(internal)
                }
                Visibility::Internal | Visibility::Private => None,
            };
        }
        self.platform
            .inherited_classifier_shape(internal, inheritor)
    }

    fn resolve_symbols(&self, fqn: &str) -> ResolvedSymbols {
        (*self.resolve_symbols_name(crate::types::type_name(fqn))).clone()
    }

    fn resolve_symbols_name(&self, fqn: TypeName) -> Rc<ResolvedSymbols> {
        if let Some(merged) = self.symbols_memo.borrow().get(&fqn) {
            return merged.clone();
        }
        let primary = self.platform.resolve_symbols_name(fqn);
        let fallback = self.source().resolve_symbols_name(fqn);
        let fallback_classifier = fallback
            .classifier
            .as_ref()
            .filter(|classifier| classifier.is_public);
        let classifier = match (&primary.classifier, fallback_classifier) {
            (Some(primary), Some(fallback)) => Some(Rc::new(merge_type(
                (**primary).clone(),
                (**fallback).clone(),
            ))),
            (Some(primary), None) => Some(primary.clone()),
            (None, Some(fallback)) => Some(fallback.clone()),
            (None, None) => None,
        };
        let (primary_functions, primary_properties) = split_callables(primary.callables.clone());
        let (fallback_functions, fallback_properties) = split_callables(fallback.callables.clone());
        let merged = Rc::new(ResolvedSymbols {
            classifier,
            callables: join_callables(
                merge_functions(
                    self.platform.as_ref(),
                    primary_functions,
                    public_functions(fallback_functions),
                ),
                merge_properties(
                    self.platform.as_ref(),
                    primary_properties,
                    public_properties(fallback_properties),
                ),
            ),
        });
        self.symbols_memo.borrow_mut().insert(fqn, merged.clone());
        merged
    }

    fn property_members(&self, recv: Ty, name: &str) -> PropertySet {
        if let Some(hit) = self
            .props_memo
            .borrow()
            .get(&recv)
            .and_then(|by_name| by_name.get(name))
        {
            return hit.clone();
        }
        let mut platform_properties = self.platform.property_members(recv, name);
        // A Kotlin override of a Java-supertype getter keeps the Java synthetic property but
        // REFINES its type: `RefinedCatalog.getEntries(): Array<RefinedEntry>` overrides
        // `JavaCatalog.getEntries(): BaseEntry[]`. The platform walk resolves the JAVA
        // declaration (the source override is invisible to it), so rewrite each member property
        // with the most-derived SOURCE override's return type. Only an EXISTING platform property
        // is refined — a pure-Kotlin `getX()` still creates no synthetic property (kotlinc
        // parity). The lookup is structural and does not recognize any concrete API name.
        if platform_properties
            .overloads
            .iter()
            .any(|property| property.kind == PropKind::Member)
        {
            if let Some(ret) = self
                .physical_property_getter_names(name)
                .iter()
                .flat_map(|getter| self.source().instance_members(recv, getter))
                .find(|member| member.params.is_empty() && member.ret != Ty::Unit)
                .map(|member| member.ret)
            {
                for property in &mut platform_properties.overloads {
                    if property.kind == PropKind::Member && property.setter.is_none() {
                        property.ty = ret;
                    }
                }
            }
        }
        let merged = merge_properties(
            self.platform.as_ref(),
            platform_properties,
            public_properties(self.source().property_members(recv, name)),
        );
        self.props_memo
            .borrow_mut()
            .entry(recv)
            .or_default()
            .insert(name.to_string(), merged.clone());
        merged
    }

    fn member_is_property(&self, recv: Ty, name: &str) -> bool {
        self.platform.member_is_property(recv, name)
            || !public_properties(self.source().property_members(recv, name))
                .overloads
                .is_empty()
    }

    fn inheritance_shape_name(&self, internal: TypeName) -> Option<InheritanceShape> {
        if let Some(hit) = self.shape_memo.borrow().get(&internal) {
            return *hit;
        }
        let shape = self.platform.inheritance_shape_name(internal).or_else(|| {
            self.public_source_type_name(internal)
                .and_then(|_| self.source().inheritance_shape_name(internal))
        });
        self.shape_memo.borrow_mut().insert(internal, shape);
        shape
    }
}

impl SemanticPlatform for SourceFallbackPlatform {
    fn function_type(&self, arity: usize) -> Option<Ty> {
        self.platform.function_type(arity)
    }

    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        self.platform.value_underlying(ty).or_else(|| {
            ty.obj_internal().and_then(|internal| {
                self.resolve_type_name(internal)
                    .and_then(|shape| shape.value_underlying)
            })
        })
    }

    fn value_underlying_name(&self, internal: TypeName) -> Option<Ty> {
        self.resolve_type_name(internal)
            .and_then(|shape| shape.value_underlying)
    }

    fn static_field(&self, internal: &str, name: &str) -> Option<StaticFieldRef> {
        self.platform.static_field(internal, name)
    }

    fn static_field_name(&self, internal: TypeName, name: &str) -> Option<StaticFieldRef> {
        self.platform.static_field_name(internal, name)
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

    fn is_default_library_owner(&self, internal: TypeName) -> bool {
        self.platform.is_default_library_owner(internal)
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

    fn supports_member_reference(&self, member: &LibraryMember) -> bool {
        self.platform.supports_member_reference(member)
    }

    fn property_reference_type(&self, arity: usize, mutable: bool) -> Option<Ty> {
        self.platform.property_reference_type(arity, mutable)
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

    fn physical_property_getter_name(&self, property: &str) -> Option<String> {
        self.platform.physical_property_getter_name(property)
    }

    fn physical_property_getter_names(&self, property: &str) -> Vec<String> {
        self.platform.physical_property_getter_names(property)
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
    }

    impl SymbolSource for TypeVisibility {
        fn resolve_type_name(&self, internal: TypeName) -> Option<Rc<LibraryType>> {
            self.public
                .get(&internal)
                .copied()
                .map(|is_public| Rc::new(type_shape(is_public)))
        }
    }

    impl SemanticPlatform for TypeVisibility {}

    fn type_shape(is_public: bool) -> LibraryType {
        LibraryType {
            is_public,
            kind: TypeKind::Class,
            supertypes: Default::default(),
            constructors: Vec::new(),
            members: Vec::new(),
            companion: Vec::new(),
            companion_consts: HashMap::new(),
            sam_method: None,
            companion_object: None,
            value_companion_fns: Vec::new(),
            value_underlying: None,
            alias_target: None,
            type_params: Vec::new(),
            sealed_subclasses: Default::default(),
            enum_entries: Vec::new(),
            value_ctor_has_default: false,
            ctor_named_params: Vec::new(),
            value_class_properties: Vec::new(),
            retention: None,
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
            kind: PropKind::Member,
            receiver: Some(Ty::obj_name(owner)),
            formals: Vec::new(),
            ty: Ty::Int,
            context_count: 0,
            getter,
            setter,
            is_const: false,
            visibility,
            owner,
            receiver_rank: 0,
            source_key: None,
        }
    }

    #[test]
    fn resolve_symbols_name_reuses_the_merged_record_for_a_repeated_query() {
        let platform = SourceFallbackPlatform::new(
            Box::new(TypeVisibility::default()),
            crate::resolve::SymbolTable::default(),
        );
        let fqn = crate::types::type_name("demo/twice");
        let first = platform.resolve_symbols_name(fqn);
        let second = platform.resolve_symbols_name(fqn);
        assert!(
            Rc::ptr_eq(&first, &second),
            "repeated queries must not re-merge the namespace record"
        );
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
}
