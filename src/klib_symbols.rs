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
//! Classes and functions are reported. Properties and package-level type aliases are not yet: a
//! property's accessors are shaped by the target that realizes them, and the reader does not decode
//! a `TypeAlias` declaration at all. Both are missing answers rather than wrong ones — a name that
//! is not reported simply does not resolve through this source.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use crate::klib::KlibArchive;
use crate::libraries::{
    CallSig, Callables, FnKind, FunctionInfo, FunctionSet, GenericSig, LibraryMember, LibraryType,
    PropertySet, ResolvedSymbols, TypeKind,
};
use crate::metadata::reader::{
    builtin_bounds, builtin_ty, library_type::library_type, parse_package_fragment, BuiltinMember,
    BuiltinTypeParam,
};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
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
                let mut declared: HashMap<String, FunctionSet> = HashMap::new();
                let mut order: Vec<String> = Vec::new();
                let mut members: Vec<LibraryMember> = Vec::new();
                for member in &declaration.members {
                    // A property's accessors are shaped by the target that realizes them; only the
                    // function half is reported here, and `is_property` keeps the split explicit
                    // rather than incidental.
                    if member.is_property {
                        continue;
                    }
                    let record = member_record(owner, declaration.kind, member, &class_bounds);
                    declared
                        .entry(member.name.clone())
                        .or_insert_with(|| {
                            order.push(member.name.clone());
                            FunctionSet::default()
                        })
                        .overloads
                        .push(FunctionInfo::classifier_member(
                            FnKind::Member,
                            owner,
                            record.clone(),
                        ));
                    members.push(record);
                }
                let mut classifier = library_type(declaration);
                classifier.declared_callables = declared
                    .into_iter()
                    .map(|(name, set)| (name, Callables::from_parts(set, PropertySet::default())))
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
                let receiver = function
                    .receiver
                    .as_ref()
                    .map(|receiver| builtin_ty(receiver, &bounds));
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
                };
                let kind = match receiver {
                    Some(_) => FnKind::Extension,
                    None => FnKind::TopLevel,
                };
                let mut record = member_record(package_name, TypeKind::Class, &member, &bounds);
                if let (Some(signature), Some(receiver)) = (&mut record.generic_sig, receiver) {
                    signature.receiver = Some(receiver);
                }
                let mut overload = FunctionInfo::classifier_member(kind, package_name, record);
                overload.receiver = receiver;
                self.top_level
                    .entry((package_name, function.name))
                    .or_default()
                    .overloads
                    .push(overload);
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
fn member_record(
    owner: TypeName,
    owner_kind: TypeKind,
    member: &BuiltinMember,
    owner_bounds: &HashMap<String, Ty>,
) -> LibraryMember {
    let bounds = builtin_bounds(&member.formals, owner_bounds);
    let params: Vec<Ty> = member
        .params
        .iter()
        .map(|parameter| builtin_ty(parameter, &bounds))
        .collect();
    let ret = builtin_ty(&member.ret, &bounds);
    let mut record = LibraryMember::new(member.name.clone(), params.clone(), ret, String::new());
    record.owner = Some(owner);
    record.set_ret_nullable(member.ret_nullable);
    record.set_is_operator(member.is_operator);
    record.set_is_infix(member.is_infix);
    record.set_is_abstract(member.is_abstract);
    // Whether the owner is an interface is a declaration fact the decoded kind already carries; how
    // a call to it dispatches is the backend's reading of that fact.
    record.set_is_interface(matches!(
        owner_kind,
        TypeKind::Interface | TypeKind::Annotation
    ));
    // The parameter names and defaults a named argument needs, which a descriptor erases.
    record.call_sig = CallSig::metadata_member(
        params.len(),
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
        receiver: None,
        params,
        ret,
        return_policy: Default::default(),
    });
    record
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
        let functions = match namespace {
            SymbolNamespace::Classifier(_) => FunctionSet::default(),
            SymbolNamespace::Package(package) => self
                .top_level
                .get(&(package, name.to_string()))
                .cloned()
                .unwrap_or_default(),
        };
        let classifier = namespace
            .existing_classifier(name)
            .and_then(|identity| self.classifiers.get(&identity).cloned());
        let classifier_name = classifier.as_ref().and(namespace.existing_classifier(name));
        Rc::new(ResolvedSymbols {
            classifier_name,
            classifier,
            callables: Callables::from_parts(functions, PropertySet::default()),
            importable_declaration: false,
        })
    }
}
