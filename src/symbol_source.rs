//! `SymbolSource` — the federatable seam shared by every provider of declarations.
//!
//! A *source* answers the two arg-independent questions resolution needs about a body of code: the
//! overloads of a name (`functions`) and the shape of a type (`resolve_type`, which also redirects a
//! `typealias` to its target). Both the current module (its AST decls) and a compiled library (a
//! classpath) are sources.
//!
//! Sources COMPOSE: a [`CompositeSource`] holds an ordered list of children and is itself a
//! `SymbolSource`, so `[current module, sibling modules, stdlib, extra jars]` federate uniformly with
//! first-source-wins precedence (user code shadows libraries). Selection of a single overload stays
//! INSIDE one source (an extension's receiver-MRO rank is only comparable within one type hierarchy);
//! the composite federates at the resolve boundary, never by flattening one global overload set.

use crate::libraries::{FunctionSet, LibraryType, PropertySet, ResolvedSymbols};
use crate::types::{Ty, TypeName};

/// A provider of declarations — a module's AST or a compiled library. The arg-independent metadata
/// surface that federates across sources; arg-dependent selection/binding lives above (the resolver).
pub trait SymbolSource {
    /// The instance-member overloads named `name` applicable on receiver `recv` (own + inherited), each an
    /// [`crate::libraries::FunctionInfo`] tagged with its receiver-MRO rung. UNLIKE the extension/top-level
    /// namespace (`resolve_symbols`, receiver-AGNOSTIC by fqn), a member is inherently RECEIVER-COUPLED: its
    /// return can bind to the receiver's type arguments (`Repo<Cfg>.byId(): Cfg`, a suspend `Continuation<T>`
    /// recovered from the receiver) — a decode only the platform that knows the receiver's shape can do. So
    /// members are their own receiver-parameterized query through the TYPE, not the fqn seam. Empty default.
    fn member_overloads(&self, _recv: Ty, _name: &str) -> FunctionSet {
        FunctionSet::default()
    }

    /// The shape of the type named `internal` — constructors, members, companion, supertypes, and the
    /// type-shape facts a resolver needs about it (formal type parameters, sealed subclasses, enum
    /// entries, value-class underlying, constructor named-parameter lists, value-class-typed properties).
    /// `None` if this source has no such type.
    fn resolve_type(&self, _internal: &str) -> Option<LibraryType> {
        None
    }

    /// Id-backed type lookup. Providers that already index by ids should override this; the default keeps
    /// legacy string-backed sources working while callers stop rendering names at each use site.
    fn resolve_type_name(&self, internal: TypeName) -> Option<LibraryType> {
        self.resolve_type(&internal.render())
    }

    /// Whether `internal` names a `@JvmInline value`/inline class — the value-class-ness attribute of the
    /// class SYMBOL, queried by name. THE authority the value-class pass and resolver consult, rather than
    /// a side "value-class set". Derived from the symbol's `value_underlying` shape.
    fn is_value(&self, internal: &str) -> bool {
        self.resolve_type(internal)
            .is_some_and(|t| t.value_underlying.is_some())
    }

    fn is_value_name(&self, internal: TypeName) -> bool {
        self.resolve_type_name(internal)
            .is_some_and(|t| t.value_underlying.is_some())
    }

    /// Resolve a fully-qualified name to its namespace record (classifier + callables) — THE FQN query
    /// this source answers. The resolver forms candidate FQNs from the file's import scope and unions the
    /// results across candidates + sources; this returns just what THIS source has at `fqn`. `receiver`
    /// Receiver-coupled work (value-class receivers, `@JvmName` element variants, return binding) is
    /// SELECTION + emit, done by the consumer — resolution is purely by fqn. Empty by default.
    fn resolve_symbols(&self, _fqn: &str) -> ResolvedSymbols {
        ResolvedSymbols::default()
    }

    /// The MEMBER-property declarations named `name` on receiver `recv` (own + inherited), each with its
    /// [`crate::libraries::PropKind`], type, accessors, and visibility — symmetric to [`Self::member_overloads`]
    /// and RECEIVER-COUPLED for the same reason (a member property is a shape of the type). Extension/top-level
    /// properties are surfaced by [`Self::resolve_symbols`]. Empty by default (a source with no such property).
    fn property_members(&self, _recv: Ty, _name: &str) -> PropertySet {
        PropertySet::default()
    }

    /// Whether `name` on `recv` (its type + supertype closure) is declared a PROPERTY rather than a
    /// function — the authoritative classifier a callable reference needs to choose a `KProperty`
    /// (`s::length`) over a zero-arg method reference (`it::next`). Both may otherwise resolve to a
    /// zero-arg readable member, so the getter-guessing resolver cannot tell them apart. Default `false`.
    fn member_is_property(&self, _recv: Ty, _name: &str) -> bool {
        false
    }

    /// Whether `internal` names a plain class this source can be used as a SUPERCLASS of an emitted
    /// user class: a concrete (non-`final`, non-`abstract`) non-interface class that actually exists.
    /// A `final` base can't be inherited, an `abstract` base needs abstract-method/bridge synthesis the
    /// backend doesn't do, and an interface isn't a superclass — all must be rejected before emitting a
    /// `super(…)` to it. `false` by default (a source with no such class).
    fn class_is_extensible(&self, _internal: &str) -> bool {
        false
    }
}

/// An ordered federation of sources — itself a [`SymbolSource`], so it nests. Earlier children win:
/// `functions` concatenates in order (each overload keeps its own origin), `resolve_type` takes the
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
    fn member_overloads(&self, recv: Ty, name: &str) -> FunctionSet {
        // Concatenate in precedence order — each `FunctionInfo` already carries its source's origin, and
        // selection is done per-source (ranks are not comparable across sources), so order is enough.
        FunctionSet {
            overloads: self
                .children
                .iter()
                .flat_map(|c| c.member_overloads(recv, name).overloads)
                .collect(),
        }
    }

    fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
        self.children.iter().find_map(|c| c.resolve_type(internal))
    }

    fn resolve_type_name(&self, internal: TypeName) -> Option<LibraryType> {
        self.children
            .iter()
            .find_map(|c| c.resolve_type_name(internal))
    }

    fn resolve_symbols(&self, fqn: &str) -> ResolvedSymbols {
        use crate::libraries::Callables;
        // Classifier: first source wins (user shadows library). Callables: concatenate in precedence
        // order (each overload keeps its origin) — functions XOR a property, so take whichever appears.
        let mut classifier = None;
        let mut fns = Vec::new();
        let mut props = Vec::new();
        for c in &self.children {
            let r = c.resolve_symbols(fqn);
            if classifier.is_none() {
                classifier = r.classifier;
            }
            match r.callables {
                Callables::Functions(f) => fns.extend(f.overloads),
                Callables::Properties(p) => props.extend(p.overloads),
                Callables::None => {}
            }
        }
        let callables = if !fns.is_empty() {
            Callables::Functions(FunctionSet { overloads: fns })
        } else if !props.is_empty() {
            Callables::Properties(PropertySet { overloads: props })
        } else {
            Callables::None
        };
        ResolvedSymbols {
            classifier,
            callables,
        }
    }

    fn class_is_extensible(&self, internal: &str) -> bool {
        self.children
            .iter()
            .any(|c| c.class_is_extensible(internal))
    }

    fn property_members(&self, recv: Ty, name: &str) -> PropertySet {
        // Concatenate in precedence order, exactly like `member_overloads` — selection (receiver rank)
        // stays per-source, so order is enough.
        PropertySet {
            overloads: self
                .children
                .iter()
                .flat_map(|c| c.property_members(recv, name).overloads)
                .collect(),
        }
    }

    fn member_is_property(&self, recv: Ty, name: &str) -> bool {
        self.children
            .iter()
            .any(|c| c.member_is_property(recv, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        FnKind, FunctionInfo, LibraryCallable, LibraryType, PropKind, PropertyInfo, Visibility,
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

    impl SymbolSource for FakeSource {
        fn member_overloads(&self, _recv: Ty, name: &str) -> FunctionSet {
            // A source provides ONE member overload of its chosen name, owner-stamped so federation order
            // is observable.
            if self.fn_name.as_deref() == Some(name) {
                FunctionSet {
                    overloads: vec![FunctionInfo::plain(
                        FnKind::Member,
                        None,
                        callable(&self.owner, name),
                    )],
                }
            } else {
                FunctionSet::default()
            }
        }
        fn property_members(&self, _recv: Ty, name: &str) -> PropertySet {
            // A source provides ONE member property whose name matches its `fn_name`, owner-stamped so
            // federation order is observable.
            if self.fn_name.as_deref() == Some(name) {
                PropertySet {
                    overloads: vec![PropertyInfo {
                        kind: PropKind::Member,
                        receiver: None,
                        formals: Vec::new(),
                        ty: Ty::Int,
                        getter: callable(&self.owner, name),
                        setter: None,
                        is_const: false,
                        visibility: Visibility::Public,
                        owner: self.owner.as_str().into(),
                        receiver_rank: 0,
                    }],
                }
            } else {
                PropertySet::default()
            }
        }
        fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
            if self.typed.as_deref() == Some(internal) {
                Some(LibraryType {
                    is_public: true,
                    kind: crate::libraries::TypeKind::Class,
                    supertypes: vec![self.owner.clone()].into(),
                    constructors: vec![],
                    members: vec![],
                    companion: vec![],
                    companion_consts: std::collections::HashMap::new(),
                    sam_method: None,
                    companion_object: None,
                    value_companion_fns: Vec::new(),
                    value_underlying: None,
                    alias_target: None,
                    type_params: Vec::new(),
                    sealed_subclasses: crate::types::TypeNameList::new(),
                    enum_entries: Vec::new(),
                    value_ctor_has_default: false,
                    ctor_named_params: Vec::new(),
                    value_class_properties: Vec::new(),
                    retention: None,
                })
            } else {
                None
            }
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
        let fs = c.member_overloads(Ty::obj("R"), "greet");
        // Both contribute; the module's (first) overload comes first.
        assert_eq!(fs.overloads.len(), 2);
        assert!(fs.overloads[0].callable.owner.matches("module"));
        assert!(fs.overloads[1].callable.owner.matches("library"));
    }

    #[test]
    fn functions_empty_when_no_source_has_name() {
        let m = module();
        let l = library();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &l]);
        assert!(c
            .member_overloads(Ty::obj("R"), "absent")
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
        let ps = c.property_members(Ty::obj("R"), "greet");
        assert_eq!(ps.overloads.len(), 2);
        assert!(ps.overloads[0].owner.matches("module"));
        assert!(ps.overloads[1].owner.matches("library"));
    }

    #[test]
    fn properties_empty_when_no_source_has_name() {
        let m = module();
        let l = library();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &l]);
        assert!(c
            .property_members(Ty::obj("R"), "absent")
            .overloads
            .is_empty());
        // A receiver-scoped query also finds nothing here (the fakes only provide top-level props).
        assert!(c
            .property_members(Ty::obj("X"), "absent")
            .overloads
            .is_empty());
    }

    #[test]
    fn resolve_type_takes_the_earliest_source() {
        let m = module();
        let l = library();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &l]);
        // Both define `shared`; the module (first) wins.
        let t = c.resolve_type("shared").expect("a shape");
        assert_eq!(t.supertypes.to_vec(), vec!["module".to_string()]);
    }

    #[test]
    fn resolve_type_falls_through_to_later_source() {
        // Only the library has `lib/only`.
        let lib = FakeSource {
            fn_name: None,
            owner: "library".into(),
            typed: Some("lib/only".into()),
        };
        let m = module();
        let c = CompositeSource::new(vec![&m as &dyn SymbolSource, &lib]);
        assert!(c.resolve_type("lib/only").is_some());
        assert!(c.resolve_type("nope").is_none());
    }

    #[test]
    fn nested_composite_is_a_source() {
        let m = module();
        let inner = CompositeSource::new(vec![&m as &dyn SymbolSource]);
        let l = library();
        let outer = CompositeSource::new(vec![&inner as &dyn SymbolSource, &l]);
        // Nesting works: the inner composite's module overload is found, library appends after.
        let fs = outer.member_overloads(Ty::obj("R"), "greet");
        assert_eq!(fs.overloads.len(), 2);
        assert!(fs.overloads[0].callable.owner.matches("module"));
    }

    #[test]
    fn push_appends_at_lowest_precedence() {
        let m = module();
        let l = library();
        let mut c = CompositeSource::new(vec![&m as &dyn SymbolSource]);
        c.push(&l);
        let fs = c.member_overloads(Ty::obj("R"), "greet");
        assert_eq!(fs.overloads.len(), 2);
        // The pushed library is consulted last.
        assert!(fs.overloads[1].callable.owner.matches("library"));
    }

    #[test]
    fn empty_composite_has_no_functions_and_no_types() {
        let c = CompositeSource::default();
        assert!(c
            .member_overloads(Ty::obj("R"), "anything")
            .overloads
            .is_empty());
        assert!(c.resolve_type("anything").is_none());
    }
}
