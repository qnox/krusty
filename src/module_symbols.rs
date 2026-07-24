//! `ModuleSymbols` — the current compilation's own declarations exposed as a [`SymbolSource`].
//!
//! It wraps the user-declared half of [`crate::frontend::FrontendSymbols`] (top-level functions,
//! classes, extensions) and answers the same `seed`/`functions`/`resolve_type` queries a compiled
//! library does — so module code federates with libraries through one
//! [`crate::symbol_source::CompositeSource`] instead of the
//! scattered "user-first, else library" branching. Every callable is stamped [`Origin::Module`] so the
//! lowerer can pick the same-file / cross-file / library emit form from resolution alone.

use crate::frontend::{pick_overload, FrontendClassSig, FrontendSymbols, Signature};
use crate::libraries::{
    FnFlags, FnKind, FunctionInfo, FunctionSet, InlineKind, LibraryCallable, LibraryMember,
    LibraryType, Origin,
};
use crate::symbol_source::SymbolSource;
use crate::types::{type_name, Ty, TypeName};
use std::collections::HashMap;

/// The current module's declarations as a [`SymbolSource`]. Borrows the frontend symbols; cheap.
pub struct ModuleSymbols<'a> {
    syms: &'a FrontendSymbols,
}

impl<'a> ModuleSymbols<'a> {
    pub fn new(syms: &'a FrontendSymbols) -> Self {
        ModuleSymbols { syms }
    }

    /// The declaring facade of a top-level `name`, if the multi-file driver recorded one. `None` means
    /// "the file being compiled" — the lowerer then resolves it as a same-file local.
    fn facade_of(&self, name: &str) -> Option<TypeName> {
        self.syms.fn_facades.get(name).copied()
    }

    fn facade_of_sig(&self, name: &str, sig: &Signature) -> TypeName {
        sig.source_file
            .zip(sig.source_decl)
            .and_then(|(file, decl)| self.syms.fn_facades_by_decl.get(&(file, decl.0)).copied())
            .or_else(|| self.facade_of(name))
            .unwrap_or_else(|| type_name(""))
    }

    /// The user [`FrontendClassSig`] whose JVM internal name is `internal`, if any.
    fn class_by_internal(&self, internal: &str) -> Option<&'a FrontendClassSig> {
        self.syms.class_by_internal(internal)
    }

    fn class_by_type_name(&self, internal: TypeName) -> Option<&'a FrontendClassSig> {
        self.syms.class_by_type_name(internal)
    }

    fn type_shape_for(&self, c: &'a FrontendClassSig) -> LibraryType {
        let members = c
            .methods
            .iter()
            .flat_map(|(n, sigs)| {
                sigs.iter()
                    .map(move |s| lib_member(n, s, c.internal_name(), c.is_interface))
            })
            .collect();
        let companion = c
            .static_methods
            .iter()
            .map(|(n, s)| lib_member(n, s, c.internal_name(), c.is_interface))
            .collect();
        // The primary constructor (+ secondaries) as `<init>` members returning Unit.
        let mut constructors = vec![LibraryMember::new(
            "<init>".to_string(),
            c.ctor_params.clone(),
            Ty::Unit,
            String::new(),
        )];
        for params in &c.secondary_ctors {
            constructors.push(LibraryMember::new(
                "<init>".to_string(),
                params.clone(),
                Ty::Unit,
                String::new(),
            ));
        }
        let mut supertypes: Vec<TypeName> = c.interfaces.iter_ids().collect();
        if let Some(s) = c.super_internal {
            supertypes.push(s);
        }
        // Module objects resolve as values via the existing user-object path (StaticInstance), not the
        // classpath ExternalStaticField path, so this needn't distinguish `Object`.
        let kind = if c.is_annotation {
            crate::libraries::TypeKind::Annotation
        } else if c.is_interface {
            crate::libraries::TypeKind::Interface
        } else {
            crate::libraries::TypeKind::Class
        };
        LibraryType {
            is_public: true,
            kind,
            supertypes: supertypes.into(),
            constructors,
            members,
            companion,
            companion_consts: HashMap::new(),
            sam_method: None,
            // In-module classes resolve a bare-companion reference via their own companion path
            // (`companion_class`/`companion_methods`); the classpath fallback isn't used for them.
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
        }
    }

    /// Whether the module declares a top-level function named `name` — the shadow-precedence test (a
    /// user function hides a library/builtin of the same name). Cheap existence query over the source.
    pub fn declares_top_level(&self, name: &str) -> bool {
        self.syms.funs.contains_key(name)
    }

    /// Instance members named `name` on `rt`, collected over the MODULE (user-declared) hierarchy only —
    /// DFS self → interfaces → super, stopping at a classpath supertype (which the module source does not
    /// own). This is the module analog the checker uses where a user-declared method must be found but an
    /// INHERITED classpath member must fall through to the classpath resolver (which records the call for
    /// emit). Federating the classpath here would arity-bind a Java member (`Iterable.forEach(Consumer)`)
    /// over the Kotlin extension, or bind an inherited classpath member the lowerer can't emit.
    pub fn instance_members(&self, rt: Ty, name: &str) -> Vec<LibraryMember> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if let Some(i) = rt.non_null().obj_internal() {
            self.collect_member_libs(i, name, &mut out, &mut seen);
        }
        out
    }

    fn collect_member_libs(
        &self,
        internal: TypeName,
        name: &str,
        out: &mut Vec<LibraryMember>,
        seen: &mut std::collections::HashSet<TypeName>,
    ) {
        if !seen.insert(internal) {
            return;
        }
        let Some(c) = self.class_by_type_name(internal) else {
            return; // a classpath supertype — not owned by the module source
        };
        for sig in c.methods_named(name) {
            out.push(lib_member(name, sig, c.internal_name(), c.is_interface));
        }
        for i in c.interfaces.iter_ids() {
            self.collect_member_libs(i, name, out, seen);
        }
        if let Some(s) = c.super_internal {
            self.collect_member_libs(s, name, out, seen);
        }
    }

    /// Select the top-level overload of `name` matching `arg_tys` and return it as a [`FunctionInfo`].
    /// The source owns the selection, so callers need not touch `syms.funs` or re-run the picker
    /// themselves.
    pub fn resolve_top_level(&self, name: &str, arg_tys: &[Ty]) -> Option<FunctionInfo> {
        let i = pick_overload(self.syms.funs.get(name)?, arg_tys)?;
        self.top_level_overloads(name).into_iter().nth(i)
    }

    pub fn resolve_top_level_in_scope(
        &self,
        name: &str,
        arg_tys: &[Ty],
        packages: &[TypeName],
    ) -> Option<FunctionInfo> {
        let overloads = self.top_level_overloads_in_scope(name, packages);
        let params = overloads
            .iter()
            .map(|fi| crate::frontend::Signature {
                params: fi.callable.params.clone(),
                ret: fi.callable.ret,
                vararg: fi.call_sig.vararg,
                required: fi.call_sig.required,
                param_defaults: fi.call_sig.param_defaults.clone(),
                param_default_values: Vec::new(),
                param_names: fi.call_sig.param_names.clone(),
                lambda_param_types: fi.call_sig.lambda_param_types.clone(),
                lambda_recv: Vec::new(),
                is_inline: fi.flags.inline.can_inline(),
                is_final: true,
                is_suspend: fi.flags.suspend,
                context_count: fi.context_count,
                source_decl: None,
                source_file: None,
                package: String::new(),
            })
            .collect::<Vec<_>>();
        let i = pick_overload(&params, arg_tys)?;
        overloads.into_iter().nth(i)
    }

    /// The module's TOP-LEVEL function overloads of `name` as [`FunctionInfo`]s — every `fun name(...)`
    /// declared at file scope, each stamped with its declaring facade [`Origin::Module`]. The building
    /// block `resolve_symbols`/`resolve_top_level` share, so the source answers a name without the old
    /// receiver-indexed `functions()` API.
    pub fn top_level_overloads(&self, name: &str) -> Vec<FunctionInfo> {
        let mut overloads = Vec::new();
        if let Some(sigs) = self.syms.funs.get(name) {
            for sig in sigs {
                let owner = self.facade_of_sig(name, sig);
                let origin = Origin::Module { facade: owner };
                overloads.push(fn_info(
                    FnKind::TopLevel,
                    sig,
                    None,
                    owner,
                    name,
                    0,
                    origin.clone(),
                ));
            }
        }
        overloads
    }

    pub fn top_level_overloads_in_scope(
        &self,
        name: &str,
        packages: &[TypeName],
    ) -> Vec<FunctionInfo> {
        self.top_level_overloads(name)
            .into_iter()
            .filter(|fi| {
                fi.source_key
                    .and_then(|(file, decl)| {
                        self.syms.funs.get(name).and_then(|sigs| {
                            sigs.iter().find(|sig| {
                                sig.source_file == Some(file)
                                    && sig.source_decl.is_some_and(|d| d.0 == decl)
                            })
                        })
                    })
                    .is_some_and(|sig| packages.iter().any(|pkg| pkg.matches(&sig.package)))
            })
            .collect()
    }

    /// Collect members named `name` over the user hierarchy in DEPTH-FIRST pre-order (self, then each
    /// interface subtree, then the superclass subtree) — the exact order `Checker::lookup_method` uses,
    /// so the first collected overload is the one that lookup would return. `rung` is the visit counter.
    fn collect_members(
        &self,
        internal: TypeName,
        name: &str,
        out: &mut Vec<FunctionInfo>,
        seen: &mut std::collections::HashSet<TypeName>,
        rung: &mut u32,
    ) {
        if !seen.insert(internal) {
            return;
        }
        let Some(c) = self.class_by_type_name(internal) else {
            return;
        };
        let here = *rung;
        *rung += 1;
        for sig in c.methods_named(name) {
            out.push(fn_info(
                FnKind::Member,
                sig,
                None,
                c.internal_name(),
                name,
                here,
                Origin::Module {
                    facade: c.internal_name(),
                },
            ));
        }
        for i in c.interfaces.iter_ids() {
            self.collect_members(i, name, out, seen, rung);
        }
        if let Some(s) = c.super_internal {
            self.collect_members(s, name, out, seen, rung);
        }
    }
}

/// A user [`Signature`] as a [`LibraryMember`] — the module-source shape of a class method. Carries the
/// source call-shape (`call_sig`) so a named / omitted-default member call resolves through the type
/// interface.
fn lib_member(name: &str, sig: &Signature, owner: TypeName, is_interface: bool) -> LibraryMember {
    let mut m = LibraryMember::new(name.to_string(), sig.params.clone(), sig.ret, String::new());
    m.owner = Some(owner);
    m.is_interface = is_interface;
    m.suspend = sig.is_suspend;
    m.inline = crate::libraries::InlineKind::from_flags(sig.is_inline, false);
    m.call_sig = sig.call_sig();
    m
}

/// Build a top-level / extension `FunctionInfo` from a user [`Signature`]. `receiver` is `Some` for an
/// extension (prepended to `params`, matching the library convention that `params[0]` is the receiver).
fn fn_info(
    kind: FnKind,
    sig: &Signature,
    receiver: Option<Ty>,
    owner: TypeName,
    name: &str,
    rank: u32,
    origin: Origin,
) -> FunctionInfo {
    let mut params: Vec<Ty> = Vec::new();
    if let Some(r) = receiver {
        params.push(r);
    }
    params.extend(sig.params.iter().copied());
    let callable = LibraryCallable {
        owner,
        name: name.to_string(),
        descriptor: String::new(),
        params,
        ret: sig.ret,
        physical_ret: sig.ret,
        suspend: sig.is_suspend,
        inline: InlineKind::from_flags(sig.is_inline, false),
        default_call: false,
        vararg_elem: None,
        signature: None,
        origin,
        // A module extension's declared receiver, verbatim; the value-class pass filters by value-class
        // identity, so a generic or non-value-class receiver is inert here.
        source_receiver: receiver,
    };
    FunctionInfo {
        receiver_rank: rank,
        call_sig: sig.call_sig(),
        context_count: sig.context_count,
        source_key: sig
            .source_file
            .zip(sig.source_decl)
            .map(|(file, decl)| (file, decl.0)),
        flags: FnFlags {
            inline: InlineKind::from_flags(sig.is_inline, false),
            // Same-file `suspend fun` — flows from the AST via `Signature.is_suspend` so the resolver
            // reports suspend-ness uniformly with classpath callees (whose flag comes from @Metadata).
            suspend: sig.is_suspend,
        },
        ..FunctionInfo::plain(kind, receiver, callable)
    }
}

impl SymbolSource for ModuleSymbols<'_> {
    fn resolve_symbols(&self, fqn: &str) -> crate::libraries::ResolvedSymbols {
        (*self.resolve_symbols_name(type_name(fqn))).clone()
    }

    fn resolve_symbols_name(
        &self,
        fqn: TypeName,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        use crate::libraries::{Callables, ResolvedSymbols};
        // Classifier: a module class at the fqn. Callables: `functions(name, receiver)` — members (always
        // visible on their type) plus the module's top-level/extension functions when the fqn's package is
        // their declaring package (a same-file function has no recorded facade — it lives in the file's own
        // package, which the resolver queries as the same-package candidate fqn).
        let classifier = self.resolve_type_name(fqn);
        let pkg = fqn.parent().unwrap_or_else(|| type_name(""));
        let name = fqn.segment();
        let pkg_scope = [pkg];
        let mut overloads = if self.syms.funs.contains_key(&name) {
            self.top_level_overloads_in_scope(&name, &pkg_scope)
        } else {
            Vec::new()
        };
        // Module EXTENSIONS of `name`, receiver as an ATTRIBUTE (fqn resolution is receiver-agnostic; the
        // resolver's `receiver_extensions` filters by receiver applicability). Keyed by erased receiver in
        // `ext_funs` — the exact-receiver key is rung 0, the universal `Any` key rung 1.
        let any = Ty::obj("kotlin/Any");
        for ((recv, en), sigs) in &self.syms.ext_funs {
            if en == &name {
                let rank = if *recv == any { 1 } else { 0 };
                // Surface EVERY overload registered for this (receiver, name) so the resolver's
                // overload picker can choose by arity/argument types (`fun R.f()` vs `fun R.f(x)`).
                for sig in sigs {
                    overloads.push(fn_info(
                        FnKind::Extension,
                        sig,
                        Some(*recv),
                        crate::types::type_name(""),
                        &name,
                        rank,
                        Origin::Module {
                            facade: type_name(""),
                        },
                    ));
                }
            }
        }
        let callables = if overloads.is_empty() {
            Callables::None
        } else {
            Callables::Functions(FunctionSet { overloads })
        };
        std::rc::Rc::new(ResolvedSymbols {
            classifier,
            callables,
        })
    }

    fn member_overloads(&self, recv: Ty, name: &str) -> FunctionSet {
        // Instance members of the receiver's user type (own + inherited), in DEPTH-FIRST pre-order
        // (self → interfaces → super) — exactly the checker's `lookup_method` walk, so `overloads[0]` is
        // the same member hand-rolled lookup picks. Each carries its visit rung in `receiver_rank`. The
        // module's top-level/extension callables are surfaced by `resolve_symbols`, not here.
        let mut overloads = Vec::new();
        if let Ty::Obj(internal, _) = recv {
            let mut seen = std::collections::HashSet::new();
            let mut rung: u32 = 0;
            self.collect_members(internal, name, &mut overloads, &mut seen, &mut rung);
        }
        FunctionSet { overloads }
    }

    fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
        self.class_by_internal(internal)
            .map(|c| self.type_shape_for(c))
    }

    fn resolve_type_name(&self, internal: TypeName) -> Option<std::rc::Rc<LibraryType>> {
        self.syms
            .class_by_type_name(internal)
            .map(|c| std::rc::Rc::new(self.type_shape_for(c)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn sig(params: Vec<Ty>, ret: Ty) -> Signature {
        Signature {
            params,
            ret,
            vararg: false,
            required: 0,
            param_defaults: vec![],
            param_default_values: vec![],
            param_names: vec![],
            lambda_param_types: vec![],
            lambda_recv: vec![],
            is_inline: false,
            is_final: false,
            is_suspend: false,
            context_count: 0,
            source_decl: None,
            source_file: None,
            package: String::new(),
        }
    }

    fn class(internal: &str) -> FrontendClassSig {
        FrontendClassSig {
            internal: internal.into(),
            props: vec![],
            ctor_params: vec![],
            ctor_param_names: vec![],
            methods: HashMap::new(),
            is_interface: false,
            is_object: false,
            is_abstract: false,
            is_fun_interface: false,
            is_sealed: false,
            inner_of: None,
            static_methods: HashMap::new(),
            companion_fun_names: HashSet::new(),
            static_props: HashMap::new(),
            lateinit_props: HashSet::new(),
            interfaces: crate::types::TypeNameList::new(),
            super_internal: None,
            super_ctor_params: Vec::new(),
            is_annotation: false,
            ctor_defaults: vec![],
            secondary_ctors: vec![],
            tparam_names: vec![],
            tparam_bound_erasures: vec![],
            generic_props: HashMap::new(),
            value_field: None,
            generic_methods: HashMap::new(),
            prop_visibility: std::collections::HashMap::new(),
            fn_visibility: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn top_level_functions_are_module_origin_with_semantic_shape() {
        let mut st = FrontendSymbols::default();
        st.funs
            .insert("twice".into(), vec![sig(vec![Ty::Int], Ty::Int)]);
        let m = ModuleSymbols::new(&st);
        let fs = m.top_level_overloads("twice");
        assert_eq!(fs.len(), 1);
        let o = &fs[0];
        assert_eq!(o.kind, FnKind::TopLevel);
        assert_eq!(o.callable.params, vec![Ty::Int]);
        assert_eq!(o.callable.ret, Ty::Int);
        assert_eq!(
            o.callable.origin,
            Origin::Module {
                facade: type_name("")
            }
        );
    }

    #[test]
    fn call_sig_mirrors_the_source_signature() {
        let mut st = FrontendSymbols::default();
        let mut s = sig(vec![Ty::Int, Ty::Int], Ty::Int);
        s.required = 1;
        s.param_defaults = vec![false, true];
        s.param_names = vec!["a".into(), "b".into()];
        s.vararg = false;
        st.funs.insert("f".into(), vec![s]);
        let m = ModuleSymbols::new(&st);
        let cs = &m.top_level_overloads("f")[0].call_sig;
        assert_eq!(cs.required, 1);
        assert_eq!(cs.param_defaults, vec![false, true]);
        assert_eq!(cs.param_names, vec!["a".to_string(), "b".to_string()]);
        assert!(!cs.vararg);
    }

    #[test]
    fn top_level_overloads_all_returned() {
        let mut st = FrontendSymbols::default();
        st.funs.insert(
            "f".into(),
            vec![sig(vec![Ty::Int], Ty::Int), sig(vec![Ty::String], Ty::Int)],
        );
        let m = ModuleSymbols::new(&st);
        assert_eq!(m.top_level_overloads("f").len(), 2);
    }

    #[test]
    fn cross_file_facade_flows_into_origin() {
        let mut st = FrontendSymbols::default();
        st.funs.insert("helper".into(), vec![sig(vec![], Ty::Unit)]);
        st.fn_facades
            .insert("helper".into(), crate::types::type_name("pkg/AKt"));
        let m = ModuleSymbols::new(&st);
        let o = &m.top_level_overloads("helper")[0];
        assert!(o.callable.owner.matches("pkg/AKt"));
        assert_eq!(
            o.callable.origin,
            Origin::Module {
                facade: type_name("pkg/AKt")
            }
        );
    }

    #[test]
    fn members_walk_user_hierarchy_depth_first_with_rank() {
        let mut st = FrontendSymbols::default();
        let mut base = class("demo/Base");
        base.methods
            .insert("greet".into(), vec![sig(vec![], Ty::String)]);
        let mut sub = class("demo/Sub");
        sub.super_internal = Some(crate::types::type_name("demo/Base"));
        sub.methods.insert("own".into(), vec![sig(vec![], Ty::Int)]);
        st.insert_class("Base".into(), base);
        st.insert_class("Sub".into(), sub);
        let m = ModuleSymbols::new(&st);

        // `own` is on Sub itself (rung 0).
        let own = m.member_overloads(Ty::obj("demo/Sub"), "own");
        assert_eq!(own.overloads.len(), 1);
        assert_eq!(own.overloads[0].kind, FnKind::Member);
        assert_eq!(own.overloads[0].receiver_rank, 0);

        // `greet` is inherited from Base (rung 1).
        let greet = m.member_overloads(Ty::obj("demo/Sub"), "greet");
        assert_eq!(greet.overloads.len(), 1);
        assert_eq!(greet.overloads[0].receiver_rank, 1);
        assert!(greet.overloads[0].callable.owner.matches("demo/Base"));
    }

    #[test]
    fn extension_prepends_receiver_and_keys_by_erased_receiver() {
        let mut st = FrontendSymbols::default();
        let recv = Ty::obj("demo/Point");
        st.ext_funs.insert(
            (recv.erased_recv(), "shifted".into()),
            vec![sig(vec![Ty::Int], recv)],
        );
        let m = ModuleSymbols::new(&st);
        // A module extension is surfaced through `resolve_symbols` by fqn, with the receiver as an attribute.
        let fs = match m.resolve_symbols("shifted").callables {
            crate::libraries::Callables::Functions(f) => f.overloads,
            _ => Vec::new(),
        };
        assert_eq!(fs.len(), 1);
        let o = &fs[0];
        assert_eq!(o.kind, FnKind::Extension);
        // receiver prepended → params = [Point, Int]
        assert_eq!(o.callable.params, vec![recv, Ty::Int]);
        assert_eq!(o.receiver_rank, 0);
    }

    #[test]
    fn resolve_type_builds_shape_with_ctor_and_members() {
        let mut st = FrontendSymbols::default();
        let mut c = class("demo/Point");
        c.ctor_params = vec![Ty::Int, Ty::Int];
        c.methods.insert("sum".into(), vec![sig(vec![], Ty::Int)]);
        c.interfaces = vec![crate::types::type_name("demo/Shape")].into();
        st.insert_class("Point".into(), c);
        let m = ModuleSymbols::new(&st);
        let t = m.resolve_type("demo/Point").expect("shape");
        assert_eq!(t.constructors.len(), 1);
        assert_eq!(t.constructors[0].params, vec![Ty::Int, Ty::Int]);
        assert_eq!(t.members.len(), 1);
        assert_eq!(t.members[0].name, "sum");
        assert_eq!(t.members[0].ret, Ty::Int);
        assert_eq!(t.supertypes.to_vec(), vec!["demo/Shape".to_string()]);
        assert!(m.resolve_type("demo/Nope").is_none());
    }
}
