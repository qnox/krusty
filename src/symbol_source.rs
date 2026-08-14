//! `SymbolSource` — the federatable seam shared by every provider of declarations.
//!
//! A source answers one arg-independent question: what declarations occupy `(namespace, name)`.
//! A namespace is always explicit: either a package (including [`TypeName::ROOT`] for the default
//! package) or a classifier. The leaf stays textual because most probes are callable names, not
//! classifier identities, and a miss must not add an arbitrary leaf to the global type-name tree. The
//! returned record contains both Kotlin declaration namespaces at that key: classifier metadata and
//! callable signatures. Both the current module and compiled libraries implement this same lookup.
//!
//! Sources COMPOSE: a [`CompositeSource`] holds an ordered list of children and is itself a
//! `SymbolSource`, so `[current module, sibling modules, stdlib, extra jars]` federate uniformly.
//! Classifiers use first-source-wins precedence; callable overloads from every contributing source are
//! collected together and selected later by [`crate::symbol_resolver::SymbolResolver`].

pub use crate::libraries::ClassifierAccess;
use crate::libraries::{FunctionSet, LibraryType, PropertySet, ResolvedSymbols};
use crate::types::TypeName;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SymbolNamespace {
    /// A package namespace. [`TypeName::ROOT`] represents the default package; it is not absence.
    Package(TypeName),
    /// A classifier namespace, used by nested classifiers and member callables alike.
    Classifier(TypeName),
}

impl SymbolNamespace {
    pub fn name(self) -> TypeName {
        match self {
            Self::Package(name) | Self::Classifier(name) => name,
        }
    }

    /// Decompose a classifier identity into its semantic namespace and final source segment. A
    /// top-level classifier always has a package namespace, including `Package(TypeName::ROOT)`;
    /// a nested classifier has its immediate classifier as namespace.
    pub fn classifier_key(internal: TypeName) -> (Self, &'static str) {
        internal.nested_owner().map_or_else(
            || (Self::Package(internal.namespace()), internal.segment_ref()),
            |owner| (Self::Classifier(owner), internal.nested_segment_ref()),
        )
    }

    /// Read an already-interned classifier child at this namespace without interning a miss.
    pub fn existing_classifier(self, name: &str) -> Option<TypeName> {
        match self {
            Self::Package(package) => crate::types::existing_type_name_child(package, name),
            Self::Classifier(owner) => crate::types::existing_type_name_nested_child(owner, name),
        }
    }
}

/// A provider of declarations — a module's AST or a compiled library. The arg-independent metadata
/// surface that federates across sources; arg-dependent selection/binding lives above (the resolver).
pub trait SymbolSource {
    /// Core projection of the classifier half of [`Self::symbols`]. Providers must not override this;
    /// the implementation lives here solely while call sites migrate to reading the record directly.
    fn classifier(&self, internal: TypeName) -> Option<std::rc::Rc<LibraryType>> {
        let (namespace, name) = SymbolNamespace::classifier_key(internal);
        self.symbols(namespace, name).classifier.clone()
    }

    /// Whether `name` is a package directly inside `parent` in this source.
    ///
    /// A Kotlin reference is resolved one segment at a time, and each prefix denotes a package, a
    /// classifier, or nothing. Classifiers are carried by [`Self::symbols`]; packages are the one
    /// namespace fact not representable by a declaration record,
    /// without which a fully-qualified reference (`java.util.ArrayList`, `pkg.topLevelFun()`) has no
    /// way to know that its leading segments are a package rather than an unresolved name. Intermediate
    /// packages that declare nothing themselves must answer `true`. Empty default — a source with no
    /// package namespace of its own contributes nothing to the walk.
    fn package_exists(&self, _parent: TypeName, _name: &str) -> bool {
        false
    }

    /// Return the complete declaration record for one `(namespace, name)` key. This is the sole
    /// declaration API. `Package(TypeName::ROOT)` is the default package. Providers may map the key to
    /// an already-interned classifier, but must not intern it merely because it was probed. Parallel
    /// classifier/member/visibility queries whose answers can disagree with this record must not be
    /// exposed.
    fn symbols(&self, _namespace: SymbolNamespace, _name: &str) -> std::rc::Rc<ResolvedSymbols> {
        std::rc::Rc::new(ResolvedSymbols::default())
    }
}

/// An ordered federation of sources — itself a [`SymbolSource`], so it nests. Earlier children win:
/// Callables concatenate in order (each overload keeps its own origin); the classifier comes from the
/// first/earliest contributor on a name clash. Holds children by REFERENCE so a resolver can federate the
/// borrowed live sources (the current module over the classpath) without allocation or moving them.
#[derive(Default)]
pub struct CompositeSource<'a> {
    children: Vec<&'a dyn SymbolSource>,
}

impl<'a> CompositeSource<'a> {
    /// Build a composite from sources in PRECEDENCE order (first shadows later).
    pub fn new(children: Vec<&'a dyn SymbolSource>) -> Self {
        CompositeSource { children }
    }

    /// Append a source at the lowest precedence (consulted last).
    pub fn push(&mut self, source: &'a dyn SymbolSource) {
        self.children.push(source);
    }
}

impl SymbolSource for CompositeSource<'_> {
    /// A package exists if ANY source declares it — packages are a union across the module and the
    /// classpath, not a shadowing lookup: the same package name legitimately holds declarations from
    /// both, and a qualifier walk must be able to continue through either.
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        self.children.iter().any(|c| c.package_exists(parent, name))
    }

    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
        use crate::libraries::Callables;
        // Classifier: first source wins (user shadows library). Callables: concatenate in precedence
        // order (each overload keeps its origin) — functions XOR a property, so take whichever appears.
        // A single contributing source (the common case) passes its record through unmerged.
        let mut records: Vec<std::rc::Rc<ResolvedSymbols>> = Vec::new();
        for c in &self.children {
            let r = c.symbols(namespace, name);
            if !r.is_empty() {
                records.push(r);
            }
        }
        match records.len() {
            0 => std::rc::Rc::new(ResolvedSymbols::default()),
            1 => records.pop().expect("one record"),
            _ => {
                let mut classifier = None;
                let mut classifier_name = None;
                let mut fns = Vec::new();
                let mut props = Vec::new();
                for r in &records {
                    if classifier.is_none() {
                        classifier = r.classifier.clone();
                        classifier_name = r.classifier_name;
                    }
                    match &r.callables {
                        Callables::Functions(f) => fns.extend(f.overloads.iter().cloned()),
                        Callables::Properties(p) => props.extend(p.overloads.iter().cloned()),
                        Callables::Both {
                            functions,
                            properties,
                        } => {
                            fns.extend(functions.overloads.iter().cloned());
                            props.extend(properties.overloads.iter().cloned());
                        }
                        Callables::None => {}
                    }
                }
                let callables = match (fns.is_empty(), props.is_empty()) {
                    (false, false) => Callables::Both {
                        functions: FunctionSet { overloads: fns },
                        properties: PropertySet { overloads: props },
                    },
                    (false, true) => Callables::Functions(FunctionSet { overloads: fns }),
                    (true, false) => Callables::Properties(PropertySet { overloads: props }),
                    (true, true) => Callables::None,
                };
                std::rc::Rc::new(ResolvedSymbols {
                    classifier_name,
                    classifier,
                    callables,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        Callables, FnKind, FunctionInfo, LibraryCallable, LibraryType, PropKind, PropertyInfo,
        Visibility,
    };
    use crate::types::Ty;

    /// A minimal source: one top-level overload of a chosen name, one type shape.
    struct FakeSource {
        fn_name: Option<String>, // a top-level fn this source provides
        owner: String,           // owner stamped on its callable (proxy for "origin")
        typed: Option<String>,   // an internal name this source has a shape for
    }

    fn callable(owner: &str, name: &str) -> LibraryCallable {
        LibraryCallable::library(owner, name, vec![], Ty::Unit, Ty::Unit, "()V")
    }

    fn declared(source: &dyn SymbolSource, receiver: Ty, name: &str) -> Callables {
        crate::symbol_resolver::declared_member_callables(source, receiver, name)
    }

    impl FakeSource {
        fn classifier_record(&self, internal: TypeName) -> Option<std::rc::Rc<LibraryType>> {
            if self
                .typed
                .as_deref()
                .is_some_and(|name| internal.matches(name))
            {
                let mut declared_callables = std::collections::HashMap::new();
                if let Some(name) = &self.fn_name {
                    declared_callables.insert(
                        name.clone(),
                        Callables::from_parts(
                            FunctionSet {
                                overloads: vec![FunctionInfo::plain(
                                    FnKind::Member,
                                    None,
                                    callable(&self.owner, name),
                                )],
                            },
                            PropertySet {
                                overloads: vec![PropertyInfo {
                                    name: name.clone(),
                                    kind: PropKind::Member,
                                    receiver: None,
                                    formals: Vec::new(),
                                    ty: Ty::Int,
                                    context_count: 0,
                                    getter: callable(&self.owner, name),
                                    setter: None,
                                    setter_visibility: Visibility::Public,
                                    is_const: false,
                                    visibility: Visibility::Public,
                                    owner: self.owner.as_str().into(),
                                    receiver_rank: 0,
                                    source_key: None,
                                    source_member: None,
                                }],
                            },
                        ),
                    );
                }
                Some(std::rc::Rc::new(LibraryType {
                    access: crate::libraries::ClassifierAccess::Public,
                    source_file: None,
                    is_nested: false,
                    outer_instance: None,
                    kind: crate::libraries::TypeKind::Class,
                    inheritance: crate::libraries::ClassifierInheritance {
                        is_extensible: self.owner == "library",
                        ..Default::default()
                    },
                    supertypes: vec![self.owner.clone()].into(),
                    supertype_templates: vec![Ty::obj(&self.owner)],
                    constructors: vec![],
                    fields: vec![],
                    declared_callables,
                    members: vec![],
                    companion: vec![],
                    constants: std::collections::HashMap::new(),
                    sam_method: None,
                    callable_signature: None,
                    companion_object: None,
                    value_companion_fns: Vec::new(),
                    value_underlying: None,
                    value_underlying_property: None,
                    alias_target: None,
                    type_parameters: crate::types::TypeParameters::default(),
                    sealed_subclasses: crate::types::TypeNameList::new(),
                    enum_entries: Vec::new(),
                    enum_entries_accessor: None,
                    ctor_named_params: Vec::new(),
                    retention: None,
                }))
            } else {
                None
            }
        }
    }

    impl SymbolSource for FakeSource {
        fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
            // The record at `(namespace, name)`: this fake's type (when `typed` matches) and its one top-level
            // overload (when `fn_name` matches).
            let classifier_name = namespace.existing_classifier(name);
            let classifier = classifier_name.and_then(|name| self.classifier_record(name));
            let callables = if namespace == SymbolNamespace::Package(TypeName::ROOT)
                && self.fn_name.as_deref() == Some(name)
            {
                crate::libraries::Callables::Functions(FunctionSet {
                    overloads: vec![FunctionInfo::plain(
                        FnKind::TopLevel,
                        None,
                        callable(&self.owner, name),
                    )],
                })
            } else {
                crate::libraries::Callables::None
            };
            std::rc::Rc::new(ResolvedSymbols {
                classifier_name: classifier.as_ref().map(|classifier| {
                    classifier
                        .alias_target
                        .unwrap_or_else(|| classifier_name.expect("classifier identity"))
                }),
                classifier,
                callables,
            })
        }
    }

    fn module() -> FakeSource {
        FakeSource {
            fn_name: Some("greet".into()),
            owner: "module".into(),
            typed: Some("shared".into()),
        }
    }

    fn library() -> FakeSource {
        FakeSource {
            fn_name: Some("greet".into()), // clashes with module on `greet`
            owner: "library".into(),
            typed: Some("shared".into()), // clashes with module on `shared`
        }
    }

    #[test]
    fn functions_concatenates_in_precedence_order() {
        let m = module();
        let l = library();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &l]);
        let fs = declared(&c, Ty::obj("shared"), "greet").into_parts().0;
        // The classifier record follows first-source precedence.
        assert_eq!(fs.overloads.len(), 1);
        assert!(fs.overloads[0].callable.owner.matches("module"));
    }

    #[test]
    fn functions_empty_when_no_source_has_name() {
        let m = module();
        let l = library();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &l]);
        assert!(declared(&c, Ty::obj("shared"), "absent")
            .into_parts()
            .0
            .overloads
            .is_empty());
    }

    #[test]
    fn properties_concatenate_in_precedence_order() {
        // The property query federates exactly like `functions`: both sources contribute an overload of
        // `greet`, the module's (first) coming first, each keeping its own origin.
        let m = module();
        let l = library();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &l]);
        let ps = declared(&c, Ty::obj("shared"), "greet").into_parts().1;
        assert_eq!(ps.overloads.len(), 1);
        assert!(ps.overloads[0].owner.matches("module"));
    }

    #[test]
    fn properties_empty_when_no_source_has_name() {
        let m = module();
        let l = library();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &l]);
        assert!(declared(&c, Ty::obj("shared"), "absent")
            .into_parts()
            .1
            .overloads
            .is_empty());
        // A receiver-scoped query also finds nothing here (the fakes only provide top-level props).
        assert!(declared(&c, Ty::obj("X"), "absent")
            .into_parts()
            .1
            .overloads
            .is_empty());
    }

    #[test]
    fn classifier_takes_the_earliest_source() {
        let m = module();
        let l = library();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &l]);
        // Both define `shared`; the module (first) wins.
        let t = c
            .classifier(crate::types::type_name("shared"))
            .expect("a shape");
        assert_eq!(t.supertypes.to_vec(), vec!["module".to_string()]);
        assert!(!t.inheritance.is_extensible);
        assert!(
            l.classifier(crate::types::type_name("shared"))
                .expect("a library classifier")
                .inheritance
                .is_extensible
        );
    }

    #[test]
    fn classifier_comes_from_the_first_source_that_declares_it() {
        // Only the library has `lib/only`.
        let lib = FakeSource {
            fn_name: None,
            owner: "library".into(),
            typed: Some("lib/only".into()),
        };
        let m = module();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &lib]);
        assert!(c.classifier(crate::types::type_name("lib/only")).is_some());
        assert!(c.classifier(crate::types::type_name("nope")).is_none());
    }

    #[test]
    fn nested_composite_is_a_source() {
        let m = module();
        let inner = CompositeSource::new(vec![&m as &dyn SymbolSource]);
        let l = library();
        let outer = CompositeSource::new(vec![&inner as &dyn SymbolSource, &l]);
        // Nesting works: the inner composite's module overload is found, library appends after.
        let fs = declared(&outer, Ty::obj("shared"), "greet").into_parts().0;
        assert_eq!(fs.overloads.len(), 1);
        assert!(fs.overloads[0].callable.owner.matches("module"));
    }

    #[test]
    fn push_appends_at_lowest_precedence() {
        let m = module();
        let l = library();
        let mut c = CompositeSource::new(vec![&m as &dyn SymbolSource]);
        c.push(&l);
        let fs = declared(&c, Ty::obj("shared"), "greet").into_parts().0;
        assert_eq!(fs.overloads.len(), 1);
        assert!(fs.overloads[0].callable.owner.matches("module"));
    }

    #[test]
    fn empty_composite_has_no_functions_and_no_types() {
        let c = CompositeSource::default();
        assert!(declared(&c, Ty::obj("R"), "anything")
            .into_parts()
            .0
            .overloads
            .is_empty());
        assert!(c.classifier(crate::types::type_name("anything")).is_none());
    }

    #[test]
    fn symbols_passes_a_single_contributor_through() {
        // Only the library answers `greet`: the composite must surface its record intact (the
        // single-record fast path) and stay empty for an unknown name.
        let m = FakeSource {
            fn_name: None,
            owner: "module".into(),
            typed: None,
        };
        let l = library();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &l]);
        let r = c.symbols(SymbolNamespace::Package(TypeName::ROOT), "greet");
        assert!(r.classifier.is_none());
        match &r.callables {
            crate::libraries::Callables::Functions(f) => {
                assert_eq!(f.overloads.len(), 1);
                assert!(f.overloads[0].callable.owner.matches("library"));
            }
            _ => panic!("expected functions"),
        }
        assert!(c
            .symbols(SymbolNamespace::Package(TypeName::ROOT), "missing")
            .is_empty());
    }

    #[test]
    fn symbols_merges_classifier_and_callables_across_children() {
        // The module knows the TYPE `shared`, the library the FUNCTION `shared` — the merged record
        // carries both namespaces, classifier from the earliest contributor, overloads concatenated in
        // precedence order.
        let m = FakeSource {
            fn_name: Some("shared".into()),
            owner: "module".into(),
            typed: Some("shared".into()),
        };
        let l = FakeSource {
            fn_name: Some("shared".into()),
            owner: "library".into(),
            typed: Some("shared".into()),
        };
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &l]);
        let r = c.symbols(SymbolNamespace::Package(TypeName::ROOT), "shared");
        let classifier = r
            .classifier
            .as_ref()
            .expect("merged record keeps the classifier");
        assert!(classifier.supertypes.contains("module"));
        match &r.callables {
            crate::libraries::Callables::Functions(f) => {
                assert_eq!(f.overloads.len(), 2);
                assert!(f.overloads[0].callable.owner.matches("module"));
                assert!(f.overloads[1].callable.owner.matches("library"));
            }
            _ => panic!("expected functions"),
        }
    }

    #[test]
    fn missing_textual_symbol_probe_does_not_intern_a_type_name() {
        const MISSING: &str = "property_name_that_must_never_enter_the_type_tree_7f36a9";
        assert!(crate::types::existing_type_name(MISSING).is_none());

        let source = FakeSource {
            fn_name: None,
            owner: "empty".into(),
            typed: None,
        };
        assert!(source
            .symbols(SymbolNamespace::Package(TypeName::ROOT), MISSING)
            .is_empty());

        assert!(
            crate::types::existing_type_name(MISSING).is_none(),
            "a callable miss must remain a string and must not pollute the classifier name tree"
        );
    }

    #[test]
    fn classifier_keys_preserve_root_packages_and_immediate_classifier_owners() {
        let top = crate::types::type_name("Top");
        assert_eq!(
            SymbolNamespace::classifier_key(top),
            (SymbolNamespace::Package(TypeName::ROOT), "Top")
        );

        let nested = crate::types::type_name("sample/Outer$Inner$Deep");
        assert_eq!(
            SymbolNamespace::classifier_key(nested),
            (
                SymbolNamespace::Classifier(crate::types::type_name("sample/Outer$Inner")),
                "Deep",
            )
        );
    }
}
