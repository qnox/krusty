use crate::libraries::{
    Callables, FunctionInfo, FunctionSet, LibraryMember, LibraryType, PropertyInfo, PropertySet,
    ResolvedSymbols, SemanticPlatform, SemanticSupertype, StaticFieldRef,
};
use crate::module_symbols::ModuleSymbols;
use crate::symbol_source::{InheritanceShape, SymbolSource};
use crate::types::{Ty, TypeName};
use std::rc::Rc;

use super::SymbolTable;

pub(crate) struct SourceFallbackPlatform {
    platform: Box<dyn SemanticPlatform>,
    symbols: SymbolTable,
}

impl SourceFallbackPlatform {
    pub(crate) fn new(platform: Box<dyn SemanticPlatform>, symbols: SymbolTable) -> Self {
        SourceFallbackPlatform { platform, symbols }
    }

    fn source(&self) -> ModuleSymbols<'_> {
        ModuleSymbols::new(&self.symbols)
    }
}

fn same_member(left: &LibraryMember, right: &LibraryMember) -> bool {
    left.name == right.name && left.params == right.params
}

fn same_function(left: &FunctionInfo, right: &FunctionInfo) -> bool {
    left.kind == right.kind
        && left.receiver == right.receiver
        && left.callable.name == right.callable.name
        && left.callable.params == right.callable.params
}

fn same_property(left: &PropertyInfo, right: &PropertyInfo) -> bool {
    left.kind == right.kind && left.receiver == right.receiver
}

fn merge_functions(mut primary: FunctionSet, fallback: FunctionSet) -> FunctionSet {
    for candidate in fallback.overloads {
        if !primary
            .overloads
            .iter()
            .any(|existing| same_function(existing, &candidate))
        {
            primary.overloads.push(candidate);
        }
    }
    primary
}

fn merge_properties(mut primary: PropertySet, fallback: PropertySet) -> PropertySet {
    for candidate in fallback.overloads {
        if !primary
            .overloads
            .iter()
            .any(|existing| same_property(existing, &candidate))
        {
            primary.overloads.push(candidate);
        }
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
    fn member_overloads(&self, recv: Ty, name: &str) -> FunctionSet {
        merge_functions(
            self.platform.member_overloads(recv, name),
            self.source().member_overloads(recv, name),
        )
    }

    fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
        match (
            self.platform.resolve_type(internal),
            self.source().resolve_type(internal),
        ) {
            (Some(primary), Some(fallback)) => Some(merge_type(primary, fallback)),
            (Some(primary), None) => Some(primary),
            (None, Some(fallback)) => Some(fallback),
            (None, None) => None,
        }
    }

    fn resolve_type_name(&self, internal: TypeName) -> Option<Rc<LibraryType>> {
        match (
            self.platform.resolve_type_name(internal),
            self.source().resolve_type_name(internal),
        ) {
            (Some(primary), Some(fallback)) => {
                Some(Rc::new(merge_type((*primary).clone(), (*fallback).clone())))
            }
            (Some(primary), None) => Some(primary),
            (None, Some(fallback)) => Some(fallback),
            (None, None) => None,
        }
    }

    fn resolve_symbols(&self, fqn: &str) -> ResolvedSymbols {
        (*self.resolve_symbols_name(crate::types::type_name(fqn))).clone()
    }

    fn resolve_symbols_name(&self, fqn: TypeName) -> Rc<ResolvedSymbols> {
        let primary = self.platform.resolve_symbols_name(fqn);
        let fallback = self.source().resolve_symbols_name(fqn);
        let classifier = match (&primary.classifier, &fallback.classifier) {
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
        Rc::new(ResolvedSymbols {
            classifier,
            callables: join_callables(
                merge_functions(primary_functions, fallback_functions),
                merge_properties(primary_properties, fallback_properties),
            ),
        })
    }

    fn property_members(&self, recv: Ty, name: &str) -> PropertySet {
        merge_properties(
            self.platform.property_members(recv, name),
            self.source().property_members(recv, name),
        )
    }

    fn member_is_property(&self, recv: Ty, name: &str) -> bool {
        self.platform.member_is_property(recv, name) || self.source().member_is_property(recv, name)
    }

    fn inheritance_shape_name(&self, internal: TypeName) -> Option<InheritanceShape> {
        self.platform
            .inheritance_shape_name(internal)
            .or_else(|| self.source().inheritance_shape_name(internal))
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

    fn builtin_type_internal(&self, simple_name: &str) -> Option<String> {
        self.platform.builtin_type_internal(simple_name)
    }

    fn is_collection_interface(&self, internal: &str) -> bool {
        self.platform.is_collection_interface(internal)
    }

    fn is_collection_interface_name(&self, internal: TypeName) -> bool {
        self.platform.is_collection_interface_name(internal)
    }

    fn collection_property_accessor(&self, property: &str) -> Option<String> {
        self.platform.collection_property_accessor(property)
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
