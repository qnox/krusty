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

use crate::libraries::{FunctionSet, LibraryType, PropertySet};
use crate::types::Ty;

/// A provider of declarations — a module's AST or a compiled library. The arg-independent metadata
/// surface that federates across sources; arg-dependent selection/binding lives above (the resolver).
pub trait SymbolSource {
    /// ALL overloads of function `name` applicable to a call — members + extensions (`receiver = Some`)
    /// or top-level functions (`receiver = None`) — in ONE query, each tagged with its `FnKind` and
    /// carrying full metadata (inline/`@InlineOnly`, return nullability, receiver rung). Empty by default.
    fn functions(&self, _name: &str, _receiver: Option<Ty>) -> FunctionSet {
        FunctionSet::default()
    }

    /// The shape of the type named `internal` — constructors, members, companion, supertypes, and the
    /// type-shape facts a resolver needs about it (formal type parameters, sealed subclasses, enum
    /// entries, value-class underlying, constructor named-parameter lists, value-class-typed properties).
    /// `None` if this source has no such type.
    fn resolve_type(&self, _internal: &str) -> Option<LibraryType> {
        None
    }

    /// ALL declarations of PROPERTY `name` applicable to an access — members + extensions
    /// (`receiver = Some`) or top-level properties (`receiver = None`) — in ONE query, symmetric to
    /// [`Self::functions`]. Each carries its [`crate::libraries::PropKind`], type, accessors, and
    /// visibility. Empty by default (a source with no such property). This is the seam that replaces
    /// resolving a property by guessing its physical getter name and routing it through `functions`.
    fn properties(&self, _name: &str, _receiver: Option<Ty>) -> PropertySet {
        PropertySet::default()
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
/// first/earliest contributor on a name clash.
#[derive(Default)]
pub struct CompositeSource {
    children: Vec<Box<dyn SymbolSource>>,
}

impl CompositeSource {
    /// Build a composite from sources in PRECEDENCE order (first shadows later).
    pub fn new(children: Vec<Box<dyn SymbolSource>>) -> Self {
        CompositeSource { children }
    }

    /// Append a source at the lowest precedence (consulted last).
    pub fn push(&mut self, source: Box<dyn SymbolSource>) {
        self.children.push(source);
    }
}

impl SymbolSource for CompositeSource {
    fn functions(&self, name: &str, receiver: Option<Ty>) -> FunctionSet {
        // Concatenate in precedence order — each `FunctionInfo` already carries its source's origin, and
        // selection is done per-source (ranks are not comparable across sources), so order is enough.
        FunctionSet {
            overloads: self
                .children
                .iter()
                .flat_map(|c| c.functions(name, receiver).overloads)
                .collect(),
        }
    }

    fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
        self.children.iter().find_map(|c| c.resolve_type(internal))
    }

    fn class_is_extensible(&self, internal: &str) -> bool {
        self.children
            .iter()
            .any(|c| c.class_is_extensible(internal))
    }

    fn properties(&self, name: &str, receiver: Option<Ty>) -> PropertySet {
        // Concatenate in precedence order, exactly like `functions` — selection (receiver rank) stays
        // per-source, so order is enough.
        PropertySet {
            overloads: self
                .children
                .iter()
                .flat_map(|c| c.properties(name, receiver).overloads)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        FnKind, FunctionInfo, GSig, LibraryCallable, LibraryType, PropKind, PropertyInfo,
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

    impl SymbolSource for FakeSource {
        fn functions(&self, name: &str, receiver: Option<Ty>) -> FunctionSet {
            if receiver.is_none() && self.fn_name.as_deref() == Some(name) {
                FunctionSet {
                    overloads: vec![FunctionInfo::plain(
                        FnKind::TopLevel,
                        None,
                        callable(&self.owner, name),
                    )],
                }
            } else {
                FunctionSet::default()
            }
        }
        fn properties(&self, name: &str, receiver: Option<Ty>) -> PropertySet {
            // A source provides ONE top-level property whose name matches its `fn_name` (reused as the
            // property name), owner-stamped so federation order is observable.
            if receiver.is_none() && self.fn_name.as_deref() == Some(name) {
                PropertySet {
                    overloads: vec![PropertyInfo {
                        kind: PropKind::TopLevel,
                        receiver: None,
                        formals: Vec::new(),
                        ty: GSig::Prim(Ty::Int),
                        getter: callable(&self.owner, name),
                        setter: None,
                        is_const: false,
                        visibility: Visibility::Public,
                        owner: self.owner.clone(),
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
                    supertypes: vec![self.owner.clone()],
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
                    sealed_subclasses: Vec::new(),
                    enum_entries: Vec::new(),
                    value_ctor_has_default: false,
                    ctor_named_params: Vec::new(),
                    value_class_properties: Vec::new(),
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
        let c = CompositeSource::new(vec![Box::new(module()), Box::new(library())]);
        let fs = c.functions("greet", None);
        // Both contribute; the module's (first) overload comes first.
        assert_eq!(fs.overloads.len(), 2);
        assert_eq!(fs.overloads[0].callable.owner, "module");
        assert_eq!(fs.overloads[1].callable.owner, "library");
    }

    #[test]
    fn functions_empty_when_no_source_has_name() {
        let c = CompositeSource::new(vec![Box::new(module()), Box::new(library())]);
        assert!(c.functions("absent", None).overloads.is_empty());
    }

    #[test]
    fn properties_concatenate_in_precedence_order() {
        // The property query federates exactly like `functions`: both sources contribute an overload of
        // `greet`, the module's (first) coming first, each keeping its own origin.
        let c = CompositeSource::new(vec![Box::new(module()), Box::new(library())]);
        let ps = c.properties("greet", None);
        assert_eq!(ps.overloads.len(), 2);
        assert_eq!(ps.overloads[0].owner, "module");
        assert_eq!(ps.overloads[1].owner, "library");
    }

    #[test]
    fn properties_empty_when_no_source_has_name() {
        let c = CompositeSource::new(vec![Box::new(module()), Box::new(library())]);
        assert!(c.properties("absent", None).overloads.is_empty());
        // A receiver-scoped query also finds nothing here (the fakes only provide top-level props).
        assert!(c
            .properties("greet", Some(Ty::obj("X")))
            .overloads
            .is_empty());
    }

    #[test]
    fn resolve_type_takes_the_earliest_source() {
        let c = CompositeSource::new(vec![Box::new(module()), Box::new(library())]);
        // Both define `shared`; the module (first) wins.
        let t = c.resolve_type("shared").expect("a shape");
        assert_eq!(t.supertypes, vec!["module".to_string()]);
    }

    #[test]
    fn resolve_type_falls_through_to_later_source() {
        // Only the library has `lib/only`.
        let lib = FakeSource {
            fn_name: None,
            owner: "library".into(),
            typed: Some("lib/only".into()),
        };
        let c = CompositeSource::new(vec![Box::new(module()), Box::new(lib)]);
        assert!(c.resolve_type("lib/only").is_some());
        assert!(c.resolve_type("nope").is_none());
    }

    #[test]
    fn nested_composite_is_a_source() {
        let inner = CompositeSource::new(vec![Box::new(module())]);
        let outer = CompositeSource::new(vec![Box::new(inner), Box::new(library())]);
        // Nesting works: the inner composite's module overload is found, library appends after.
        let fs = outer.functions("greet", None);
        assert_eq!(fs.overloads.len(), 2);
        assert_eq!(fs.overloads[0].callable.owner, "module");
    }

    #[test]
    fn push_appends_at_lowest_precedence() {
        let mut c = CompositeSource::new(vec![Box::new(module())]);
        c.push(Box::new(library()));
        let fs = c.functions("greet", None);
        assert_eq!(fs.overloads.len(), 2);
        // The pushed library is consulted last.
        assert_eq!(fs.overloads[1].callable.owner, "library");
    }

    #[test]
    fn empty_composite_has_no_functions_and_no_types() {
        let c = CompositeSource::default();
        assert!(c.functions("anything", None).overloads.is_empty());
        assert!(c.resolve_type("anything").is_none());
    }
}
