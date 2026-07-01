//! `SymbolSource` — the federatable seam shared by every provider of declarations.
//!
//! A *source* answers the three arg-independent questions resolution needs about a body of code: the
//! type universe it contributes (`seed`), the overloads of a name (`functions`), and the shape of a
//! type (`resolve_type`). Both the current module (its AST decls) and a compiled library (a classpath)
//! are sources; [`crate::libraries::SymbolSource`] is a `SymbolSource` plus the JVM-emit extras.
//!
//! Sources COMPOSE: a [`CompositeSource`] holds an ordered list of children and is itself a
//! `SymbolSource`, so `[current module, sibling modules, stdlib, extra jars]` federate uniformly with
//! first-source-wins precedence (user code shadows libraries). Selection of a single overload stays
//! INSIDE one source (an extension's receiver-MRO rank is only comparable within one type hierarchy);
//! the composite federates at the resolve boundary, never by flattening one global overload set.

use crate::libraries::{FunctionSet, LibraryMember, LibrarySeed, LibraryType};
use crate::types::Ty;

/// The shared-base form of a [`LibrarySeed`]: class names, type aliases, and canonical-name aliases,
/// each behind an `Rc` so many files compiled against one provider share the large maps rather than
/// cloning them.
pub type SharedSeed = (
    std::rc::Rc<std::collections::HashMap<String, String>>,
    std::rc::Rc<std::collections::HashMap<String, String>>,
    std::rc::Rc<std::collections::HashMap<String, String>>,
);

/// A provider of declarations — a module's AST or a compiled library. The arg-independent metadata
/// surface that federates across sources; arg-dependent selection/binding lives above (the resolver).
pub trait SymbolSource {
    /// The type universe this source contributes, resolved to internal names (simple name → internal,
    /// plus type aliases). Empty by default.
    fn seed(&self) -> LibrarySeed {
        LibrarySeed::default()
    }

    /// The same type universe as [`seed`], but with the (large) base maps shared by `Rc` so a caller
    /// that compiles many files against one provider does NOT clone the whole stdlib+JDK class-name
    /// map per file. Returns `(class_names, type_aliases)`. The default just wraps `seed`; a heavy
    /// classpath-backed source (the JVM one) overrides this to cache and return a shared `Rc`.
    fn seed_shared(&self) -> SharedSeed {
        let s = self.seed();
        (
            std::rc::Rc::new(s.class_names),
            std::rc::Rc::new(s.type_aliases),
            std::rc::Rc::new(s.canonical_names),
        )
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

    /// The value-class underlying type for a semantic type, when this source knows it. The default
    /// handles ordinary reference-named value classes through `resolve_type`; platform providers can
    /// add builtins whose source type is not represented as `Ty::Obj`.
    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        match ty {
            Ty::Obj(internal, _) => self.resolve_type(internal).and_then(|t| t.value_underlying),
            _ => None,
        }
    }

    /// Whether this source recognizes `ty` as one of Kotlin's unsigned integer library types. The checker
    /// uses this for the source-level unsigned arithmetic/equality rules without carrying a local list of
    /// `UInt`/`ULong` in resolver code.
    fn is_unsigned_integer_type(&self, _ty: Ty) -> bool {
        false
    }

    /// If values of this type can be invoked like a Kotlin function, return their arity. Plain
    /// `Ty::Fun` is handled here; platform providers can add callable runtime types such as property
    /// references without the checker knowing their class names.
    fn function_like_arity(&self, ty: Ty) -> Option<usize> {
        ty.fun_arity().map(usize::from)
    }

    /// The platform/library type used for a property reference with the given arity and mutability.
    /// Resolver needs this type so direct property-reference APIs (`get`, `name`) keep working, but the
    /// actual class name is provider-owned.
    fn property_reference_type(&self, _arity: usize, _mutable: bool) -> Option<Ty> {
        None
    }

    /// The type produced by a class literal (`X::class`) on this target/platform.
    fn class_literal_type(&self) -> Option<Ty> {
        None
    }

    /// Additional default wildcard-import packages contributed by this platform, in dotted Kotlin
    /// package syntax. Common Kotlin defaults live in the resolver; this hook is only for documented
    /// target additions such as JVM's `java.lang` and `kotlin.jvm`.
    fn platform_default_import_packages(&self) -> &'static [&'static str] {
        &[]
    }

    /// Platform spelling for a physical zero-arg getter when Kotlin property metadata is unavailable.
    /// Common resolution asks for a semantic property name first; this hook is a fallback owned by the
    /// source because JVM uses JavaBean-style `getX`/`isX` while other targets need not.
    fn physical_property_getter_name(&self, _property: &str) -> Option<String> {
        None
    }

    /// SOURCE value-parameter names of the constructor of `internal` taking `arity` parameters, when this
    /// source records them (from `@Metadata`) — for mapping NAMED constructor arguments onto positions.
    /// Descriptors don't carry parameter names, so this is the only source for a classpath constructor.
    fn constructor_param_names(&self, _internal: &str, _arity: usize) -> Option<Vec<String>> {
        None
    }

    /// The primary constructor's SOURCE parameter names PLUS a per-parameter "declares a default value"
    /// flag, for a constructor whose parameter count is at least `min_arity` — so a NAMED call may OMIT a
    /// defaulted parameter (`Cfg(a = 1, c = "x")` for `Cfg(a, b = 9, c = "z")`). Unlike
    /// [`Self::constructor_param_names`] (exact arity), this returns the FULL parameter list; the omitted
    /// slots lower to kotlinc's `<init>$default` synthetic. `None` when no such constructor is recorded.
    fn constructor_named_params(
        &self,
        _internal: &str,
        _min_arity: usize,
    ) -> Option<(Vec<String>, Vec<bool>)> {
        None
    }

    /// Whether the classpath `@JvmInline value class` named `internal` exposes a DEFAULTED primary
    /// constructor — kotlinc emits a `constructor-impl$default` synthetic exactly then. A zero-arg
    /// construction `Id()` (all params defaulted) is realized through that synthetic; `false` when the
    /// value class's sole underlying param is mandatory, so `Id()` stays unresolved rather than miscompiled.
    fn value_class_ctor_has_default(&self, _internal: &str) -> bool {
        false
    }

    /// Whether the classpath type `internal` declares an enum entry named `name` — a `static final`
    /// field of the enum's own type (`Kind.PENDING` → `getstatic lib/Kind.PENDING:Llib/Kind;`). Lets
    /// `EnumName.ENTRY` resolve for a classpath enum, as it already does for a source enum.
    fn is_enum_entry(&self, _internal: &str, _name: &str) -> bool {
        false
    }

    /// A property of classpath type `internal` whose declared type is a `@JvmInline value class`
    /// (`Holder(val id: Vid)`): its getter is `@JvmName`-mangled (`getId-<hash>`) and its physical
    /// return erases to the value class's underlying, so ordinary getter resolution misses it. Returns a
    /// member carrying the MANGLED getter name + physical descriptor but the LOGICAL value-class return
    /// type (recovered from `@Metadata`), so `h.id` types as the value class and `h.id.v` resolves.
    fn value_class_property_member(
        &self,
        _internal: &str,
        _property: &str,
    ) -> Option<LibraryMember> {
        None
    }

    /// The type arguments of `internal` INFERRED from a constructor call's argument types — `Pair(1, 2)` →
    /// `[Int, Int]`, so `Pair(1, 2)` types as `Pair<Int, Int>` (its `first`/`second`/`component*` then type
    /// concretely). Each formal type parameter is bound by unifying the constructor's generic parameter
    /// signatures with `arg_tys`; an unbound formal defaults to `Any`. `None` if `internal` is non-generic
    /// or this source can't infer.
    fn infer_constructor_type_args(&self, _internal: &str, _arg_tys: &[Ty]) -> Option<Vec<Ty>> {
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
            seed.canonical_names.extend(s.canonical_names);
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

    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        self.children.iter().find_map(|c| c.value_underlying(ty))
    }

    fn is_unsigned_integer_type(&self, ty: Ty) -> bool {
        self.children.iter().any(|c| c.is_unsigned_integer_type(ty))
    }

    fn function_like_arity(&self, ty: Ty) -> Option<usize> {
        self.children.iter().find_map(|c| c.function_like_arity(ty))
    }

    fn property_reference_type(&self, arity: usize, mutable: bool) -> Option<Ty> {
        self.children
            .iter()
            .find_map(|c| c.property_reference_type(arity, mutable))
    }

    fn class_literal_type(&self) -> Option<Ty> {
        self.children.iter().find_map(|c| c.class_literal_type())
    }

    fn platform_default_import_packages(&self) -> &'static [&'static str] {
        self.children
            .iter()
            .find_map(|c| {
                let imports = c.platform_default_import_packages();
                (!imports.is_empty()).then_some(imports)
            })
            .unwrap_or(&[])
    }

    fn physical_property_getter_name(&self, property: &str) -> Option<String> {
        self.children
            .iter()
            .find_map(|c| c.physical_property_getter_name(property))
    }

    fn constructor_param_names(&self, internal: &str, arity: usize) -> Option<Vec<String>> {
        self.children
            .iter()
            .find_map(|c| c.constructor_param_names(internal, arity))
    }

    fn infer_constructor_type_args(&self, internal: &str, arg_tys: &[Ty]) -> Option<Vec<Ty>> {
        self.children
            .iter()
            .find_map(|c| c.infer_constructor_type_args(internal, arg_tys))
    }

    fn value_class_ctor_has_default(&self, internal: &str) -> bool {
        self.children
            .iter()
            .any(|c| c.value_class_ctor_has_default(internal))
    }

    fn is_enum_entry(&self, internal: &str, name: &str) -> bool {
        self.children
            .iter()
            .any(|c| c.is_enum_entry(internal, name))
    }

    fn value_class_property_member(&self, internal: &str, property: &str) -> Option<LibraryMember> {
        self.children
            .iter()
            .find_map(|c| c.value_class_property_member(internal, property))
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
            inline: crate::libraries::InlineKind::None,
            default_call: false,
            vararg_elem: None,
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
                        ret_class: None,
                        public: true,
                        receiver_rank: 0,
                        overload_rank: 0,
                        generic_sig: None,
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
