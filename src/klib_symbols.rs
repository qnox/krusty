//! Declarations from a KLIB, as a [`SymbolSource`].
//!
//! The library half of klib ingestion: `crate::klib` opens the container, `crate::metadata::reader`
//! decodes its `linkdata` and says what the declared names mean as types, and this turns those
//! declarations into the records a resolver selects against. Nothing in the path is a backend's, so
//! a target whose libraries are klibs — Native, JS, wasm — takes its signatures from the library its
//! own distribution ships instead of inferring them from the JVM stdlib jar.
//!
//! What it does not do is decide representation. The callables it reports carry no descriptor and no
//! erasure: a backend adds those to the record it receives, the same division the reader keeps.
//!
//! Classes, functions and properties are reported. Package-level type aliases are not: the reader
//! does not decode a `TypeAlias` declaration at all. That is a missing answer rather than a wrong
//! one — a name that is not reported simply does not resolve through this source.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use crate::klib::KlibArchive;
use crate::libraries::{
    AliasExpansion, CallSig, Callables, FnKind, FunctionInfo, FunctionSet, GenericSig,
    LibraryCallable, LibraryMember, LibraryType, PlatformSourceHeaderInput, PropKind, PropertyInfo,
    PropertySet, ResolvedSymbols, SemanticPlatform, SemanticSupertype, SourceHeaderError, TypeKind,
};
use crate::metadata::reader::{
    builtin_bounds, builtin_ty, library_type::library_type, parse_package_fragment, BuiltinMember,
    BuiltinTypeParam,
};
use crate::symbol_source::{CompositeSource, SymbolNamespace, SymbolSource};
use crate::types::{type_name, Ty, TypeName};

/// Declarations read out of one or more klibs.
#[derive(Default)]
pub struct KlibSymbols {
    /// Classifier records, each already carrying its own member surface: a member call resolves
    /// through `LibraryType::declared_callables`, not through a classifier-namespace probe.
    classifiers: HashMap<TypeName, Arc<LibraryType>>,
    /// Every package any klib declares into, and every prefix of one: a qualifier walk resolves one
    /// segment at a time, so an intermediate package that declares nothing itself must still exist.
    packages: HashSet<(TypeName, String)>,
    /// Top-level functions and extensions, keyed by their package and source name.
    top_level: HashMap<(TypeName, String), FunctionSet>,
    /// Top-level properties and extension properties, keyed the same way.
    top_level_properties: HashMap<(TypeName, String), PropertySet>,
    /// Top-level type aliases, by their own identity. A use site resolves the alias's SPELLING to
    /// this identity and then asks for the template, so the identity is the key and not the target.
    aliases: HashMap<TypeName, AliasExpansion>,
}

/// Whether a declaration was read from a classifier's member list or a package's top-level list.
/// A receiver means an extension either way, but the two spell it with different property kinds.
#[derive(Clone, Copy)]
enum DeclarationScope {
    Member,
    TopLevel,
}

impl KlibSymbols {
    /// Read every klib at `paths`. A path that is not a klib is skipped: a library that cannot be
    /// opened contributes no declarations, which is what an absent library does too.
    pub fn open(paths: &[impl AsRef<Path>]) -> Self {
        let mut symbols = Self::default();
        for path in paths {
            if let Some(archive) = KlibArchive::open(path.as_ref()) {
                symbols.read(&archive);
            }
        }
        symbols
    }

    fn read(&mut self, archive: &KlibArchive) {
        for fragment in archive.package_fragments() {
            let Some(bytes) = archive.read(&fragment.entry) else {
                continue;
            };
            self.declare_package(&fragment.package_fqname);
            let package = parse_package_fragment(&bytes);
            for (internal, declaration) in package.classes {
                let owner = type_name(&internal);
                let class_bounds = builtin_bounds(&declaration.type_params, &HashMap::new());
                let mut functions: HashMap<String, FunctionSet> = HashMap::new();
                let mut properties: HashMap<String, PropertySet> = HashMap::new();
                let mut order: Vec<String> = Vec::new();
                let mut members: Vec<LibraryMember> = Vec::new();
                for member in &declaration.members {
                    let mut note = |name: &String| {
                        if !order.contains(name) {
                            order.push(name.clone());
                        }
                    };
                    if member.is_property {
                        note(&member.name);
                        properties
                            .entry(member.name.clone())
                            .or_default()
                            .overloads
                            .push(property_record(
                                owner,
                                member,
                                &class_bounds,
                                DeclarationScope::Member,
                            ));
                        continue;
                    }
                    let record = member_record(owner, declaration.kind, member, &class_bounds);
                    note(&member.name);
                    functions
                        .entry(member.name.clone())
                        .or_default()
                        .overloads
                        .push(FunctionInfo::classifier_member(
                            FnKind::Member,
                            owner,
                            record.clone(),
                        ));
                    members.push(record);
                }
                let mut classifier = library_type(owner, declaration);
                classifier.declared_callables = order
                    .iter()
                    .map(|name| {
                        (
                            name.clone(),
                            Callables::from_parts(
                                functions.remove(name).unwrap_or_default(),
                                properties.remove(name).unwrap_or_default(),
                            ),
                        )
                    })
                    .collect();
                classifier.declared_callable_order = order;
                classifier.members = members;
                self.classifiers
                    .entry(owner)
                    .or_insert_with(|| Arc::new(classifier));
            }
            let package_name = package_identity(&fragment.package_fqname);
            for function in package.functions {
                let bounds = builtin_bounds(&function.formals, &HashMap::new());
                let member = BuiltinMember {
                    name: function.name.clone(),
                    params: function.params.clone(),
                    ret: function.ret.clone(),
                    is_property: false,
                    is_operator: function.is_operator,
                    is_infix: function.is_infix,
                    is_abstract: false,
                    formals: function.formals.clone(),
                    ret_nullable: function.ret.nullable(),
                    param_names: function.param_names.clone(),
                    param_defaults: function.param_defaults.clone(),
                    receiver: function.receiver.clone(),
                    is_var: false,
                    is_const: false,
                    visibility: function.visibility,
                    annotations: function.annotations.clone(),
                    setter_visibility: None,
                };
                let record = member_record(package_name, TypeKind::Class, &member, &bounds);
                let receiver = record.generic_sig.as_ref().and_then(|sig| sig.receiver);
                let kind = match receiver {
                    Some(_) => FnKind::Extension,
                    None => FnKind::TopLevel,
                };
                let mut overload = FunctionInfo::classifier_member(kind, package_name, record);
                overload.receiver = receiver;
                self.top_level
                    .entry((package_name, function.name))
                    .or_default()
                    .overloads
                    .push(overload);
            }
            for alias in &package.type_aliases {
                let identity = crate::types::type_name_child(package_name, &alias.name);
                let bounds = builtin_bounds(&alias.type_params, &HashMap::new());
                let expansion = builtin_ty(&alias.expanded, &bounds);
                let Some(target) = expansion.non_null().kotlin_class_internal() else {
                    // An alias whose right-hand side is not a classifier (a function type, a bare
                    // type parameter) has no target for a use site to check a spelling against.
                    continue;
                };
                self.aliases.insert(
                    identity,
                    AliasExpansion {
                        identity,
                        target,
                        formals: alias
                            .type_params
                            .iter()
                            .map(|parameter| parameter.name.clone())
                            .collect(),
                        expansion,
                        // A klib records an abbreviated right-hand side in `Type.abbreviatedTypeId`,
                        // which the reader does not read yet; the default means "no alias was
                        // spelled here", so a chained alias expands unabbreviated.
                        expansion_spelling: crate::spelling::Spelled::default(),
                    },
                );
            }
            for property in &package.properties {
                self.top_level_properties
                    .entry((package_name, property.name.clone()))
                    .or_default()
                    .overloads
                    .push(property_record(
                        package_name,
                        property,
                        &HashMap::new(),
                        DeclarationScope::TopLevel,
                    ));
            }
        }
    }

    /// Record a package and every prefix of it.
    fn declare_package(&mut self, fqname: &str) {
        let mut parent = TypeName::ROOT;
        if fqname.is_empty() {
            return;
        }
        for segment in fqname.split('.') {
            self.packages.insert((parent, segment.to_string()));
            parent = crate::types::type_name_child(parent, segment);
        }
    }

    /// The template a top-level `typealias` this source declares expands to.
    pub fn type_alias_expansion(&self, internal: TypeName) -> Option<AliasExpansion> {
        self.aliases.get(&internal).cloned()
    }
}

/// The package a fragment's declarations belong to, as a classifier-namespace identity.
fn package_identity(fqname: &str) -> TypeName {
    if fqname.is_empty() {
        return TypeName::ROOT;
    }
    type_name(&fqname.replace('.', "/"))
}

/// The declaration record one decoded function denotes.
///
/// A member that declares its own type parameters composes them over its owner's before its
/// signature is read, so `fun <R> map(transform: (T) -> R): R` resolves `R` to the method's own
/// parameter and `T` to the class's. The record carries no descriptor: a klib records none, and
/// inventing one would be this layer deciding a target's representation.
///
/// A member EXTENSION (`class Holder { fun Int.f() }`) keeps its declaring class as the dispatch
/// receiver and carries the extension receiver at the head of its realized parameter list, marked
/// by the member-extension flag — the shape the module's own declarations already publish.
fn member_record(
    owner: TypeName,
    owner_kind: TypeKind,
    member: &BuiltinMember,
    owner_bounds: &HashMap<String, Ty>,
) -> LibraryMember {
    let bounds = builtin_bounds(&member.formals, owner_bounds);
    let receiver = member
        .receiver
        .as_ref()
        .map(|receiver| builtin_ty(receiver, &bounds));
    let mut params: Vec<Ty> = member
        .params
        .iter()
        .map(|parameter| builtin_ty(parameter, &bounds))
        .collect();
    let value_params = params.clone();
    if let Some(receiver) = receiver {
        params.insert(0, receiver);
    }
    let ret = builtin_ty(&member.ret, &bounds);
    let mut record = LibraryMember::new(member.name.clone(), params, ret, String::new());
    record.owner = Some(owner);
    record.set_is_member_extension(receiver.is_some());
    record.set_ret_nullable(member.ret_nullable);
    record.set_is_operator(member.is_operator);
    record.set_is_infix(member.is_infix);
    record.set_is_abstract(member.is_abstract);
    record.visibility = member.visibility;
    record.annotations = member
        .annotations
        .iter()
        .map(|annotation| type_name(annotation))
        .collect();
    // Whether the owner is an interface is a declaration fact the decoded kind already carries; how
    // a call to it dispatches is the backend's reading of that fact.
    record.set_is_interface(matches!(
        owner_kind,
        TypeKind::Interface | TypeKind::Annotation
    ));
    // The parameter names and defaults a named argument needs, which a descriptor erases. A
    // `CallSig` is parallel to the LOGICAL parameter list, which never includes the receiver.
    record.call_sig = CallSig::metadata_member(
        value_params.len(),
        member.param_names.clone(),
        member.param_defaults.clone(),
        None,
    );
    record.generic_sig = Some(GenericSig {
        formals: member
            .formals
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        formal_bounds: formal_bounds(&member.formals, &bounds),
        receiver,
        params: value_params,
        ret,
        return_policy: Default::default(),
    });
    record
}

/// The property record one decoded property denotes.
///
/// A klib names no accessor: whether a read is a field load, a getter call or something else is the
/// realizing target's decision, so the accessors here are semantic handles carrying the property's
/// own name and no descriptor. A `var` gets a setter because the flag word says it has one; a `val`
/// does not, and a write against it must be rejected.
fn property_record(
    owner: TypeName,
    member: &BuiltinMember,
    owner_bounds: &HashMap<String, Ty>,
    scope: DeclarationScope,
) -> PropertyInfo {
    let bounds = builtin_bounds(&member.formals, owner_bounds);
    let ty = builtin_ty(&member.ret, &bounds);
    let receiver = member
        .receiver
        .as_ref()
        .map(|receiver| builtin_ty(receiver, &bounds));
    let accessor = |params: Vec<Ty>, ret: Ty| {
        LibraryCallable::library(owner, member.name.clone(), params, ret, ret, String::new())
    };
    let kind = match (receiver, scope) {
        (Some(_), DeclarationScope::Member) => PropKind::MemberExtension,
        (Some(_), DeclarationScope::TopLevel) => PropKind::Extension,
        (None, DeclarationScope::Member) => PropKind::Member,
        (None, DeclarationScope::TopLevel) => PropKind::TopLevel,
    };
    let getter_params = receiver.map(|receiver| vec![receiver]).unwrap_or_default();
    let setter_params = receiver
        .map(|receiver| vec![receiver, ty])
        .unwrap_or_else(|| vec![ty]);
    PropertyInfo {
        receiver,
        formals: member
            .formals
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        setter: member.is_var.then(|| accessor(setter_params, Ty::Unit)),
        is_const: member.is_const,
        visibility: member.visibility,
        // A setter declared less visible than its property (`private set`) states its own
        // visibility; one that is not declared separately is as visible as the property.
        setter_visibility: member.setter_visibility.unwrap_or(member.visibility),
        ..PropertyInfo::declared(
            member.name.clone(),
            kind,
            owner,
            ty,
            accessor(getter_params, ty),
        )
    }
}

/// Declared upper bounds, parallel to the formals — read with the formals themselves already in
/// scope, so an F-bounded parameter (`<T : Comparable<T>>`) resolves to its own type parameter.
fn formal_bounds(formals: &[BuiltinTypeParam], bounds: &HashMap<String, Ty>) -> Vec<Vec<Ty>> {
    formals
        .iter()
        .map(|parameter| {
            parameter
                .bounds
                .iter()
                .map(|bound| builtin_ty(bound, bounds))
                .collect()
        })
        .collect()
}

impl SymbolSource for KlibSymbols {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        self.packages.contains(&(parent, name.to_string()))
    }

    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
        // Member callables are NOT answered here: a member call reads them from the classifier
        // record, the channel every other provider uses. A classifier namespace therefore answers
        // with its nested classifiers only.
        let (functions, properties) = match namespace {
            SymbolNamespace::Classifier(_) => Default::default(),
            SymbolNamespace::Package(package) => {
                let key = (package, name.to_string());
                (
                    self.top_level.get(&key).cloned().unwrap_or_default(),
                    self.top_level_properties
                        .get(&key)
                        .cloned()
                        .unwrap_or_default(),
                )
            }
        };
        let identity = namespace.existing_classifier(name);
        // A `typealias` resolves to its TARGET's record, tagged with the target it came through, and
        // is importable in its own right — the shape a source alias already federates as. The
        // target's record may be absent when it is not this source's to declare; the identity is
        // still the answer, and the federation resolves the record.
        if let Some(alias) = identity.and_then(|identity| self.aliases.get(&identity)) {
            let classifier = self.classifiers.get(&alias.target).map(|target| {
                let mut record = (**target).clone();
                record.alias_target = Some(alias.target);
                Arc::new(record)
            });
            return Rc::new(ResolvedSymbols {
                classifier_name: Some(alias.target),
                classifier,
                callables: Callables::from_parts(functions, properties),
                importable_declaration: true,
            });
        }
        let classifier = identity.and_then(|identity| self.classifiers.get(&identity).cloned());
        let classifier_name = classifier.as_ref().and(identity);
        Rc::new(ResolvedSymbols {
            classifier_name,
            classifier,
            callables: Callables::from_parts(functions, properties),
            importable_declaration: false,
        })
    }
}

/// A platform whose dependency path includes klibs.
///
/// The klibs are additional libraries, not a replacement platform: representation, builtins and
/// every other platform semantic stay the wrapped platform's, and only the declaration lookup is
/// federated. Precedence follows [`CompositeSource`] — the platform's own libraries shadow a
/// classifier a klib declares under the same name, while callable overloads from both are collected
/// and selected later, exactly as an extra jar on the classpath behaves.
pub struct PlatformWithKlibs {
    platform: Box<dyn SemanticPlatform>,
    klibs: KlibSymbols,
}

impl PlatformWithKlibs {
    pub fn new(platform: Box<dyn SemanticPlatform>, klibs: KlibSymbols) -> Self {
        PlatformWithKlibs { platform, klibs }
    }

    /// `platform`, with the klibs at `paths` federated under it — or `platform` itself when there
    /// are none. Whether a compilation needs the wrapper is a property of its dependency list, so a
    /// driver states the list and this decides, rather than every driver repeating the test.
    pub fn over(
        platform: Box<dyn SemanticPlatform>,
        paths: &[impl AsRef<Path>],
    ) -> Box<dyn SemanticPlatform> {
        if paths.is_empty() {
            return platform;
        }
        Box::new(Self::new(platform, KlibSymbols::open(paths)))
    }

    /// The federation this platform answers declaration queries through, in precedence order.
    fn federated(&self) -> CompositeSource<'_> {
        CompositeSource::new(vec![self.platform.as_ref(), &self.klibs])
    }
}

impl SymbolSource for PlatformWithKlibs {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        self.federated().package_exists(parent, name)
    }

    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
        self.federated().symbols(namespace, name)
    }

    /// A klib records no flexible types — the pairing is the JVM's platform-type relation — so the
    /// wrapped platform answers alone.
    fn platform_flexible_upper_bound(&self, lower: Ty) -> Ty {
        self.platform.platform_flexible_upper_bound(lower)
    }
}

impl PlatformWithKlibs {
    /// An alias expansion is a declaration lookup, so it federates rather than delegating: the
    /// platform's own aliases shadow, matching the classifier precedence, and a klib's answer stands
    /// where the platform has none.
    fn federated_alias_expansion(&self, internal: TypeName) -> Option<AliasExpansion> {
        self.platform
            .type_alias_expansion(internal)
            .or_else(|| self.klibs.type_alias_expansion(internal))
    }
}

/// Pass a platform query straight through to the wrapped platform. Every `SemanticPlatform` method
/// beyond declaration lookup is the platform's to answer, so each one is delegated verbatim rather
/// than left to the trait default, which would silently answer "no platform" for a wrapped platform
/// that does have an answer.
macro_rules! delegated {
    ($(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty;)*) => {
        $(
            fn $name(&self $(, $arg: $ty)*) -> $ret {
                self.platform.$name($($arg),*)
            }
        )*
    };
}

impl SemanticPlatform for PlatformWithKlibs {
    fn type_alias_expansion(&self, internal: TypeName) -> Option<AliasExpansion> {
        self.federated_alias_expansion(internal)
    }

    delegated! {
        fn install_source_module_headers(
            sources: &[PlatformSourceHeaderInput<'_>],
            source_classifiers: &[TypeName],
        ) -> Result<(), SourceHeaderError>;
        fn is_optional_expectation(classifier: TypeName) -> bool;
        fn internal_accessible(owner: TypeName) -> bool;
        fn function_type(arity: usize) -> Option<Ty>;
        fn value_underlying(ty: Ty) -> Option<Ty>;
        fn classifier_associated_property(internal: TypeName, name: &str) -> Option<PropertyInfo>;
        fn inherits_classifier_callables(internal: TypeName) -> bool;
        fn top_level_associated_property(package: TypeName, name: &str) -> Option<PropertyInfo>;
        fn external_property_diagnostic_label(
            property: crate::fir::ExternalPropertyId,
            name: &str,
            ty: Ty,
        ) -> Option<String>;
        fn library_value_form(ty: Ty) -> Ty;
        fn library_value_form_name(internal: TypeName) -> TypeName;
        fn canonical_source_type_name(internal: TypeName) -> TypeName;
        fn is_default_library_owner(internal: TypeName) -> bool;
        fn is_erased_contract_callable(callable: &LibraryCallable) -> bool;
        fn boxed_primitive(ty: Ty) -> Option<Ty>;
        fn reference_primitive(ty: Ty) -> Option<Ty>;
        fn extension_receiver_rank(recv: Ty, decl_recv: Ty) -> Option<u32>;
        fn function_like_arity(ty: Ty) -> Option<usize>;
        fn property_reference_type(arity: usize, mutable: bool, args: &[Ty]) -> Option<Ty>;
        fn function_reference_type(function: Ty) -> Option<Ty>;
        fn class_literal_type() -> Option<Ty>;
        fn intrinsic_property(receiver: Ty, name: &str) -> Option<LibraryMember>;
        fn implicit_common_supertypes(types: &[Ty]) -> Vec<SemanticSupertype>;
        fn platform_default_import_packages() -> &'static [&'static str];
        fn physical_property_getter_names(property: &str) -> Vec<String>;
        fn inherited_accessor_properties(
            source: &dyn SymbolSource,
            receiver: Ty,
            property: &str,
        ) -> PropertySet;
        fn builtin_type_internal(simple_name: &str) -> Option<String>;
        fn signature_formal_names(signature: &str) -> Vec<String>;
        fn iterable_element_type(internal: &str) -> Option<Ty>;
        fn iterable_element_type_name(internal: TypeName) -> Option<Ty>;
    }
}
