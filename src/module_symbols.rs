//! `ModuleSymbols` — the current compilation's own declarations exposed as a [`SymbolSource`].
//!
//! It wraps the user-declared half of a [`SymbolTable`] (top-level functions, classes, extensions) and
//! answers the same `seed`/`functions`/`resolve_type` queries a compiled library does — so module code
//! federates with libraries through one [`crate::symbol_source::CompositeSource`] instead of the
//! scattered "user-first, else library" branching. Descriptors are synthesized from the declared `Ty`s
//! (`Ty::descriptor`), since the module isn't emitted yet; every callable is stamped [`Origin::Module`]
//! so the lowerer can pick the same-file / cross-file / library emit form from resolution alone.

use crate::libraries::{
    CallSig, FnFlags, FnKind, FunctionInfo, FunctionSet, LibraryCallable, LibraryMember,
    LibrarySeed, LibraryType, Origin,
};
use crate::resolve::{ClassSig, Signature, SymbolTable};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// The current module's declarations as a [`SymbolSource`]. Borrows the [`SymbolTable`]; cheap.
pub struct ModuleSymbols<'a> {
    syms: &'a SymbolTable,
}

impl<'a> ModuleSymbols<'a> {
    pub fn new(syms: &'a SymbolTable) -> Self {
        ModuleSymbols { syms }
    }

    /// The declaring facade of a top-level `name`, if the multi-file driver recorded one. `None` means
    /// "the file being compiled" — the lowerer then resolves it as a same-file local.
    fn facade_of(&self, name: &str) -> Option<String> {
        self.syms.fn_facades.get(name).cloned()
    }

    /// The user [`ClassSig`] whose JVM internal name is `internal`, if any.
    fn class_by_internal(&self, internal: &str) -> Option<&'a ClassSig> {
        self.syms.classes.values().find(|c| c.internal == internal)
    }

    /// Whether the module declares a top-level function named `name` — the shadow-precedence test (a
    /// user function hides a library/builtin of the same name). Cheap existence query over the source.
    pub fn declares_top_level(&self, name: &str) -> bool {
        self.syms.funs.contains_key(name)
    }

    /// Select the top-level overload of `name` matching `arg_tys` (Kotlin overload resolution via
    /// [`crate::resolve::pick_overload`]) and return it as a [`FunctionInfo`]. The source owns the
    /// selection, so callers need not touch `syms.funs` or re-run the picker themselves.
    pub fn resolve_top_level(&self, name: &str, arg_tys: &[Ty]) -> Option<FunctionInfo> {
        let i = crate::resolve::pick_overload(self.syms.funs.get(name)?, arg_tys)?;
        self.functions(name, None).overloads.into_iter().nth(i)
    }

    /// Collect members named `name` over the user hierarchy in DEPTH-FIRST pre-order (self, then each
    /// interface subtree, then the superclass subtree) — the exact order `Checker::lookup_method` uses,
    /// so the first collected overload is the one that lookup would return. `rung` is the visit counter.
    fn collect_members(
        &self,
        internal: &str,
        name: &str,
        out: &mut Vec<FunctionInfo>,
        seen: &mut std::collections::HashSet<String>,
        rung: &mut u32,
    ) {
        if !seen.insert(internal.to_string()) {
            return;
        }
        let Some(c) = self.class_by_internal(internal) else {
            return;
        };
        let here = *rung;
        *rung += 1;
        if let Some(sig) = c.methods.get(name) {
            out.push(fn_info(
                FnKind::Member,
                sig,
                None,
                c.internal.clone(),
                name,
                here,
                Origin::Module {
                    facade: c.internal.clone(),
                },
            ));
        }
        for i in &c.interfaces {
            self.collect_members(i, name, out, seen, rung);
        }
        if let Some(s) = &c.super_internal {
            self.collect_members(s, name, out, seen, rung);
        }
    }
}

/// A JVM method descriptor `(params)ret` synthesized from declared `Ty`s.
fn descriptor(params: &[Ty], ret: Ty) -> String {
    let mut s = String::from("(");
    for p in params {
        s.push_str(&p.descriptor());
    }
    s.push(')');
    s.push_str(&ret.descriptor());
    s
}

/// Build a top-level / extension `FunctionInfo` from a user [`Signature`]. `receiver` is `Some` for an
/// extension (prepended to `params`, matching the library convention that `params[0]` is the receiver).
fn fn_info(
    kind: FnKind,
    sig: &Signature,
    receiver: Option<Ty>,
    owner: String,
    name: &str,
    rank: u32,
    origin: Origin,
) -> FunctionInfo {
    let mut params: Vec<Ty> = Vec::new();
    if let Some(r) = receiver {
        params.push(r);
    }
    params.extend(sig.params.iter().copied());
    FunctionInfo {
        kind,
        receiver,
        ret_nullable: false,
        public: true,
        receiver_rank: rank,
        // The call shape mirrors the source Signature, parallel to the LOGICAL params (no receiver).
        call_sig: CallSig {
            param_names: sig.param_names.clone(),
            param_defaults: sig.param_defaults.clone(),
            lambda_param_types: sig.lambda_param_types.clone(),
            required: sig.required,
            vararg: sig.vararg,
        },
        flags: FnFlags {
            inline: sig.is_inline,
            inline_only: false,
            // Same-file `suspend fun` — flows from the AST via `Signature.is_suspend` so the resolver
            // reports suspend-ness uniformly with classpath callees (whose flag comes from @Metadata).
            suspend: sig.is_suspend,
        },
        callable: LibraryCallable {
            owner,
            name: name.to_string(),
            descriptor: descriptor(&params, sig.ret),
            params,
            ret: sig.ret,
            physical_ret: sig.ret,
            is_inline: sig.is_inline,
            default_call: false,
            vararg_elem: None,
            must_inline: false,
            signature: None,
            origin,
        },
    }
}

impl SymbolSource for ModuleSymbols<'_> {
    fn seed(&self) -> LibrarySeed {
        // The module's declared type names → their internal names. `class_names` already holds the
        // resolved internal for every user class/object/enum (mixed with library names); pick out the
        // ones this module actually declares.
        let mut class_names = std::collections::HashMap::new();
        let declared = self
            .syms
            .classes
            .keys()
            .chain(self.syms.objects.iter())
            .chain(self.syms.enums.keys());
        for name in declared {
            if let Some(internal) = self.syms.class_names.get(name) {
                class_names.insert(name.clone(), internal.clone());
            }
        }
        LibrarySeed {
            class_names,
            type_aliases: std::collections::HashMap::new(),
        }
    }

    fn functions(&self, name: &str, receiver: Option<Ty>) -> FunctionSet {
        let mut overloads = Vec::new();
        match receiver {
            None => {
                // Top-level functions: every overload of `name`.
                if let Some(sigs) = self.syms.funs.get(name) {
                    let owner = self.facade_of(name).unwrap_or_default();
                    let origin = Origin::Module {
                        facade: owner.clone(),
                    };
                    for sig in sigs {
                        overloads.push(fn_info(
                            FnKind::TopLevel,
                            sig,
                            None,
                            owner.clone(),
                            name,
                            0,
                            origin.clone(),
                        ));
                    }
                }
            }
            Some(recv) => {
                // Instance members of the receiver's user type (own + inherited), in DEPTH-FIRST
                // pre-order (self → interfaces → super) — exactly the checker's `lookup_method` walk, so
                // `overloads[0]` is the same member that hand-rolled lookup picks. Each carries its visit
                // rung in `receiver_rank`.
                if let Ty::Obj(internal, _) = recv {
                    let mut seen = std::collections::HashSet::new();
                    let mut rung: u32 = 0;
                    self.collect_members(internal, name, &mut overloads, &mut seen, &mut rung);
                }
                // Extension functions, keyed by erased receiver descriptor: the exact receiver (rung 0),
                // then the generic `Any`/`Object` key (rung 1) for a type-variable-receiver extension —
                // matching the checker's exact-then-generic extension lookup.
                let exact = recv.descriptor();
                let any = Ty::obj("kotlin/Any").descriptor();
                for (rank, key) in [exact, any].into_iter().enumerate() {
                    if let Some(sig) = self.syms.ext_funs.get(&(key.clone(), name.to_string())) {
                        overloads.push(fn_info(
                            FnKind::Extension,
                            sig,
                            Some(recv),
                            String::new(),
                            name,
                            rank as u32,
                            Origin::Module {
                                facade: String::new(),
                            },
                        ));
                    }
                }
            }
        }
        FunctionSet { overloads }
    }

    fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
        let c = self.class_by_internal(internal)?;
        let member = |name: &str, sig: &Signature| LibraryMember {
            name: name.to_string(),
            params: sig.params.clone(),
            ret: sig.ret,
            descriptor: descriptor(&sig.params, sig.ret),
        };
        let members = c.methods.iter().map(|(n, s)| member(n, s)).collect();
        let companion = c.static_methods.iter().map(|(n, s)| member(n, s)).collect();
        // The primary constructor (+ secondaries) as `<init>` members returning Unit.
        let mut constructors = vec![LibraryMember {
            name: "<init>".to_string(),
            params: c.ctor_params.clone(),
            ret: Ty::Unit,
            descriptor: descriptor(&c.ctor_params, Ty::Unit),
        }];
        for params in &c.secondary_ctors {
            constructors.push(LibraryMember {
                name: "<init>".to_string(),
                params: params.clone(),
                ret: Ty::Unit,
                descriptor: descriptor(params, Ty::Unit),
            });
        }
        let mut supertypes = c.interfaces.clone();
        if let Some(s) = &c.super_internal {
            supertypes.push(s.clone());
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
        Some(LibraryType {
            is_public: true,
            kind,
            supertypes,
            constructors,
            members,
            companion,
        })
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
            param_names: vec![],
            lambda_param_types: vec![],
            is_inline: false,
            is_final: false,
            is_suspend: false,
        }
    }

    fn class(internal: &str) -> ClassSig {
        ClassSig {
            internal: internal.to_string(),
            props: vec![],
            ctor_params: vec![],
            methods: HashMap::new(),
            is_interface: false,
            is_sealed: false,
            inner_of: None,
            static_methods: HashMap::new(),
            static_props: HashMap::new(),
            lateinit_props: HashSet::new(),
            interfaces: vec![],
            super_internal: None,
            is_annotation: false,
            ctor_defaults: vec![],
            secondary_ctors: vec![],
            tparam_names: vec![],
            generic_props: HashMap::new(),
            value_field: None,
        }
    }

    #[test]
    fn top_level_functions_are_module_origin_with_synth_descriptor() {
        let mut st = SymbolTable::default();
        st.funs
            .insert("twice".into(), vec![sig(vec![Ty::Int], Ty::Int)]);
        let m = ModuleSymbols::new(&st);
        let fs = m.functions("twice", None);
        assert_eq!(fs.overloads.len(), 1);
        let o = &fs.overloads[0];
        assert_eq!(o.kind, FnKind::TopLevel);
        assert_eq!(o.callable.descriptor, "(I)I");
        assert_eq!(o.callable.origin, Origin::Module { facade: "".into() });
    }

    #[test]
    fn call_sig_mirrors_the_source_signature() {
        let mut st = SymbolTable::default();
        let mut s = sig(vec![Ty::Int, Ty::Int], Ty::Int);
        s.required = 1;
        s.param_defaults = vec![false, true];
        s.param_names = vec!["a".into(), "b".into()];
        s.vararg = false;
        st.funs.insert("f".into(), vec![s]);
        let m = ModuleSymbols::new(&st);
        let cs = &m.functions("f", None).overloads[0].call_sig;
        assert_eq!(cs.required, 1);
        assert_eq!(cs.param_defaults, vec![false, true]);
        assert_eq!(cs.param_names, vec!["a".to_string(), "b".to_string()]);
        assert!(!cs.vararg);
    }

    #[test]
    fn top_level_overloads_all_returned() {
        let mut st = SymbolTable::default();
        st.funs.insert(
            "f".into(),
            vec![sig(vec![Ty::Int], Ty::Int), sig(vec![Ty::String], Ty::Int)],
        );
        let m = ModuleSymbols::new(&st);
        assert_eq!(m.functions("f", None).overloads.len(), 2);
    }

    #[test]
    fn cross_file_facade_flows_into_origin() {
        let mut st = SymbolTable::default();
        st.funs.insert("helper".into(), vec![sig(vec![], Ty::Unit)]);
        st.fn_facades.insert("helper".into(), "pkg/AKt".into());
        let m = ModuleSymbols::new(&st);
        let o = &m.functions("helper", None).overloads[0];
        assert_eq!(o.callable.owner, "pkg/AKt");
        assert_eq!(
            o.callable.origin,
            Origin::Module {
                facade: "pkg/AKt".into()
            }
        );
    }

    #[test]
    fn members_walk_user_hierarchy_depth_first_with_rank() {
        let mut st = SymbolTable::default();
        let mut base = class("demo/Base");
        base.methods.insert("greet".into(), sig(vec![], Ty::String));
        let mut sub = class("demo/Sub");
        sub.super_internal = Some("demo/Base".into());
        sub.methods.insert("own".into(), sig(vec![], Ty::Int));
        st.classes.insert("Base".into(), base);
        st.classes.insert("Sub".into(), sub);
        let m = ModuleSymbols::new(&st);

        // `own` is on Sub itself (rung 0).
        let own = m.functions("own", Some(Ty::obj("demo/Sub")));
        assert_eq!(own.overloads.len(), 1);
        assert_eq!(own.overloads[0].kind, FnKind::Member);
        assert_eq!(own.overloads[0].receiver_rank, 0);

        // `greet` is inherited from Base (rung 1).
        let greet = m.functions("greet", Some(Ty::obj("demo/Sub")));
        assert_eq!(greet.overloads.len(), 1);
        assert_eq!(greet.overloads[0].receiver_rank, 1);
        assert_eq!(greet.overloads[0].callable.owner, "demo/Base");
    }

    #[test]
    fn extension_prepends_receiver_and_keys_by_descriptor() {
        let mut st = SymbolTable::default();
        let recv = Ty::obj("demo/Point");
        st.ext_funs.insert(
            (recv.descriptor(), "shifted".into()),
            sig(vec![Ty::Int], recv),
        );
        let m = ModuleSymbols::new(&st);
        let fs = m.functions("shifted", Some(recv));
        assert_eq!(fs.overloads.len(), 1);
        let o = &fs.overloads[0];
        assert_eq!(o.kind, FnKind::Extension);
        // receiver prepended → params = [Point, Int]
        assert_eq!(o.callable.params, vec![recv, Ty::Int]);
        assert_eq!(o.receiver_rank, 0);
    }

    #[test]
    fn resolve_type_builds_shape_with_ctor_and_members() {
        let mut st = SymbolTable::default();
        let mut c = class("demo/Point");
        c.ctor_params = vec![Ty::Int, Ty::Int];
        c.methods.insert("sum".into(), sig(vec![], Ty::Int));
        c.interfaces = vec!["demo/Shape".into()];
        st.classes.insert("Point".into(), c);
        let m = ModuleSymbols::new(&st);
        let t = m.resolve_type("demo/Point").expect("shape");
        assert_eq!(t.constructors.len(), 1);
        assert_eq!(t.constructors[0].descriptor, "(II)V");
        assert_eq!(t.members.len(), 1);
        assert_eq!(t.members[0].name, "sum");
        assert_eq!(t.supertypes, vec!["demo/Shape".to_string()]);
        assert!(m.resolve_type("demo/Nope").is_none());
    }

    #[test]
    fn seed_lists_declared_types() {
        let mut st = SymbolTable::default();
        st.classes.insert("Point".into(), class("demo/Point"));
        st.class_names.insert("Point".into(), "demo/Point".into());
        st.class_names
            .insert("String".into(), "java/lang/String".into()); // library, not declared
        let m = ModuleSymbols::new(&st);
        let seed = m.seed();
        assert_eq!(seed.class_names.get("Point"), Some(&"demo/Point".into()));
        assert!(!seed.class_names.contains_key("String"));
    }
}
