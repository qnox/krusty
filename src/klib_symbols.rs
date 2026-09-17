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
    Callables, FnFlags, FnKind, FunctionInfo, FunctionSet, LibraryCallable, LibraryType,
    PropertySet, ResolvedSymbols, ReturnInfo,
};
use crate::metadata::reader::{
    builtin_bounds, builtin_ty, library_type::library_type, parse_package_fragment, BuiltinMember,
};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, Ty, TypeName};

/// Declarations read out of one or more klibs.
#[derive(Default)]
pub struct KlibSymbols {
    classifiers: HashMap<TypeName, Arc<LibraryType>>,
    /// Every package any klib declares into, and every prefix of one: a qualifier walk resolves one
    /// segment at a time, so an intermediate package that declares nothing itself must still exist.
    packages: HashSet<(TypeName, String)>,
    /// Member functions, keyed by their owning classifier and source name.
    members: HashMap<(TypeName, String), FunctionSet>,
    /// Top-level functions, keyed by their package and source name.
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
                let bounds = builtin_bounds(&declaration.type_params, &HashMap::new());
                for member in &declaration.members {
                    // A property's accessors are shaped by the target that realizes them; only the
                    // function half is reported here, and `is_property` keeps the split explicit
                    // rather than incidental.
                    if member.is_property {
                        continue;
                    }
                    let overload = function_info(owner, member, &bounds, FnKind::Member);
                    self.members
                        .entry((owner, member.name.clone()))
                        .or_default()
                        .overloads
                        .push(overload);
                }
                self.classifiers
                    .entry(owner)
                    .or_insert_with(|| Arc::new(library_type(declaration)));
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
                };
                let receiver = function
                    .receiver
                    .as_ref()
                    .map(|receiver| builtin_ty(receiver, &bounds));
                let kind = match receiver {
                    Some(_) => FnKind::Extension,
                    None => FnKind::TopLevel,
                };
                let mut overload = function_info(package_name, &member, &bounds, kind);
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

/// One overload, from a decoded member.
fn function_info(
    owner: TypeName,
    member: &BuiltinMember,
    bounds: &HashMap<String, Ty>,
    kind: FnKind,
) -> FunctionInfo {
    let params: Vec<Ty> = member
        .params
        .iter()
        .map(|parameter| builtin_ty(parameter, bounds))
        .collect();
    let ret = builtin_ty(&member.ret, bounds);
    let callable = LibraryCallable::library(
        owner,
        member.name.clone(),
        params,
        ret,
        ret,
        // No descriptor: a klib records none, and inventing one would be this layer deciding a
        // target's representation.
        String::new(),
    );
    FunctionInfo {
        ret: ReturnInfo::new(member.ret_nullable, None),
        flags: FnFlags {
            operator: member.is_operator,
            infix: member.is_infix,
            is_abstract: member.is_abstract,
            ..FnFlags::default()
        },
        // The parameter names and defaults a named argument needs, which a descriptor erases.
        call_sig: crate::libraries::CallSig::metadata_member(
            member.params.len(),
            member.param_names.clone(),
            member.param_defaults.clone(),
            None,
        ),
        ..FunctionInfo::plain(kind, None, callable)
    }
}

impl SymbolSource for KlibSymbols {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        self.packages.contains(&(parent, name.to_string()))
    }

    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
        let functions = match namespace {
            SymbolNamespace::Classifier(owner) => self.members.get(&(owner, name.to_string())),
            SymbolNamespace::Package(package) => self.top_level.get(&(package, name.to_string())),
        }
        .cloned()
        .unwrap_or_default();
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
