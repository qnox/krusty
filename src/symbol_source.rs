//! `SymbolSource` — the federatable seam shared by every provider of declarations.
//!
//! A *source* answers the three arg-independent questions resolution needs about a body of code: the
//! type universe it contributes (`seed`), the overloads of a name (`functions`), and the shape of a
//! type (`resolve_type`). Both the current module (its AST decls) and a compiled library (a classpath)
//! are sources; [`crate::libraries::LibrarySet`] is a `SymbolSource` plus the JVM-emit extras.
//!
//! Sources COMPOSE: a [`CompositeSource`] holds an ordered list of children and is itself a
//! `SymbolSource`, so `[current module, sibling modules, stdlib, extra jars]` federate uniformly with
//! first-source-wins precedence (user code shadows libraries). Selection of a single overload stays
//! INSIDE one source (an extension's receiver-MRO rank is only comparable within one type hierarchy);
//! the composite federates at the resolve boundary, never by flattening one global overload set.

use crate::libraries::{FunctionSet, LibrarySeed, LibraryType};
use crate::types::Ty;

/// A provider of declarations — a module's AST or a compiled library. The arg-independent metadata
/// surface that federates across sources; arg-dependent selection/binding lives above (the resolver).
pub trait SymbolSource {
    /// The type universe this source contributes, resolved to internal names (simple name → internal,
    /// plus type aliases). Empty by default.
    fn seed(&self) -> LibrarySeed {
        LibrarySeed::default()
    }

    /// ALL overloads of function `name` applicable to a call — members + extensions (`receiver = Some`)
    /// or top-level functions (`receiver = None`) — in ONE query, each tagged with its `FnKind` and
    /// carrying full metadata (inline/`@InlineOnly`, return nullability, receiver rung). Empty by default.
    fn functions(&self, _name: &str, _receiver: Option<Ty>) -> FunctionSet {
        FunctionSet::default()
    }

    /// The shape of the type named `internal` (constructors, members, companion, supertypes), or `None`
    /// if this source has no such type.
    fn resolve_type(&self, _internal: &str) -> Option<LibraryType> {
        None
    }
}

/// An ordered federation of sources — itself a [`SymbolSource`], so it nests. Earlier children win:
/// `functions` concatenates in order (each overload keeps its own origin), `resolve_type`/`seed` take
/// the first/earliest contributor on a name clash.
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
    fn seed(&self) -> LibrarySeed {
        // Merge with EARLIEST-wins: fill from the lowest-precedence child first, then let each more
        // specific child overwrite, so a name a high-precedence source defines shadows the rest.
        let mut seed = LibrarySeed::default();
        for child in self.children.iter().rev() {
            let s = child.seed();
            seed.class_names.extend(s.class_names);
            seed.type_aliases.extend(s.type_aliases);
        }
        seed
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        FnFlags, FnKind, FunctionInfo, LibraryCallable, LibrarySeed, LibraryType,
    };
    use crate::types::Ty;

    /// A minimal source: a few class names and one top-level overload of a chosen name.
    struct FakeSource {
        class: Option<(String, String)>, // simple -> internal
        fn_name: Option<String>,         // a top-level fn this source provides
        owner: String,                   // owner stamped on its callable (proxy for "origin")
        typed: Option<String>,           // an internal name this source has a shape for
    }

    fn callable(owner: &str, name: &str) -> LibraryCallable {
        LibraryCallable {
            owner: owner.to_string(),
            name: name.to_string(),
            params: vec![],
            ret: Ty::Unit,
            physical_ret: Ty::Unit,
            descriptor: "()V".to_string(),
            is_inline: false,
            default_call: false,
            vararg_elem: None,
            must_inline: false,
            signature: None,
            origin: crate::libraries::Origin::Library,
        }
    }

    impl SymbolSource for FakeSource {
        fn seed(&self) -> LibrarySeed {
            let mut s = LibrarySeed::default();
            if let Some((k, v)) = &self.class {
                s.class_names.insert(k.clone(), v.clone());
            }
            s
        }
        fn functions(&self, name: &str, receiver: Option<Ty>) -> FunctionSet {
            if receiver.is_none() && self.fn_name.as_deref() == Some(name) {
                FunctionSet {
                    overloads: vec![FunctionInfo {
                        kind: FnKind::TopLevel,
                        receiver: None,
                        ret_nullable: false,
                        public: true,
                        receiver_rank: 0,
                        call_sig: crate::libraries::CallSig::default(),
                        flags: FnFlags::default(),
                        callable: callable(&self.owner, name),
                    }],
                }
            } else {
                FunctionSet::default()
            }
        }
        fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
            if self.typed.as_deref() == Some(internal) {
                Some(LibraryType {
                    is_public: true,
                    is_interface: false,
                    is_annotation: false,
                    supertypes: vec![self.owner.clone()],
                    constructors: vec![],
                    members: vec![],
                    companion: vec![],
                })
            } else {
                None
            }
        }
    }

    fn module() -> FakeSource {
        FakeSource {
            class: Some(("Foo".into(), "mod/Foo".into())),
            fn_name: Some("greet".into()),
            owner: "module".into(),
            typed: Some("shared".into()),
        }
    }

    fn library() -> FakeSource {
        FakeSource {
            class: Some(("Foo".into(), "lib/Foo".into())), // clashes with module on `Foo`
            fn_name: Some("greet".into()),                 // clashes with module on `greet`
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
            class: None,
            fn_name: None,
            owner: "library".into(),
            typed: Some("lib/only".into()),
        };
        let c = CompositeSource::new(vec![Box::new(module()), Box::new(lib)]);
        assert!(c.resolve_type("lib/only").is_some());
        assert!(c.resolve_type("nope").is_none());
    }

    #[test]
    fn seed_merges_with_earliest_winning_on_clash() {
        let c = CompositeSource::new(vec![Box::new(module()), Box::new(library())]);
        let seed = c.seed();
        // `Foo` is defined by both; the module (earliest/highest precedence) wins.
        assert_eq!(seed.class_names.get("Foo"), Some(&"mod/Foo".to_string()));
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
}
