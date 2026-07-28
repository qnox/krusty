//! `ModuleSymbols` — the current compilation's own declarations exposed as a [`SymbolSource`].
//!
//! It wraps the user-declared half of [`crate::frontend::FrontendSymbols`] (top-level functions,
//! classes, extensions) and answers the same `seed`/`functions`/`resolve_type` queries a compiled
//! library does — so module code federates with libraries through one
//! [`crate::symbol_source::CompositeSource`] instead of the
//! scattered "user-first, else library" branching. Every callable is stamped [`Origin::Module`] so the
//! lowerer can pick the same-file / cross-file / library emit form from resolution alone.

use crate::frontend::{
    pick_overload, FrontendClassSig, FrontendDeclaredPropertySig, FrontendSymbols, SigFlags,
    Signature,
};
use crate::libraries::{
    FnFlags, FnKind, FunctionInfo, FunctionSet, InlineKind, LibraryCallable, LibraryMember,
    LibraryType, Origin, PropKind, PropertyInfo, PropertySet,
};
use crate::symbol_source::{InheritanceShape, SymbolSource};
use crate::types::{stored_value_ty, type_name, Ty, TypeName, Visibility};
use std::collections::HashMap;

/// The current module's declarations as a [`SymbolSource`]. Borrows the frontend symbols; cheap.
pub struct ModuleSymbols<'a> {
    syms: &'a FrontendSymbols,
    source_file: Option<u32>,
}

impl<'a> ModuleSymbols<'a> {
    pub fn new(syms: &'a FrontendSymbols) -> Self {
        ModuleSymbols {
            syms,
            source_file: None,
        }
    }

    pub fn for_file(syms: &'a FrontendSymbols, source_file: u32) -> Self {
        ModuleSymbols {
            syms,
            source_file: Some(source_file),
        }
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
                    .map(move |s| lib_member(n, s, c.internal_name(), c.is_interface()))
            })
            .collect();
        let companion = c
            .static_methods
            .iter()
            .map(|(n, s)| lib_member(n, s, c.internal_name(), c.is_interface()))
            .collect();
        // The primary constructor (+ secondaries) as `<init>` members returning Unit.
        let mut constructors = Vec::new();
        if c.has_primary_ctor {
            constructors.push(LibraryMember::new(
                "<init>".to_string(),
                c.ctor_params.clone(),
                Ty::Unit,
                String::new(),
            ));
        }
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
        let enum_entries = self.syms.enums.iter().find_map(|(name, entries)| {
            self.syms
                .classes
                .get(name)
                .is_some_and(|class| class.internal_name() == c.internal_name())
                .then(|| entries.clone())
        });
        let kind = if c.is_annotation() {
            crate::libraries::TypeKind::Annotation
        } else if c.is_object() {
            crate::libraries::TypeKind::Object
        } else if enum_entries.is_some() {
            crate::libraries::TypeKind::Enum
        } else if c.is_interface() {
            crate::libraries::TypeKind::Interface
        } else {
            crate::libraries::TypeKind::Class
        };
        let enum_entries = enum_entries.unwrap_or_default();
        let sealed_subclasses = if c.is_sealed() {
            self.syms.subclass_names_of(c.internal_name()).into()
        } else {
            Default::default()
        };
        LibraryType {
            is_public: c.visibility == Visibility::Public,
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
            sealed_subclasses,
            enum_entries,
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
            let rendered = i.render();
            if out.is_empty() {
                if let Some(signature) = rendered
                    .strip_suffix("$Companion")
                    .and_then(|outer| self.class_by_internal(outer))
                    .filter(|class| class.companion_fun_names.contains(name))
                    .and_then(|class| class.static_methods.get(name))
                {
                    out.push(lib_member(name, signature, i, false));
                }
            }
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
            out.push(lib_member(name, sig, c.internal_name(), c.is_interface()));
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
                generic_sig: fi.generic_sig.clone(),
                projected_return_hazard: fi.projected_return_hazard,
                flags: SigFlags::default()
                    .with_vararg(fi.call_sig.vararg)
                    .with_is_inline(fi.flags.inline.can_inline())
                    .with_is_operator(false)
                    .with_is_override(false)
                    .with_is_final(true)
                    .with_is_suspend(fi.flags.suspend),
                vararg_index: fi.call_sig.vararg_index,
                required: fi.call_sig.required,
                param_defaults: fi.call_sig.param_defaults.clone(),
                param_default_values: Vec::new(),
                param_names: fi.call_sig.param_names.clone(),
                lambda_param_types: fi.call_sig.lambda_param_types.clone(),
                lambda_recv: Vec::new(),
                visibility: fi.visibility,
                context_count: fi.context_count,
                source_decl: None,
                source_file: None,
                source_receiver: None,
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

    fn collect_properties(
        &self,
        internal: TypeName,
        name: &str,
        out: &mut Vec<PropertyInfo>,
        seen: &mut std::collections::HashSet<TypeName>,
        rung: &mut u32,
    ) {
        if !seen.insert(internal) {
            return;
        }
        let Some(class) = self.class_by_type_name(internal) else {
            return;
        };
        let here = *rung;
        *rung += 1;
        if let Some(property) = class.declared_props.get(name) {
            out.push(source_property(class.internal_name(), property, here));
        }
        for interface in class.interfaces.iter_ids() {
            self.collect_properties(interface, name, out, seen, rung);
        }
        if let Some(superclass) = class.super_internal {
            self.collect_properties(superclass, name, out, seen, rung);
        }
    }

    fn collect_property_accessors(
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
        let Some(class) = self.class_by_type_name(internal) else {
            return;
        };
        let here = *rung;
        *rung += 1;
        for property in class.declared_props.values() {
            let accessor = if property.getter_name == name {
                Some(source_accessor(
                    class.internal_name(),
                    name,
                    property.context_params.clone(),
                    property.ty,
                    property.visibility,
                    here,
                    property.context_params.len(),
                ))
            } else if property.setter_name.as_deref() == Some(name) {
                let mut params = property.context_params.clone();
                params.push(property.ty);
                Some(source_accessor(
                    class.internal_name(),
                    name,
                    params,
                    Ty::Unit,
                    property.visibility,
                    here,
                    property.context_params.len(),
                ))
            } else {
                None
            };
            if let Some(accessor) = accessor {
                out.push(accessor);
            }
        }
        for interface in class.interfaces.iter_ids() {
            self.collect_property_accessors(interface, name, out, seen, rung);
        }
        if let Some(superclass) = class.super_internal {
            self.collect_property_accessors(superclass, name, out, seen, rung);
        }
    }
}

/// A user [`Signature`] as a [`LibraryMember`] — the module-source shape of a class method. Carries the
/// source call-shape (`call_sig`) so a named / omitted-default member call resolves through the type
/// interface.
fn lib_member(name: &str, sig: &Signature, owner: TypeName, is_interface: bool) -> LibraryMember {
    let mut m = LibraryMember::new(name.to_string(), sig.params.clone(), sig.ret, String::new());
    m.owner = Some(owner);
    m.set_is_interface(is_interface);
    m.set_suspend(sig.is_suspend());
    m.visibility = sig.visibility;
    m.inline = crate::libraries::InlineKind::from_flags(sig.is_inline(), false);
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
    let source_receiver = sig.source_receiver.or(receiver);
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
        suspend: sig.is_suspend(),
        inline: InlineKind::from_flags(sig.is_inline(), false),
        default_call: false,
        vararg_elem: None,
        signature: None,
        origin,
        // Representation lowering needs the declaration receiver before erasure.
        source_receiver,
    };
    FunctionInfo {
        receiver_rank: rank,
        generic_sig: sig.generic_sig.clone(),
        projected_return_hazard: sig.projected_return_hazard,
        call_sig: sig.call_sig(),
        context_count: sig.context_count,
        source_key: sig
            .source_file
            .zip(sig.source_decl)
            .map(|(file, decl)| (file, decl.0)),
        flags: FnFlags {
            inline: InlineKind::from_flags(sig.is_inline(), false),
            // Same-file `suspend fun` — flows from the AST via `Signature.is_suspend` so the resolver
            // reports suspend-ness uniformly with classpath callees (whose flag comes from @Metadata).
            suspend: sig.is_suspend(),
        },
        visibility: sig.visibility,
        ..FunctionInfo::plain(kind, receiver, callable)
    }
}

fn source_callable(owner: TypeName, name: String, params: Vec<Ty>, ret: Ty) -> LibraryCallable {
    LibraryCallable {
        owner,
        name,
        descriptor: String::new(),
        params,
        ret,
        physical_ret: ret,
        suspend: false,
        inline: InlineKind::None,
        default_call: false,
        vararg_elem: None,
        signature: None,
        origin: Origin::Module { facade: owner },
        source_receiver: None,
    }
}

fn source_property_getter(
    owner: TypeName,
    name: String,
    params: Vec<Ty>,
    ty: Ty,
) -> LibraryCallable {
    let mut callable = source_callable(owner, name, params, ty);
    callable.physical_ret = stored_value_ty(ty);
    callable
}

fn source_accessor(
    owner: TypeName,
    name: &str,
    params: Vec<Ty>,
    ret: Ty,
    visibility: Visibility,
    receiver_rank: u32,
    context_count: usize,
) -> FunctionInfo {
    FunctionInfo {
        visibility,
        receiver_rank,
        context_count,
        ..FunctionInfo::plain(
            FnKind::Member,
            Some(Ty::obj_name(owner)),
            source_callable(owner, name.to_string(), params, ret),
        )
    }
}

fn source_property(
    owner: TypeName,
    property: &FrontendDeclaredPropertySig,
    receiver_rank: u32,
) -> PropertyInfo {
    PropertyInfo {
        kind: PropKind::Member,
        receiver: Some(Ty::obj_name(owner)),
        formals: Vec::new(),
        ty: property.ty,
        context_count: property.context_params.len(),
        getter: source_property_getter(
            owner,
            property.getter_name.clone(),
            property.context_params.clone(),
            property.ty,
        ),
        setter: property.setter_name.as_ref().map(|setter| {
            let mut params = property.context_params.clone();
            params.push(stored_value_ty(property.ty));
            source_callable(owner, setter.clone(), params, Ty::Unit)
        }),
        is_const: false,
        visibility: property.visibility,
        owner,
        receiver_rank,
        source_key: None,
    }
}

impl SymbolSource for ModuleSymbols<'_> {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        let Some(internal) = ty.obj_internal() else {
            return Vec::new();
        };
        let Some(class) = self.class_by_type_name(internal) else {
            return Vec::new();
        };
        self.syms
            .applied_source_parents(class, class.type_parameter_bindings(ty))
            .into_iter()
            .map(|(_, applied)| applied)
            .collect()
    }

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
        // Receiver applicability is selected above this package-scoped source boundary.
        let any = Ty::obj("kotlin/Any");
        if let Some(families) = self.syms.ext_funs.get(&name) {
            for (recv, sigs) in families {
                let rank = if recv.non_null().is_ty_param() || recv.non_null() == any {
                    1
                } else {
                    0
                };
                // Surface EVERY overload registered for this (receiver, name) so the resolver's
                // overload picker can choose by arity/argument types (`fun R.f()` vs `fun R.f(x)`).
                for sig in sigs {
                    if !pkg.matches(&sig.package) {
                        continue;
                    }
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
        let mut properties = Vec::new();
        for ((_, property_name), signatures) in &self.syms.ext_props {
            if property_name != &name {
                continue;
            }
            for property in signatures {
                if !pkg.matches(&property.package)
                    || (property.visibility.is_private()
                        && self.source_file != Some(property.source.0))
                {
                    continue;
                }
                let owner = self
                    .syms
                    .ext_prop_facades_by_decl
                    .get(&property.source)
                    .copied()
                    .unwrap_or_else(|| type_name(""));
                let getter = source_property_getter(
                    owner,
                    property.getter_name.clone(),
                    vec![property.receiver],
                    property.ty,
                );
                let setter = property.setter_name.as_ref().map(|setter_name| {
                    source_callable(
                        owner,
                        setter_name.clone(),
                        vec![property.receiver, stored_value_ty(property.ty)],
                        Ty::Unit,
                    )
                });
                properties.push(PropertyInfo {
                    kind: PropKind::Extension,
                    receiver: Some(property.receiver),
                    formals: Vec::new(),
                    ty: property.ty,
                    context_count: property.context_params.len(),
                    getter,
                    setter,
                    is_const: false,
                    visibility: property.visibility,
                    owner,
                    receiver_rank: 0,
                    source_key: Some(property.source),
                });
            }
        }
        let callables = match (overloads.is_empty(), properties.is_empty()) {
            (false, false) => Callables::Both {
                functions: FunctionSet { overloads },
                properties: PropertySet {
                    overloads: properties,
                },
            },
            (false, true) => Callables::Functions(FunctionSet { overloads }),
            (true, false) => Callables::Properties(PropertySet {
                overloads: properties,
            }),
            (true, true) => Callables::None,
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
            let mut seen = std::collections::HashSet::new();
            let mut rung = 0;
            self.collect_property_accessors(internal, name, &mut overloads, &mut seen, &mut rung);
        }
        FunctionSet { overloads }
    }

    fn property_members(&self, recv: Ty, name: &str) -> PropertySet {
        let mut overloads = Vec::new();
        if let Ty::Obj(internal, _) = recv {
            let mut seen = std::collections::HashSet::new();
            let mut rung = 0;
            self.collect_properties(internal, name, &mut overloads, &mut seen, &mut rung);
        }
        PropertySet { overloads }
    }

    fn member_is_property(&self, recv: Ty, name: &str) -> bool {
        !self.property_members(recv, name).overloads.is_empty()
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

    fn inheritance_shape_name(&self, internal: TypeName) -> Option<InheritanceShape> {
        let class = self.class_by_type_name(internal)?;
        Some(InheritanceShape {
            is_interface: class.is_interface(),
            is_extensible: !class.is_interface() && !class.is_final(),
            has_no_arg_constructor: !class.is_sealed() && class.has_no_arg_constructor(),
            supports_external_subclassing: !class.is_sealed()
                && (!class.is_abstract()
                    || (!class.has_abstract_members() && class.interfaces.is_empty())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::{CtorDefaultValue, DeclaredPropertySig, ExtPropSig};
    use std::collections::{HashMap, HashSet};

    fn sig(params: Vec<Ty>, ret: Ty) -> Signature {
        Signature {
            params,
            ret,
            generic_sig: None,
            projected_return_hazard: false,
            flags: SigFlags::default()
                .with_vararg(false)
                .with_is_inline(false)
                .with_is_operator(false)
                .with_is_override(false)
                .with_is_final(false)
                .with_is_suspend(false),
            vararg_index: None,
            required: 0,
            param_defaults: vec![],
            param_default_values: vec![],
            param_names: vec![],
            lambda_param_types: vec![],
            lambda_recv: vec![],
            visibility: crate::types::Visibility::Public,
            context_count: 0,
            source_decl: None,
            source_file: None,
            source_receiver: None,
            package: String::new(),
        }
    }

    fn class(internal: &str) -> FrontendClassSig {
        FrontendClassSig {
            internal: internal.into(),
            visibility: Visibility::Public,
            props: vec![],
            declared_props: HashMap::new(),
            member_ext_props: HashMap::new(),
            member_ext_funs: HashMap::new(),
            has_primary_ctor: true,
            ctor_params: vec![],
            ctor_param_shapes: vec![],
            ctor_param_names: vec![],
            ctor_vararg: None,
            methods: HashMap::new(),
            flags: crate::resolve::ClassFlags::default(),
            inner_of: None,
            static_methods: HashMap::new(),
            companion_fun_names: HashSet::new(),
            static_props: HashMap::new(),
            lateinit_props: HashSet::new(),
            interfaces: crate::types::TypeNameList::new(),
            interface_type_args: Vec::new(),
            super_internal: None,
            super_type_args: Vec::new(),
            super_ctor_params: Vec::new(),
            ctor_defaults: vec![],
            secondary_ctors: vec![],
            tparam_names: vec![],
            tparam_bounds: vec![],
            generic_props: HashMap::new(),
            generic_function_props: HashMap::new(),
            value_field: None,
            generic_methods: HashMap::new(),
        }
    }

    #[test]
    fn type_shapes_include_finite_domain_metadata() {
        let mut symbols = FrontendSymbols::default();

        let mut state = class("sample/State");
        state.flags = state.flags.with_sealed(true);
        let mut complete = class("sample/Complete");
        complete.super_internal = Some(type_name("sample/State"));
        symbols.classes.insert("State".to_string(), state);
        symbols.classes.insert("Complete".to_string(), complete);

        symbols
            .classes
            .insert("Phase".to_string(), class("sample/Phase"));
        symbols.enums.insert(
            "Phase".to_string(),
            vec!["FIRST".to_string(), "SECOND".to_string()],
        );

        let source = ModuleSymbols::new(&symbols);
        let state = source.resolve_type("sample/State").unwrap();
        assert_eq!(
            state.sealed_subclasses.iter_ids().collect::<Vec<_>>(),
            vec![type_name("sample/Complete")]
        );

        let phase = source.resolve_type("sample/Phase").unwrap();
        assert!(phase.is_enum());
        assert_eq!(phase.enum_entries, ["FIRST", "SECOND"]);
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
        s.set_vararg(false);
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
    fn member_properties_preserve_declaration_metadata() {
        let mut symbols = FrontendSymbols::default();
        let mut base = class("demo/Base");
        base.declared_props.insert(
            "state".into(),
            DeclaredPropertySig {
                ty: Ty::String,
                storage_ty: None,
                visibility: Visibility::Protected,
                getter_name: "getState".into(),
                setter_name: Some("setState".into()),
                context_params: Vec::new(),
            },
        );
        let mut sub = class("demo/Sub");
        sub.super_internal = Some(type_name("demo/Base"));
        symbols.insert_class("Base".into(), base);
        symbols.insert_class("Sub".into(), sub);
        let source = ModuleSymbols::new(&symbols);

        let properties = source.property_members(Ty::obj("demo/Sub"), "state");
        assert_eq!(properties.overloads.len(), 1);
        let property = &properties.overloads[0];
        assert_eq!(property.kind, PropKind::Member);
        assert_eq!(property.receiver, Some(Ty::obj("demo/Base")));
        assert_eq!(property.ty, Ty::String);
        assert_eq!(property.visibility, Visibility::Protected);
        assert_eq!(property.receiver_rank, 1);
        assert!(property.owner.matches("demo/Base"));
        assert_eq!(
            property
                .setter
                .as_ref()
                .map(|setter| setter.params.as_slice()),
            Some([Ty::String].as_slice())
        );

        let getter = source.member_overloads(Ty::obj("demo/Sub"), "getState");
        assert_eq!(getter.overloads.len(), 1);
        assert_eq!(getter.overloads[0].receiver_rank, 1);
        assert_eq!(getter.overloads[0].visibility, Visibility::Protected);
        assert!(getter.overloads[0].callable.owner.matches("demo/Base"));
        assert!(matches!(
            getter.overloads[0].callable.origin,
            Origin::Module { .. }
        ));
        assert!(source.member_is_property(Ty::obj("demo/Sub"), "state"));
    }

    #[test]
    fn member_inline_flag_flows_through_module_symbols() {
        let mut st = FrontendSymbols::default();
        let mut c = class("demo/Host");
        let mut method = sig(vec![Ty::Int], Ty::Int);
        method.set_is_inline(true);
        c.methods.insert("apply".into(), vec![method]);
        st.insert_class("Host".into(), c);
        let m = ModuleSymbols::new(&st);

        let members = m.instance_members(Ty::obj("demo/Host"), "apply");
        assert_eq!(members.len(), 1);
        assert!(members[0].inline.can_inline());

        let overloads = m.member_overloads(Ty::obj("demo/Host"), "apply");
        assert_eq!(overloads.overloads.len(), 1);
        assert!(overloads.overloads[0].flags.inline.can_inline());
        assert!(overloads.overloads[0].callable.inline.can_inline());
    }

    #[test]
    fn extension_prepends_receiver_and_keeps_source_receiver_identity() {
        let mut st = FrontendSymbols::default();
        let recv = Ty::nullable(Ty::obj("demo/Point"));
        st.ext_funs
            .entry("shifted".into())
            .or_default()
            .insert(recv.extension_recv_key(), vec![sig(vec![Ty::Int], recv)]);
        let m = ModuleSymbols::new(&st);
        // A module extension is surfaced through `resolve_symbols` by fqn, with the receiver as an attribute.
        let fs = match m.resolve_symbols("shifted").callables {
            crate::libraries::Callables::Functions(f) => f.overloads,
            _ => Vec::new(),
        };
        assert_eq!(fs.len(), 1);
        let o = &fs[0];
        assert_eq!(o.kind, FnKind::Extension);
        assert_eq!(o.callable.params, vec![recv, Ty::Int]);
        assert_eq!(o.receiver, Some(recv));
        assert_eq!(o.receiver_rank, 0);
    }

    #[test]
    fn extension_preserves_generic_signature() {
        let mut symbols = FrontendSymbols::default();
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let receiver = Ty::obj_args("demo/Container", &[parameter]);
        let generic_sig = crate::libraries::GenericSig {
            formals: vec!["T".into()],
            formal_bounds: vec![Vec::new()],
            receiver: Some(receiver),
            params: vec![Ty::fun(vec![parameter], parameter)],
            ret: receiver,
        };
        let mut signature = sig(
            vec![Ty::fun(vec![Ty::obj("kotlin/Any")], Ty::obj("kotlin/Any"))],
            receiver,
        );
        signature.generic_sig = Some(generic_sig.clone());
        symbols
            .ext_funs
            .entry("transform".into())
            .or_default()
            .insert(receiver.extension_recv_key(), vec![signature]);

        let functions = match ModuleSymbols::new(&symbols)
            .resolve_symbols("transform")
            .callables
        {
            crate::libraries::Callables::Functions(functions) => functions.overloads,
            _ => Vec::new(),
        };
        let actual = functions[0]
            .generic_sig
            .as_ref()
            .expect("generic signature");
        assert_eq!(actual.formals, generic_sig.formals);
        assert_eq!(actual.receiver, generic_sig.receiver);
        assert_eq!(actual.params, generic_sig.params);
        assert_eq!(actual.ret, generic_sig.ret);
    }

    #[test]
    fn extension_property_preserves_scope_and_source_identity() {
        let mut symbols = FrontendSymbols::default();
        let receiver = Ty::String;
        symbols.ext_props.insert(
            (receiver.erased_recv(), "label".into()),
            vec![
                ExtPropSig {
                    receiver,
                    ty: Ty::String,
                    is_var: true,
                    getter_name: "getLabel".into(),
                    setter_name: Some("setLabel".into()),
                    context_params: Vec::new(),
                    accepts_nullable_receiver: false,
                    source: (0, 3),
                    package: "one".into(),
                    visibility: Visibility::Private,
                },
                ExtPropSig {
                    receiver,
                    ty: Ty::Int,
                    is_var: false,
                    getter_name: "getLabel".into(),
                    setter_name: None,
                    context_params: Vec::new(),
                    accepts_nullable_receiver: false,
                    source: (1, 4),
                    package: "two".into(),
                    visibility: Visibility::Public,
                },
            ],
        );
        symbols
            .ext_prop_facades_by_decl
            .insert((0, 3), type_name("one/FirstKt"));
        symbols
            .ext_prop_facades_by_decl
            .insert((1, 4), type_name("two/SecondKt"));

        let private = match ModuleSymbols::for_file(&symbols, 0)
            .resolve_symbols("one/label")
            .callables
        {
            crate::libraries::Callables::Properties(properties) => properties.overloads,
            _ => Vec::new(),
        };
        assert_eq!(private.len(), 1);
        assert_eq!(private[0].source_key, Some((0, 3)));
        assert!(private[0].owner.matches("one/FirstKt"));
        assert!(private[0].setter.is_some());

        assert!(matches!(
            ModuleSymbols::for_file(&symbols, 1)
                .resolve_symbols("one/label")
                .callables,
            crate::libraries::Callables::None
        ));
        let public = match ModuleSymbols::for_file(&symbols, 0)
            .resolve_symbols("two/label")
            .callables
        {
            crate::libraries::Callables::Properties(properties) => properties.overloads,
            _ => Vec::new(),
        };
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].source_key, Some((1, 4)));
        assert!(public[0].owner.matches("two/SecondKt"));
    }

    #[test]
    fn extension_preserves_declared_source_receiver() {
        let mut st = FrontendSymbols::default();
        let declared = Ty::nullable(Ty::obj("demo/Token"));
        let mut extension = sig(vec![], Ty::String);
        extension.source_receiver = Some(declared);
        st.ext_funs
            .entry("render".into())
            .or_default()
            .insert(declared.extension_recv_key(), vec![extension]);

        let m = ModuleSymbols::new(&st);
        let fs = match m.resolve_symbols("render").callables {
            crate::libraries::Callables::Functions(f) => f.overloads,
            _ => Vec::new(),
        };
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].callable.params, vec![declared]);
        assert_eq!(fs[0].callable.source_receiver, Some(declared));
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

    #[test]
    fn resolve_type_preserves_class_visibility() {
        let mut symbols = FrontendSymbols::default();
        let mut hidden = class("demo/Hidden");
        hidden.visibility = Visibility::Private;
        symbols.insert_class("Hidden".into(), hidden);

        assert!(
            !ModuleSymbols::new(&symbols)
                .resolve_type("demo/Hidden")
                .expect("shape")
                .is_public
        );
    }

    #[test]
    fn inheritance_shape_tracks_modality_and_callable_no_arg_constructors() {
        let mut st = FrontendSymbols::default();

        let mut defaulted = class("demo/Defaulted");
        defaulted.ctor_params = vec![Ty::Int];
        defaulted.ctor_defaults = vec![Some(CtorDefaultValue::Int(1))];
        st.insert_class("Defaulted".into(), defaulted);

        let mut secondary = class("demo/Secondary");
        secondary.has_primary_ctor = false;
        secondary.ctor_params = vec![Ty::Int];
        secondary.ctor_defaults = vec![None];
        secondary.secondary_ctors = vec![vec![]];
        st.insert_class("Secondary".into(), secondary);

        let mut final_required = class("demo/FinalRequired");
        final_required.has_primary_ctor = false;
        final_required.ctor_params = vec![Ty::Int];
        final_required.ctor_defaults = vec![None];
        final_required.secondary_ctors = vec![vec![Ty::Int]];
        final_required.set_is_final(true);
        st.insert_class("FinalRequired".into(), final_required);

        let mut abstract_with_interface = class("demo/AbstractWithInterface");
        abstract_with_interface.set_is_abstract(true);
        abstract_with_interface.interfaces = vec![type_name("demo/RequiredInterface")].into();
        st.insert_class("AbstractWithInterface".into(), abstract_with_interface);

        let mut sealed = class("demo/Sealed");
        sealed.set_is_abstract(true);
        sealed.set_is_sealed(true);
        st.insert_class("Sealed".into(), sealed);

        let source = ModuleSymbols::new(&st);
        let defaulted = source
            .inheritance_shape_name(type_name("demo/Defaulted"))
            .expect("defaulted class shape");
        assert!(defaulted.is_extensible);
        assert!(defaulted.has_no_arg_constructor);

        let secondary = source
            .inheritance_shape_name(type_name("demo/Secondary"))
            .expect("secondary-constructor class shape");
        assert!(secondary.is_extensible);
        assert!(secondary.has_no_arg_constructor);

        let final_required = source
            .inheritance_shape_name(type_name("demo/FinalRequired"))
            .expect("final class shape");
        assert!(!final_required.is_extensible);
        assert!(!final_required.has_no_arg_constructor);

        let abstract_with_interface = source
            .inheritance_shape_name(type_name("demo/AbstractWithInterface"))
            .expect("abstract class shape");
        assert!(!abstract_with_interface.supports_external_subclassing);

        let sealed = source
            .inheritance_shape_name(type_name("demo/Sealed"))
            .expect("sealed class shape");
        assert!(!sealed.supports_external_subclassing);
        assert!(!sealed.has_no_arg_constructor);
    }
}
