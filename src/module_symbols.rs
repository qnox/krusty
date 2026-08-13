//! `ModuleSymbols` — the current compilation's own declarations exposed as a [`SymbolSource`].
//!
//! It wraps the user-declared half of [`crate::frontend::FrontendSymbols`] (top-level functions,
//! classes and extensions) and answers the same unified symbol query as a compiled
//! library does — so module code federates with libraries through one
//! [`crate::symbol_source::CompositeSource`] instead of the
//! scattered "user-first, else library" branching. Every callable is stamped [`Origin::Module`] so the
//! lowerer can pick the same-file / cross-file / library emit form from resolution alone.

use crate::frontend::{FrontendClassSig, FrontendDeclaredPropertySig, FrontendSymbols, Signature};
use crate::libraries::{
    CallSig, FnFlags, FnKind, FunctionInfo, FunctionSet, GenericReturnPolicy, GenericSig,
    InlineKind, LibraryCallable, LibraryMember, LibraryType, Origin, ParamList, PropKind,
    PropertyInfo, PropertySet,
};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
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

    /// The module's package namespace: the package of every declared classifier and of every top-level
    /// facade, plus each of their ancestors. There is no separate package table in the symbol table —
    /// a package exists exactly because something is declared in it — so it is derived here once and
    /// memoized. Ancestors are included because a qualifier walk steps through them
    /// (`a` then `a.b` then `a.b.C`) even when only the leaf owns declarations.
    fn packages(&self) -> std::rc::Rc<std::collections::HashSet<TypeName>> {
        if self.syms.module_cache_enabled() {
            if let Some(packages) = self.syms.module_package_cache.borrow().as_ref() {
                return packages.clone();
            }
        }
        let mut packages = std::collections::HashSet::new();
        packages.extend(self.syms.source_packages.iter().copied());
        let owners = self
            .syms
            .classes
            .keys()
            .copied()
            .chain(self.syms.fn_facades.values().copied());
        for owner in owners {
            // A nested classifier's package is the package of its outermost owner: the `$` segments are
            // nesting, not path.
            let mut package = owner.parent();
            while let Some(current) = package {
                if current == crate::types::type_name("") || !packages.insert(current) {
                    break;
                }
                package = current.parent();
            }
        }
        let packages = std::rc::Rc::new(packages);
        if self.syms.module_cache_enabled() {
            *self.syms.module_package_cache.borrow_mut() = Some(packages.clone());
        }
        packages
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

    /// Build the classifier half of this provider's single symbol record. Kept private so callers
    /// cannot query classifier metadata through a second `SymbolSource` operation.
    fn classifier_record(&self, internal: TypeName) -> Option<std::rc::Rc<LibraryType>> {
        if self.syms.module_cache_enabled() {
            if let Some(shape) = self.syms.module_shape_cache.borrow().get(&internal) {
                return shape.clone();
            }
        }
        let shape = self
            .class_by_type_name(internal)
            .map(|c| std::rc::Rc::new(self.type_shape_for(c)))
            .or_else(|| {
                let target = *self.syms.source_alias_fqns.get(&internal)?;
                let mut shape = self.type_shape_for(self.class_by_type_name(target)?);
                shape.alias_target = Some(target);
                Some(std::rc::Rc::new(shape))
            })
            // Signature collection seeds every source classifier identity before it walks any
            // declaration body. Surface that identity through the SAME namespace record as a fully
            // collected classifier; consumers that only need to walk a qualified name must not grow
            // a second, source-only existence query. A fresh ModuleSymbols is built for each early
            // inference, so this temporary header cannot outlive the immutable table snapshot.
            .or_else(|| {
                self.syms
                    .has_source_class_header(internal)
                    .then(|| std::rc::Rc::new(LibraryType::declaration_header()))
            });
        if self.syms.module_cache_enabled() {
            self.syms
                .module_shape_cache
                .borrow_mut()
                .insert(internal, shape.clone());
        }
        shape
    }

    fn class_by_type_name(&self, internal: TypeName) -> Option<&'a FrontendClassSig> {
        self.syms.class_by_type_name(internal)
    }

    fn type_shape_for(&self, c: &'a FrontendClassSig) -> LibraryType {
        let mut members: Vec<LibraryMember> = c
            .methods
            .iter()
            .flat_map(|(n, sigs)| {
                sigs.iter()
                    .map(move |s| lib_member(n, s, c.internal_name(), c.is_interface()))
            })
            .collect();
        // Class-body extensions are declarations of the classifier just like ordinary methods, even
        // though their source call shape has a second (extension) receiver. Put them in the shared type
        // shape and mark that distinction explicitly, so a consumer of the federated SymbolSource never
        // has to ask whether this classifier came from the current module or a dependency. The leading
        // receiver in `params` mirrors the physical instance-method ABI used by compiled providers.
        for (name, overloads) in &c.member_ext_funs {
            for extension in overloads {
                let mut member = lib_member(
                    name,
                    extension.signature(),
                    c.internal_name(),
                    c.is_interface(),
                );
                // A member extension is an instance method whose first method parameter is the
                // extension receiver. Context parameters remain the leading parameters of the
                // callable's semantic signature, but physically follow that receiver. Publish the
                // same shape as metadata providers: receiver + (contexts + values).
                member.params.insert(0, extension.receiver_ty());
                member.set_is_member_extension(true);
                member.set_is_operator(extension.signature().is_operator());
                members.push(member);
            }
        }
        let companion = Vec::new();
        // The primary constructor (+ secondaries) as `<init>` members returning Unit.
        let mut constructors = Vec::new();
        if c.has_primary_ctor {
            let mut constructor = LibraryMember::new(
                "<init>".to_string(),
                c.ctor_params.clone(),
                Ty::Unit,
                String::new(),
            );
            let param_names = c
                .ctor_param_names
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            let param_defaults = c
                .ctor_param_names
                .iter()
                .map(|(_, has_default)| *has_default)
                .collect::<Vec<_>>();
            let required = if param_defaults.len() == c.ctor_params.len() {
                param_defaults.iter().filter(|default| !**default).count()
            } else {
                c.ctor_params.len()
            };
            constructor.call_sig = CallSig::source(
                param_names,
                param_defaults,
                c.ctor_params
                    .iter()
                    .map(|parameter| match parameter {
                        Ty::Fun(function) => function.params.clone(),
                        _ => Vec::new(),
                    })
                    .collect(),
                c.ctor_params
                    .iter()
                    .map(
                        |parameter| matches!(parameter, Ty::Fun(function) if function.has_receiver),
                    )
                    .collect(),
                c.ctor_params
                    .iter()
                    .map(|parameter| match parameter {
                        Ty::Fun(function) => function.context_count,
                        _ => 0,
                    })
                    .collect(),
                required,
                c.ctor_vararg,
            );
            if !c.type_params.is_empty() {
                constructor.generic_sig = Some(GenericSig {
                    formals: c.type_params.clone(),
                    formal_bounds: c
                        .type_param_bounds
                        .iter()
                        .map(|bound| {
                            if *bound == Ty::Error {
                                Vec::new()
                            } else {
                                vec![*bound]
                            }
                        })
                        .collect(),
                    receiver: None,
                    params: c
                        .ctor_param_shapes
                        .iter()
                        .map(|(parameter, _)| *parameter)
                        .collect(),
                    ret: Ty::obj_args_name(
                        c.internal_name(),
                        &c.type_params
                            .iter()
                            .enumerate()
                            .map(|(index, name)| {
                                Ty::ty_param(
                                    name,
                                    c.type_param_bounds
                                        .get(index)
                                        .copied()
                                        .filter(|bound| *bound != Ty::Error)
                                        .unwrap_or_else(|| Ty::obj("kotlin/Any")),
                                )
                            })
                            .collect::<Vec<_>>(),
                    ),
                    return_policy: GenericReturnPolicy::Exact,
                });
            }
            constructors.push(constructor);
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
        let mut supertype_templates = c
            .interfaces
            .iter_ids()
            .enumerate()
            .map(|(index, parent)| {
                Ty::obj_args_name(
                    parent,
                    c.interface_type_args
                        .get(index)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        let enum_entries = self.syms.enum_entries_of(c.internal_name()).cloned();
        if enum_entries.is_some() {
            // An enum's implicit superclass is the applied Kotlin declaration `Enum<Self>`.
            // Keep that semantic argument in the common class model so inherited members such as
            // `compareTo(other: E)` are specialized from the receiver like every generic member.
            // The backend independently maps this declaration to its platform representation.
            let enum_type = crate::types::type_name("kotlin/Enum");
            supertypes.push(enum_type);
            supertype_templates.push(Ty::obj_args_name(
                enum_type,
                &[Ty::obj_name(c.internal_name())],
            ));
        } else if let Some(s) = c.super_internal {
            supertypes.push(s);
            supertype_templates.push(Ty::obj_args_name(s, &c.super_type_args));
        } else if c.internal_name() != crate::types::wk::any() {
            // Kotlin's root class is implicit in source syntax (`class A` and `interface I`), but it
            // is still part of the semantic classifier signature. Publish it in the same record as
            // every explicit supertype so the core hierarchy sees `Any.equals/hashCode/toString`
            // without resolver-side name synthesis or a source-vs-library branch.
            supertypes.push(crate::types::wk::any());
            supertype_templates.push(Ty::obj_name(crate::types::wk::any()));
        }
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
        let enum_entries_accessor = enum_entries.as_ref().map(|_| {
            // Describe the accessor the backend emits even though it has no source declaration. This
            // is the same LibraryMember capability dependency providers expose, so consumers retain
            // one origin-independent target and lowering need not reconstruct its JVM call. Keep the
            // logical name uncallable: `Enum.getEntries()` is not a Kotlin source function; only the
            // synthetic `Enum.entries` property may select its physical name.
            let mut entries = LibraryMember::new(
                "<enum-entries-accessor>".to_string(),
                Vec::new(),
                Ty::obj_args(
                    "kotlin/enums/EnumEntries",
                    &[Ty::obj_name(c.internal_name())],
                ),
                "()Lkotlin/enums/EnumEntries;".to_string(),
            );
            entries.owner = Some(c.internal_name());
            entries.physical_name = Some("getEntries".to_string());
            entries
        });
        let enum_entries = enum_entries.unwrap_or_default();
        let sealed_subclasses = if c.is_sealed() {
            self.syms.subclass_names_of(c.internal_name()).into()
        } else {
            Default::default()
        };
        let ctor_named_params = (c.has_primary_ctor
            && c.ctor_param_names.len() == c.ctor_params.len())
        .then(|| ParamList {
            visibility: c.visibility,
            names: c
                .ctor_param_names
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
            defaults: c
                .ctor_param_names
                .iter()
                .map(|(_, has_default)| *has_default)
                .collect(),
            types: c.ctor_params.clone(),
            recv_fun: c
                .ctor_params
                .iter()
                .map(|parameter| matches!(parameter.non_null(), Ty::Fun(sig) if sig.has_receiver))
                .collect(),
            vararg: c.ctor_vararg,
        })
        .into_iter()
        .collect();
        let mut shape = LibraryType {
            access: c.visibility.into(),
            source_file: Some(c.source_file),
            is_nested: c.internal_name().contains("$"),
            outer_instance: c.inner_of,
            kind,
            inheritance: crate::libraries::ClassifierInheritance {
                is_abstract: c.is_abstract() || c.is_interface(),
                is_extensible: !c.is_interface() && !c.is_final(),
                has_no_arg_constructor: !c.is_sealed() && c.has_no_arg_constructor(),
                supports_external_subclassing: !c.is_sealed()
                    && (!c.is_abstract() || (!c.has_abstract_members() && c.interfaces.is_empty())),
            },
            supertypes: supertypes.into(),
            supertype_templates,
            constructors,
            fields: Vec::new(),
            declared_callables: HashMap::new(),
            members,
            companion,
            constants: HashMap::new(),
            sam_method: self.source_sam_method(c),
            callable_signature: c.callable_signature,
            // Publish the source companion through the same classifier record as a dependency
            // companion. Core can then treat `Type(args)` → companion `operator fun invoke` as one
            // callable-tower case instead of retaining a source-only retry path.
            companion_object: c
                .companion_internal
                .map(|companion| (companion.nested_segment_ref().to_string(), companion)),
            value_companion_fns: Vec::new(),
            // `LibraryType` is the provider-neutral classifier shape consumed by the federated
            // resolver. Preserve source value-class metadata here just as a classpath provider
            // does, so every downstream query (identity diagnostics included) can use the common
            // `SymbolSource::is_value_name` contract instead of branching on symbol provenance.
            value_underlying: c.value_field.as_ref().map(|(_, ty)| *ty),
            value_underlying_property: c.value_field.as_ref().map(|(name, _)| name.clone()),
            alias_target: None,
            // Preserve the classifier's formals on the common type shape. Receiver-coupled queries can
            // then bind `Scope<String>` before selecting a member extension declared on `Scope<T>`, in
            // exactly the same way for source and decoded metadata.
            type_parameters: crate::types::TypeParameters::new(
                c.type_params
                    .iter()
                    .chain(c.captured_type_parameters.type_params.iter())
                    .cloned()
                    .collect(),
                c.type_param_bounds
                    .iter()
                    .map(|bound| {
                        (*bound != Ty::Error)
                            .then_some(vec![*bound])
                            .unwrap_or_default()
                    })
                    .chain(
                        c.captured_type_parameters
                            .type_param_bounds
                            .iter()
                            .map(|bound| vec![*bound]),
                    )
                    .collect(),
                c.type_param_variances
                    .iter()
                    .chain(c.captured_type_parameters.type_param_variances.iter())
                    .copied()
                    .collect(),
            ),
            sealed_subclasses,
            enum_entries,
            enum_entries_accessor,
            ctor_named_params,
            retention: None,
        };
        let mut names = c
            .methods
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        names.extend(c.member_ext_funs.keys().cloned());
        names.extend(c.member_ext_props.keys().cloned());
        for (name, property) in &c.declared_props {
            names.insert(name.clone());
            names.insert(property.getter_name.clone());
            if let Some(setter) = &property.setter_name {
                names.insert(setter.clone());
            }
        }
        for name in names {
            let declarations = self.declared_callables_for(c, &name);
            if !matches!(declarations, crate::libraries::Callables::None) {
                shape.declared_callables.insert(name, declarations);
            }
        }
        shape
    }

    /// Publish a source functional interface's single abstract method on the same classifier record
    /// used by every other symbol provider. The method shape comes from the collected declaration
    /// metadata (including generic supertype substitution); consumers never infer it from a
    /// classifier spelling or retry against a source-only table.
    fn source_sam_method(&self, root: &FrontendClassSig) -> Option<LibraryMember> {
        if !root.is_fun_interface() {
            return None;
        }

        crate::trace_compiler!(
            "resolve",
            "source SAM metadata owner={} type_params={:?}",
            root.internal_name(),
            root.type_params
        );

        let root_args = root
            .type_params
            .iter()
            .enumerate()
            .map(|(index, name)| {
                Ty::ty_param(
                    name,
                    root.type_param_bounds
                        .get(index)
                        .copied()
                        .filter(|bound| *bound != Ty::Error)
                        .unwrap_or_else(|| Ty::obj("kotlin/Any")),
                )
            })
            .collect::<Vec<_>>();
        let root_ty = Ty::obj_args_name(root.internal_name(), &root_args);
        let mut declared = std::collections::HashSet::new();
        let mut abstract_methods = Vec::new();

        for (owner, applied, _) in self.syms.applied_source_hierarchy_bfs(root_ty) {
            let Some(class) = self.class_by_type_name(owner) else {
                continue;
            };
            let bindings = class.type_parameter_bindings(applied);
            for (name, signatures) in &class.methods {
                for signature in signatures {
                    crate::trace_compiler!(
                        "resolve",
                        "  SAM declaration {}.{} params={:?} abstract={}",
                        class.internal_name(),
                        name,
                        signature.params,
                        signature.is_abstract()
                    );
                    if !declared.insert((name.clone(), signature.params.clone()))
                        || !signature.is_abstract()
                    {
                        continue;
                    }
                    let mut member =
                        lib_member(name, signature, class.internal_name(), class.is_interface());
                    if let Some(generic) = class.generic_methods.get(name).and_then(|methods| {
                        methods
                            .iter()
                            .find(|method| method.params == signature.params)
                    }) {
                        member.generic_sig = Some(GenericSig {
                            formals: root.type_params.clone(),
                            formal_bounds: root
                                .type_param_bounds
                                .iter()
                                .map(|bound| {
                                    (*bound != Ty::Error)
                                        .then_some(vec![*bound])
                                        .unwrap_or_default()
                                })
                                .collect(),
                            receiver: None,
                            params: generic
                                .param_shapes
                                .iter()
                                .map(|parameter| parameter.substitute_erased(&bindings))
                                .collect(),
                            ret: generic.ret_shape.substitute_erased(&bindings),
                            return_policy: GenericReturnPolicy::Exact,
                        });
                    }
                    abstract_methods.push(member);
                }
            }
            for (name, signatures) in &class.member_ext_funs {
                for extension in signatures {
                    let signature = extension.signature();
                    let receiver = crate::types::ty_subst(extension.receiver_ty(), &bindings);
                    let mut semantic_params = signature.params.clone();
                    semantic_params.insert(0, receiver);
                    crate::trace_compiler!(
                        "resolve",
                        "  SAM extension declaration {}.{} params={:?} abstract={}",
                        class.internal_name(),
                        name,
                        semantic_params,
                        signature.is_abstract()
                    );
                    if !declared.insert((name.clone(), semantic_params.clone()))
                        || !signature.is_abstract()
                    {
                        continue;
                    }
                    let mut member =
                        lib_member(name, signature, class.internal_name(), class.is_interface());
                    member.params = semantic_params.clone();
                    member.set_is_member_extension(true);
                    if let Some(generic) = member.generic_sig.as_mut() {
                        let receiver_shape =
                            generic.receiver.take().unwrap_or(extension.receiver_ty());
                        generic.params.insert(
                            signature.context_count.min(generic.params.len()),
                            receiver_shape,
                        );
                    }
                    abstract_methods.push(member);
                }
            }
        }

        match abstract_methods.as_slice() {
            [method] => Some(method.clone()),
            methods => {
                crate::trace_compiler!(
                    "resolve",
                    "source SAM metadata owner={} abstract_count={}",
                    root.internal_name(),
                    methods.len()
                );
                None
            }
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
            // Synthetic source companion classifiers are still module-owned and publish their
            // members through the same record as every ordinary source classifier.
            if let Some(shape) = self.classifier_record(internal) {
                out.extend(
                    shape
                        .members
                        .iter()
                        .filter(|member| member.name == name)
                        .cloned(),
                );
            }
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
                    CallableOwner {
                        internal: owner,
                        is_interface: false,
                    },
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

    /// Package-scoped top-level overloads callable from this source file.
    pub fn top_level_overloads_accessible_in_scope(
        &self,
        name: &str,
        packages: &[TypeName],
    ) -> Vec<FunctionInfo> {
        self.top_level_overloads_in_scope(name, packages)
            .into_iter()
            .filter(|function| {
                !function.visibility.is_private()
                    || function
                        .source_key
                        .is_some_and(|(file, _)| Some(file) == self.source_file)
            })
            .collect()
    }

    fn declared_callables_for(
        &self,
        class: &FrontendClassSig,
        name: &str,
    ) -> crate::libraries::Callables {
        let internal = class.internal_name();
        let mut functions = class
            .methods_named(name)
            .iter()
            .map(|signature| {
                fn_info(
                    FnKind::Member,
                    signature,
                    None,
                    CallableOwner {
                        internal,
                        is_interface: class.is_interface(),
                    },
                    name,
                    0,
                    Origin::Module { facade: internal },
                )
            })
            .collect::<Vec<_>>();
        for property in class.declared_props.values() {
            let accessor = if property.getter_name == name {
                Some(source_accessor(
                    CallableOwner {
                        internal,
                        is_interface: class.is_interface(),
                    },
                    name,
                    property.context_params.clone(),
                    property.ty,
                    property.visibility,
                    0,
                    property.context_params.len(),
                ))
            } else if property.setter_name.as_deref() == Some(name) {
                let mut params = property.context_params.clone();
                params.push(property.ty);
                Some(source_accessor(
                    CallableOwner {
                        internal,
                        is_interface: class.is_interface(),
                    },
                    name,
                    params,
                    Ty::Unit,
                    property.visibility,
                    0,
                    property.context_params.len(),
                ))
            } else {
                None
            };
            functions.extend(accessor);
        }
        let properties = class
            .declared_props
            .get(name)
            .map(|property| {
                let mut declaration = source_property(internal, property, class.is_interface(), 0);
                let template = class
                    .generic_props
                    .get(name)
                    .and_then(|&(index, definitely_non_null)| {
                        let parameter = class.type_params.get(index)?;
                        let bound = class
                            .type_param_bounds
                            .get(index)
                            .copied()
                            .filter(|bound| *bound != Ty::Error)
                            // Kotlin's implicit upper bound is `Any?`, not `Any`. Keeping it
                            // nullable is what lets an ordinary `T` substitute to `String?`; only
                            // an explicit `T & Any` property removes that nullability below.
                            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                        Some(Ty::ty_param(
                            parameter,
                            if definitely_non_null {
                                bound.non_null()
                            } else {
                                bound
                            },
                        ))
                    })
                    .or_else(|| class.generic_property_shapes.get(name).copied())
                    .unwrap_or(property.ty);
                declaration.ty = template;
                declaration.getter.ret = template;
                if let Some(setter) = &mut declaration.setter {
                    if let Some(parameter) = setter.params.last_mut() {
                        *parameter = template;
                    }
                }
                declaration
            })
            .into_iter()
            .collect::<Vec<_>>();
        crate::libraries::Callables::from_parts(
            FunctionSet {
                overloads: functions,
            },
            PropertySet {
                overloads: properties,
            },
        )
    }
}

/// A user [`Signature`] as a [`LibraryMember`] — the module-source shape of a class method. Carries the
/// source call-shape (`call_sig`) so a named / omitted-default member call resolves through the type
/// interface.
fn lib_member(name: &str, sig: &Signature, owner: TypeName, is_interface: bool) -> LibraryMember {
    member_from_signature(name, sig, owner, is_interface)
}

pub(crate) fn member_from_signature(
    name: &str,
    sig: &Signature,
    owner: TypeName,
    is_interface: bool,
) -> LibraryMember {
    let mut m = LibraryMember::new(name.to_string(), sig.params.clone(), sig.ret, String::new());
    m.owner = Some(owner);
    m.generic_sig = sig.generic_sig.clone();
    m.set_is_interface(is_interface);
    m.set_is_abstract(sig.is_abstract());
    m.set_suspend(sig.is_suspend());
    m.visibility = sig.visibility;
    m.inline = crate::libraries::InlineKind::from_flags(sig.is_inline(), sig.requires_splice());
    m.call_sig = sig.call_sig();
    m.context_count = sig.context_count;
    m.default_values = sig.param_default_values.clone();
    m.plugin_expression = sig.plugin_expression;
    m
}

/// Build a top-level / extension `FunctionInfo` from a user [`Signature`]. `receiver` is `Some` for an
/// extension (prepended to `params`, matching the library convention that `params[0]` is the receiver).
#[derive(Clone, Copy)]
struct CallableOwner {
    internal: TypeName,
    is_interface: bool,
}

fn fn_info(
    kind: FnKind,
    sig: &Signature,
    receiver: Option<Ty>,
    owner: CallableOwner,
    name: &str,
    rank: u32,
    origin: Origin,
) -> FunctionInfo {
    let CallableOwner {
        internal: owner,
        is_interface: owner_is_interface,
    } = owner;
    let source_receiver = sig.source_receiver.or(receiver);
    let mut params: Vec<Ty> = Vec::new();
    if let Some(r) = receiver {
        params.push(r);
    }
    params.extend(sig.params.iter().copied());
    let callable = LibraryCallable {
        owner,
        name: name.to_string(),
        compiler_intrinsic: None,
        inline_body_plan: None,
        plugin_expression: sig.plugin_expression,
        descriptor: String::new(),
        physical_params: params.clone(),
        params,
        ret: sig.ret,
        physical_ret: sig.ret,
        suspend: sig.is_suspend(),
        // Declaration capabilities travel on the selected callable for every symbol source. A module
        // owner may be re-readable today, but making later consumers re-query only that origin would
        // let the source and classpath resolution paths drift and would lose this fact on a generic
        // `FunctionInfo` → `LibraryMember` round trip.
        owner_is_interface,
        member_realization: crate::libraries::MemberRealization::Dispatch,
        inline: InlineKind::from_flags(sig.is_inline(), sig.requires_splice()),
        default_call: false,
        vararg_elem: None,
        vararg_index: None,
        signature: None,
        origin,
        // Representation lowering needs the declaration receiver before erasure.
        source_receiver,
        context_count: sig.context_count,
        contract: sig.contract.clone(),
        generic_sig: sig.generic_sig.clone().map(Box::new),
        singleton_dispatch: None,
        default_realization: None,
        // A SOURCE callable's `ret` is already the declared type and its `physical_ret` is not yet
        // erased, so there is no carrier-vs-box question for the value-class pass to answer here — it
        // sees the declaration itself. The fact exists for callables read back from a class file.
        declared_ret: None,
    };
    FunctionInfo {
        receiver_rank: rank,
        generic_sig: sig.generic_sig.clone(),
        projected_return_hazard: sig.projected_return_hazard,
        call_sig: sig.call_sig(),
        default_values: sig.param_default_values.clone(),
        context_count: sig.context_count,
        source_key: sig
            .source_file
            .zip(sig.source_decl)
            .map(|(file, decl)| (file, decl.0)),
        flags: FnFlags {
            inline: InlineKind::from_flags(sig.is_inline(), sig.requires_splice()),
            // Same-file `suspend fun` — flows from the AST via `Signature.is_suspend` so the resolver
            // reports suspend-ness uniformly with classpath callees (whose flag comes from @Metadata).
            suspend: sig.is_suspend(),
            operator: sig.is_operator(),
            is_abstract: sig.is_abstract(),
            low_priority: sig.low_priority(),
        },
        visibility: sig.visibility,
        ..FunctionInfo::plain(kind, receiver, callable)
    }
}

fn source_callable(
    owner: TypeName,
    name: String,
    params: Vec<Ty>,
    ret: Ty,
    owner_is_interface: bool,
) -> LibraryCallable {
    LibraryCallable {
        owner,
        name,
        compiler_intrinsic: None,
        inline_body_plan: None,
        plugin_expression: None,
        descriptor: String::new(),
        physical_params: params.clone(),
        params,
        ret,
        physical_ret: ret,
        suspend: false,
        owner_is_interface,
        member_realization: crate::libraries::MemberRealization::Dispatch,
        inline: InlineKind::None,
        default_call: false,
        vararg_elem: None,
        vararg_index: None,
        signature: None,
        origin: Origin::Module { facade: owner },
        source_receiver: None,
        context_count: 0,
        contract: None,
        generic_sig: None,
        singleton_dispatch: None,
        default_realization: None,
        // See the note in the builder above: a source callable carries its declaration un-erased.
        declared_ret: None,
    }
}

fn source_property_getter(
    owner: TypeName,
    name: String,
    params: Vec<Ty>,
    ty: Ty,
    owner_is_interface: bool,
) -> LibraryCallable {
    let mut callable = source_callable(owner, name, params, ty, owner_is_interface);
    callable.physical_ret = stored_value_ty(ty);
    callable
}

fn source_accessor(
    owner: CallableOwner,
    name: &str,
    params: Vec<Ty>,
    ret: Ty,
    visibility: Visibility,
    receiver_rank: u32,
    context_count: usize,
) -> FunctionInfo {
    let CallableOwner {
        internal: owner,
        is_interface: owner_is_interface,
    } = owner;
    FunctionInfo {
        visibility,
        receiver_rank,
        context_count,
        ..FunctionInfo::plain(
            FnKind::Member,
            Some(Ty::obj_name(owner)),
            source_callable(owner, name.to_string(), params, ret, owner_is_interface),
        )
    }
}

fn source_property(
    owner: TypeName,
    property: &FrontendDeclaredPropertySig,
    owner_is_interface: bool,
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
            owner_is_interface,
        ),
        setter: property.setter_name.as_ref().map(|setter| {
            let mut params = property.context_params.clone();
            params.push(stored_value_ty(property.ty));
            source_callable(owner, setter.clone(), params, Ty::Unit, owner_is_interface)
        }),
        setter_visibility: property.setter_visibility.unwrap_or(property.visibility),
        is_const: property.is_const,
        visibility: property.visibility,
        owner,
        receiver_rank,
        source_key: None,
    }
}

impl SymbolSource for ModuleSymbols<'_> {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        crate::types::existing_type_name_child(parent, name)
            .is_some_and(|package| self.packages().contains(&package))
    }

    fn symbols(
        &self,
        namespace: SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        use crate::libraries::{Callables, ResolvedSymbols};
        if self.syms.module_cache_enabled() {
            if let Some(record) = self
                .syms
                .module_symbol_cache
                .borrow()
                .get(&(self.source_file, namespace))
                .and_then(|symbols| symbols.get(name))
            {
                return record.clone();
            }
        }
        // Classifier: a module class at the fqn. Callables: `functions(name, receiver)` — members (always
        // visible on their type) plus the module's top-level/extension functions when the fqn's package is
        // their declaring package (a same-file function has no recorded facade — it lives in the file's own
        // package, which the resolver queries as the same-package candidate fqn).
        let classifier_name = namespace
            .existing_classifier(name)
            .filter(|&internal| self.classifier_record(internal).is_some());
        let classifier = classifier_name.and_then(|internal| self.classifier_record(internal));
        let classifier_name = classifier.as_ref().map(|classifier| {
            classifier
                .alias_target
                .unwrap_or_else(|| classifier_name.expect("classifier identity"))
        });
        let name = name.to_string();
        let package = match namespace {
            SymbolNamespace::Package(package) => Some(package),
            SymbolNamespace::Classifier(_) => None,
        };
        let mut overloads = if package.is_some() && self.syms.funs.contains_key(&name) {
            self.top_level_overloads(&name)
                .into_iter()
                .filter(|function| {
                    function
                        .source_key
                        .and_then(|(file, decl)| {
                            self.syms.funs.get(&name).and_then(|signatures| {
                                signatures.iter().find(|signature| {
                                    signature.source_file == Some(file)
                                        && signature.source_decl.is_some_and(|id| id.0 == decl)
                                })
                            })
                        })
                        .is_some_and(|signature| {
                            package.is_some_and(|package| package.matches(&signature.package))
                        })
                })
                .collect()
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
                    if !package.is_some_and(|package| package.matches(&sig.package)) {
                        continue;
                    }
                    overloads.push(fn_info(
                        FnKind::Extension,
                        sig,
                        Some(*recv),
                        CallableOwner {
                            internal: crate::types::type_name(""),
                            is_interface: false,
                        },
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
        // `import Owner.member` makes an object's ordinary member a receiver-less callable in the
        // import scope; dispatch still targets the singleton. Normalize it here, at the provider
        // boundary, so the checker sees the same `FunctionInfo` shape as a dependency object member
        // and ordinary overload selection remains origin-neutral.
        if let SymbolNamespace::Classifier(owner) = namespace {
            if let Some(classifier) = self.classifier_record(owner).filter(|ty| ty.is_object()) {
                let companion_storage = owner.nested_owner().and_then(|outer| {
                    self.classifier_record(outer)
                        .and_then(|outer_type| outer_type.companion_object.clone())
                        .filter(|(_, companion)| *companion == owner)
                        .map(|(field, _)| (outer, field))
                });
                let (field_owner, field_name) =
                    companion_storage.unwrap_or_else(|| (owner, "INSTANCE".to_string()));
                let singleton = crate::libraries::StaticFieldRef {
                    owner: field_owner,
                    name: field_name,
                    descriptor: Some(format!("L{};", owner.render())),
                    ty: Ty::obj_name(owner),
                    constant: None,
                };
                let imported = classifier
                    .declared_callables
                    .get(&name)
                    .cloned()
                    .unwrap_or_default();
                let (imported_functions, imported_properties) = imported.into_parts();
                for mut function in imported_functions.overloads {
                    function.kind = FnKind::TopLevel;
                    function.receiver = None;
                    function.source_key = None;
                    function.callable.singleton_dispatch = Some(Box::new(singleton.clone()));
                    overloads.push(function);
                }
                for mut property in imported_properties.overloads {
                    property.kind = PropKind::TopLevel;
                    property.receiver = None;
                    property.getter.singleton_dispatch = Some(Box::new(singleton.clone()));
                    if let Some(setter) = &mut property.setter {
                        setter.singleton_dispatch = Some(Box::new(singleton.clone()));
                    }
                    properties.push(property);
                }
                if let Some(class) = self.class_by_type_name(owner) {
                    for declaration in class.member_ext_funs(&name) {
                        let mut function = fn_info(
                            FnKind::Extension,
                            declaration.signature(),
                            Some(declaration.receiver_ty()),
                            CallableOwner {
                                internal: owner,
                                is_interface: class.is_interface(),
                            },
                            &name,
                            0,
                            Origin::Module { facade: owner },
                        );
                        // This is a member callable imported into the receiver-less callable scope,
                        // not a top-level declaration. Its complete emit handle is the singleton
                        // dispatch below; a source declaration key would misroute it through the
                        // cross-file static-extension path.
                        function.source_key = None;
                        function.callable.singleton_dispatch = Some(Box::new(singleton.clone()));
                        overloads.push(function);
                    }
                    for declaration in class.member_ext_props(&name) {
                        let mut getter_params = vec![declaration.receiver_ty()];
                        getter_params.extend_from_slice(declaration.context_params());
                        let mut getter = source_property_getter(
                            owner,
                            crate::names::property_getter_name(&name),
                            getter_params.clone(),
                            declaration.ret(),
                            class.is_interface(),
                        );
                        getter.singleton_dispatch = Some(Box::new(singleton.clone()));
                        getter.source_receiver = Some(declaration.receiver_ty());
                        getter.context_count = declaration.context_params().len();
                        let setter = declaration.is_var().then(|| {
                            let mut params = getter_params;
                            params.push(stored_value_ty(declaration.ret()));
                            let mut setter = source_callable(
                                owner,
                                crate::names::property_setter_name(&name),
                                params,
                                Ty::Unit,
                                class.is_interface(),
                            );
                            setter.singleton_dispatch = Some(Box::new(singleton.clone()));
                            setter.source_receiver = Some(declaration.receiver_ty());
                            setter.context_count = declaration.context_params().len();
                            setter
                        });
                        properties.push(PropertyInfo {
                            kind: PropKind::Extension,
                            receiver: Some(declaration.receiver_ty()),
                            formals: declaration.type_params().to_vec(),
                            ty: declaration.ret(),
                            context_count: declaration.context_params().len(),
                            getter,
                            setter,
                            setter_visibility: declaration
                                .setter_visibility()
                                .unwrap_or_else(|| declaration.visibility()),
                            is_const: false,
                            visibility: declaration.visibility(),
                            owner,
                            receiver_rank: 0,
                            source_key: None,
                        });
                    }
                }
            }
        }
        for (&source, property) in &self.syms.source_props {
            if property.name != name
                || !package.is_some_and(|package| package.matches(&property.package))
                || (property.visibility.is_private() && self.source_file != Some(source.0))
            {
                continue;
            }
            let owner = self
                .syms
                .prop_facades_by_decl
                .get(&source)
                .copied()
                .unwrap_or_else(|| type_name(""));
            let getter = source_property_getter(
                owner,
                crate::names::property_getter_name(&name),
                property.context_params.clone(),
                property.ty,
                false,
            );
            let setter = property.is_var.then(|| {
                let mut params = property.context_params.clone();
                params.push(stored_value_ty(property.ty));
                source_callable(
                    owner,
                    crate::names::property_setter_name(&name),
                    params,
                    Ty::Unit,
                    false,
                )
            });
            properties.push(PropertyInfo {
                kind: PropKind::TopLevel,
                receiver: None,
                formals: Vec::new(),
                ty: property.ty,
                context_count: property.context_params.len(),
                getter,
                setter,
                setter_visibility: property.setter_visibility,
                is_const: property.is_const,
                visibility: property.visibility,
                owner,
                receiver_rank: 0,
                source_key: Some(source),
            });
        }
        for ((_, property_name), signatures) in &self.syms.ext_props {
            if property_name != &name {
                continue;
            }
            for property in signatures {
                if !package.is_some_and(|package| package.matches(&property.package))
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
                let mut getter_params = vec![property.receiver];
                getter_params.extend(property.context_params.iter().copied());
                let getter = source_property_getter(
                    owner,
                    property.getter_name.clone(),
                    getter_params.clone(),
                    property.ty,
                    false,
                );
                let setter = property.setter_name.as_ref().map(|setter_name| {
                    let mut params = getter_params.clone();
                    params.push(stored_value_ty(property.ty));
                    source_callable(owner, setter_name.clone(), params, Ty::Unit, false)
                });
                properties.push(PropertyInfo {
                    kind: PropKind::Extension,
                    receiver: Some(property.receiver),
                    formals: property.formals.clone(),
                    ty: property.ty,
                    context_count: property.context_params.len(),
                    getter,
                    setter,
                    setter_visibility: property.visibility,
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
        let record = std::rc::Rc::new(ResolvedSymbols {
            classifier_name,
            classifier,
            callables,
        });
        if self.syms.module_cache_enabled() {
            self.syms
                .module_symbol_cache
                .borrow_mut()
                .entry((self.source_file, namespace))
                .or_default()
                .insert(name, record.clone());
        }
        record
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{CtorDefaultValue, FrontendClassFlags, FrontendExtPropSig, SigFlags};
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
            exact_params: vec![],
            implicit_integer_coercion: vec![],
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
            contract: None,
            plugin_expression: None,
        }
    }

    fn class(internal: &str) -> FrontendClassSig {
        FrontendClassSig {
            internal: internal.into(),
            source_file: 0,
            source_decl: None,
            visibility: Visibility::Public,
            props: vec![],
            declared_props: HashMap::new(),
            constants: HashMap::new(),
            member_ext_props: HashMap::new(),
            member_ext_funs: HashMap::new(),
            has_primary_ctor: true,
            ctor_params: vec![],
            ctor_param_shapes: vec![],
            ctor_param_names: vec![],
            ctor_vararg: None,
            methods: HashMap::new(),
            source_methods: HashMap::new(),
            flags: FrontendClassFlags::default(),
            inner_of: None,
            companion_internal: None,
            lateinit_props: HashSet::new(),
            interfaces: crate::types::TypeNameList::new(),
            interface_type_args: Vec::new(),
            callable_signature: None,
            super_internal: None,
            super_type_args: Vec::new(),
            super_ctor_params: Vec::new(),
            ctor_defaults: vec![],
            secondary_ctors: vec![],
            type_parameters: crate::types::TypeParameters::default(),
            captured_type_parameters: crate::types::TypeParameters::default(),
            metadata_captured_type_parameters: Vec::new(),
            generic_props: HashMap::new(),
            generic_property_shapes: HashMap::new(),
            value_field: None,
            generic_methods: HashMap::new(),
        }
    }

    fn declared(
        source: &dyn SymbolSource,
        receiver: Ty,
        name: &str,
    ) -> crate::libraries::Callables {
        receiver
            .obj_internal()
            .and_then(|owner| source.classifier(owner))
            .and_then(|classifier| classifier.declared_callables.get(name).cloned())
            .unwrap_or_default()
    }

    #[test]
    fn type_shapes_include_finite_domain_metadata() {
        let mut symbols = FrontendSymbols::default();

        let mut state = class("sample/State");
        state.flags = state.flags.with_sealed(true);
        let mut complete = class("sample/Complete");
        complete.super_internal = Some(type_name("sample/State"));
        symbols.classes.insert(type_name("sample/State"), state);
        symbols
            .classes
            .insert(type_name("sample/Complete"), complete);

        symbols
            .classes
            .insert(type_name("sample/Phase"), class("sample/Phase"));
        symbols.enums.insert(
            type_name("sample/Phase"),
            vec!["FIRST".to_string(), "SECOND".to_string()],
        );

        let source = ModuleSymbols::new(&symbols);
        let state = source.classifier(type_name("sample/State")).unwrap();
        assert_eq!(
            state.sealed_subclasses.iter_ids().collect::<Vec<_>>(),
            vec![type_name("sample/Complete")]
        );

        let phase = source.classifier(type_name("sample/Phase")).unwrap();
        assert!(phase.is_enum());
        assert_eq!(phase.enum_entries, ["FIRST", "SECOND"]);
        let entries = phase
            .enum_entries_accessor
            .as_ref()
            .expect("source enum shape should retain its synthetic entries accessor");
        assert_eq!(entries.name, "<enum-entries-accessor>");
        assert_eq!(entries.descriptor, "()Lkotlin/enums/EnumEntries;");
    }

    #[test]
    fn type_shapes_include_value_class_metadata() {
        let mut symbols = FrontendSymbols::default();
        let mut id = class("sample/Id");
        id.value_field = Some(("raw".to_string(), Ty::Int));
        symbols.classes.insert(type_name("sample/Id"), id);

        let source = ModuleSymbols::new(&symbols);
        let shape = source.classifier(type_name("sample/Id")).unwrap();
        assert_eq!(shape.value_underlying, Some(Ty::Int));
        assert!(
            source
                .classifier(type_name("sample/Id"))
                .is_some_and(|class| class.value_underlying.is_some()),
            "the common SymbolSource query must recognize source value classes without a provider-specific fallback"
        );
    }

    #[test]
    fn companion_properties_are_members_of_the_companion_classifier_record() {
        let mut symbols = FrontendSymbols::default();
        let mut sample = class("sample/Sample");
        sample.companion_internal = Some(type_name("sample/Sample$Companion"));
        let mut companion = class("sample/Sample$Companion");
        companion.declared_props.insert(
            "maxValue".to_string(),
            crate::resolve::DeclaredPropertySig {
                ty: Ty::Int,
                storage_ty: None,
                visibility: Visibility::Public,
                is_const: false,
                getter_name: "getMaxValue".to_string(),
                setter_name: None,
                setter_visibility: None,
                has_custom_getter: false,
                is_open: false,
                context_params: Vec::new(),
            },
        );
        companion
            .props
            .push(("maxValue".to_string(), Ty::Int, false));
        symbols.insert_class(sample);
        symbols.insert_class(companion);

        let source = ModuleSymbols::new(&symbols);
        let companion = source
            .classifier(type_name("sample/Sample$Companion"))
            .expect("companion classifier");
        let properties = companion
            .declared_callables
            .get("maxValue")
            .cloned()
            .unwrap_or_default()
            .into_parts()
            .1;
        assert_eq!(properties.overloads.len(), 1);
        assert_eq!(properties.overloads[0].ty, Ty::Int);
        assert_eq!(
            properties.overloads[0].receiver,
            Some(Ty::obj("sample/Sample$Companion"))
        );
    }

    #[test]
    fn named_companion_keeps_its_declared_classifier_segment() {
        let mut symbols = FrontendSymbols::default();
        let mut sample = class("sample/Sample");
        sample.companion_internal = Some(type_name("sample/Sample$Factory"));
        symbols.insert_class(sample);
        symbols.insert_class(class("sample/Sample$Factory"));

        let source = ModuleSymbols::new(&symbols);
        let sample = source
            .classifier(type_name("sample/Sample"))
            .expect("outer classifier");
        assert_eq!(
            sample.companion_object,
            Some(("Factory".to_string(), type_name("sample/Sample$Factory")))
        );
    }

    #[test]
    fn classifier_reuses_the_shape_built_for_a_repeated_query() {
        let mut st = FrontendSymbols::default();
        st.insert_class(class("demo/Widget"));
        st.finish_module_mutation();
        let m = ModuleSymbols::new(&st);
        let first = m.classifier(type_name("demo/Widget")).unwrap();
        let second = m.classifier(type_name("demo/Widget")).unwrap();
        assert!(
            std::rc::Rc::ptr_eq(&first, &second),
            "repeated queries must not rebuild the class shape"
        );
    }

    #[test]
    fn symbols_reuses_the_record_built_for_a_repeated_query() {
        let mut st = FrontendSymbols::default();
        let mut twice = sig(vec![Ty::Int], Ty::Int);
        twice.source_file = Some(0);
        twice.source_decl = Some(crate::ast::DeclId(1));
        twice.package = "demo".into();
        st.funs.insert("twice".into(), vec![twice]);
        st.fn_facades_by_decl
            .insert((0, 1), type_name("demo/DemoKt"));
        st.finish_module_mutation();
        let m = ModuleSymbols::new(&st);
        let namespace = SymbolNamespace::Package(type_name("demo"));
        let first = m.symbols(namespace, "twice");
        let second = m.symbols(namespace, "twice");
        assert!(
            !first.is_empty(),
            "the test must exercise a positive memo entry"
        );
        assert!(
            std::rc::Rc::ptr_eq(&first, &second),
            "repeated queries must not rebuild the namespace record"
        );
    }

    #[test]
    fn symbol_caches_are_isolated_per_module() {
        let namespace = SymbolNamespace::Package(type_name("demo"));

        let mut first = FrontendSymbols::default();
        let mut only_here = sig(Vec::new(), Ty::Int);
        only_here.source_file = Some(0);
        only_here.source_decl = Some(crate::ast::DeclId(1));
        only_here.package = "demo".into();
        first.funs.insert("onlyHere".into(), vec![only_here]);
        first
            .fn_facades_by_decl
            .insert((0, 1), type_name("demo/FirstKt"));
        first.finish_module_mutation();

        let second = FrontendSymbols::default();
        second.finish_module_mutation();

        let present = ModuleSymbols::new(&first).symbols(namespace, "onlyHere");
        let absent = ModuleSymbols::new(&second).symbols(namespace, "onlyHere");
        assert!(!present.is_empty());
        assert!(absent.is_empty());
        assert_eq!(first.module_symbol_cache.borrow().len(), 1);
        assert_eq!(second.module_symbol_cache.borrow().len(), 1);
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
    fn classifier_record_returns_exact_declarations_without_inheritance() {
        let mut st = FrontendSymbols::default();
        let mut base = class("demo/Base");
        base.methods
            .insert("greet".into(), vec![sig(vec![], Ty::String)]);
        let mut sub = class("demo/Sub");
        sub.super_internal = Some(crate::types::type_name("demo/Base"));
        sub.methods.insert("own".into(), vec![sig(vec![], Ty::Int)]);
        st.insert_class(base);
        st.insert_class(sub);
        let m = ModuleSymbols::new(&st);

        // `own` is on Sub itself (rung 0).
        let own = declared(&m, Ty::obj("demo/Sub"), "own").into_parts().0;
        assert_eq!(own.overloads.len(), 1);
        assert_eq!(own.overloads[0].kind, FnKind::Member);
        assert_eq!(own.overloads[0].receiver_rank, 0);

        // Providers never manufacture inherited declarations.
        assert!(declared(&m, Ty::obj("demo/Sub"), "greet")
            .into_parts()
            .0
            .overloads
            .is_empty());

        // The declaration is available exactly once from its owner. Core resolution assigns its MRO
        // rank when it walks from Sub to Base.
        let greet = declared(&m, Ty::obj("demo/Base"), "greet").into_parts().0;
        assert_eq!(greet.overloads.len(), 1);
        assert_eq!(greet.overloads[0].receiver_rank, 0);
        assert!(greet.overloads[0].callable.owner.matches("demo/Base"));
    }

    #[test]
    fn member_properties_preserve_declaration_metadata() {
        let mut symbols = FrontendSymbols::default();
        let mut base = class("demo/Base");
        base.declared_props.insert(
            "state".into(),
            FrontendDeclaredPropertySig {
                ty: Ty::String,
                storage_ty: None,
                visibility: Visibility::Protected,
                is_const: false,
                getter_name: "getState".into(),
                setter_name: Some("setState".into()),
                setter_visibility: Some(Visibility::Protected),
                has_custom_getter: false,
                is_open: false,
                context_params: Vec::new(),
            },
        );
        let mut sub = class("demo/Sub");
        sub.super_internal = Some(type_name("demo/Base"));
        symbols.insert_class(base);
        symbols.insert_class(sub);
        let source = ModuleSymbols::new(&symbols);

        assert!(declared(&source, Ty::obj("demo/Sub"), "state")
            .into_parts()
            .1
            .overloads
            .is_empty());

        let properties = declared(&source, Ty::obj("demo/Base"), "state")
            .into_parts()
            .1;
        assert_eq!(properties.overloads.len(), 1);
        let property = &properties.overloads[0];
        assert_eq!(property.kind, PropKind::Member);
        assert_eq!(property.receiver, Some(Ty::obj("demo/Base")));
        assert_eq!(property.ty, Ty::String);
        assert_eq!(property.visibility, Visibility::Protected);
        assert_eq!(property.receiver_rank, 0);
        assert!(property.owner.matches("demo/Base"));
        assert_eq!(
            property
                .setter
                .as_ref()
                .map(|setter| setter.params.as_slice()),
            Some([Ty::String].as_slice())
        );

        let getter = declared(&source, Ty::obj("demo/Base"), "getState")
            .into_parts()
            .0;
        assert_eq!(getter.overloads.len(), 1);
        assert_eq!(getter.overloads[0].receiver_rank, 0);
        assert_eq!(getter.overloads[0].visibility, Visibility::Protected);
        assert!(getter.overloads[0].callable.owner.matches("demo/Base"));
        assert!(matches!(
            getter.overloads[0].callable.origin,
            Origin::Module { .. }
        ));
        assert!(!declared(&source, Ty::obj("demo/Base"), "state")
            .into_parts()
            .1
            .overloads
            .is_empty());
    }

    #[test]
    fn member_inline_flag_flows_through_module_symbols() {
        let mut st = FrontendSymbols::default();
        let mut c = class("demo/Host");
        let mut method = sig(vec![Ty::Int], Ty::Int);
        method.set_is_inline(true);
        c.methods.insert("apply".into(), vec![method]);
        st.insert_class(c);
        let m = ModuleSymbols::new(&st);

        let members = m.instance_members(Ty::obj("demo/Host"), "apply");
        assert_eq!(members.len(), 1);
        assert!(members[0].inline.can_inline());

        let overloads = declared(&m, Ty::obj("demo/Host"), "apply").into_parts().0;
        assert_eq!(overloads.overloads.len(), 1);
        assert!(overloads.overloads[0].flags.inline.can_inline());
        assert!(overloads.overloads[0].callable.inline.can_inline());
    }

    #[test]
    fn required_splice_flows_as_the_generic_inline_capability() {
        let mut symbols = FrontendSymbols::default();
        let receiver = Ty::obj("demo/Receiver");
        let mut signature = sig(vec![Ty::Int], Ty::Boolean);
        // An emitted method and a legal direct fallback are independent capabilities. Reified
        // source declarations use this signature bit even when their facade exists, and all module
        // callable projections must preserve it as the shared `MustInline` semantic state.
        signature.flags = signature
            .flags
            .with_is_inline(true)
            .with_requires_splice(true);
        symbols
            .ext_funs
            .entry("check".into())
            .or_default()
            .insert(receiver.extension_recv_key(), vec![signature]);
        let module = ModuleSymbols::new(&symbols);

        let functions = match module
            .symbols(SymbolNamespace::Package(TypeName::ROOT), "check")
            .callables
            .clone()
        {
            crate::libraries::Callables::Functions(functions) => functions.overloads,
            _ => Vec::new(),
        };
        assert_eq!(functions.len(), 1);
        assert!(functions[0].flags.inline.must_inline());
        assert!(functions[0].callable.inline.must_inline());
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
        let fs = match m
            .symbols(SymbolNamespace::Package(TypeName::ROOT), "shifted")
            .callables
            .clone()
        {
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
            return_policy: Default::default(),
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
            .symbols(SymbolNamespace::Package(TypeName::ROOT), "transform")
            .callables
            .clone()
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
                FrontendExtPropSig {
                    formals: Vec::new(),
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
                FrontendExtPropSig {
                    formals: Vec::new(),
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
            .symbols(SymbolNamespace::Package(type_name("one")), "label")
            .callables
            .clone()
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
                .symbols(SymbolNamespace::Package(type_name("one")), "label")
                .callables,
            crate::libraries::Callables::None
        ));
        let public = match ModuleSymbols::for_file(&symbols, 0)
            .symbols(SymbolNamespace::Package(type_name("two")), "label")
            .callables
            .clone()
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
        let fs = match m
            .symbols(SymbolNamespace::Package(TypeName::ROOT), "render")
            .callables
            .clone()
        {
            crate::libraries::Callables::Functions(f) => f.overloads,
            _ => Vec::new(),
        };
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].callable.params, vec![declared]);
        assert_eq!(fs[0].callable.source_receiver, Some(declared));
    }

    #[test]
    fn classifier_record_contains_constructor_and_members() {
        let mut st = FrontendSymbols::default();
        let mut c = class("demo/Point");
        c.ctor_params = vec![Ty::Int, Ty::Int];
        c.methods.insert("sum".into(), vec![sig(vec![], Ty::Int)]);
        c.interfaces = vec![crate::types::type_name("demo/Shape")].into();
        st.insert_class(c);
        let m = ModuleSymbols::new(&st);
        let t = m.classifier(type_name("demo/Point")).expect("shape");
        assert_eq!(t.constructors.len(), 1);
        assert_eq!(t.constructors[0].params, vec![Ty::Int, Ty::Int]);
        assert_eq!(t.members.len(), 1);
        assert_eq!(t.members[0].name, "sum");
        assert_eq!(t.members[0].ret, Ty::Int);
        assert_eq!(
            t.supertypes.to_vec(),
            vec!["demo/Shape".to_string(), "kotlin/Any".to_string()]
        );
        assert!(m.classifier(type_name("demo/Nope")).is_none());
    }

    #[test]
    fn classifier_record_publishes_implicit_any_supertype() {
        let mut symbols = FrontendSymbols::default();
        symbols.insert_class(class("demo/Plain"));

        let source = ModuleSymbols::new(&symbols);
        let classifier = source
            .classifier(type_name("demo/Plain"))
            .expect("source classifier");

        assert_eq!(classifier.supertypes.to_vec(), vec!["kotlin/Any"]);
        assert_eq!(
            classifier.supertype_templates,
            vec![Ty::obj_name(crate::types::wk::any())]
        );
    }

    #[test]
    fn classifier_record_preserves_class_visibility() {
        let mut symbols = FrontendSymbols::default();
        let mut hidden = class("demo/Hidden");
        hidden.visibility = Visibility::Private;
        symbols.insert_class(hidden);

        assert!(!ModuleSymbols::new(&symbols)
            .classifier(type_name("demo/Hidden"))
            .expect("shape")
            .is_public());
    }

    #[test]
    fn selected_module_callable_preserves_interface_dispatch_capability() {
        // `FunctionInfo::member_with_return` is the common overload-selection round trip used by
        // module and dependency providers. Preserve the capability on the selected callable itself;
        // re-reading only module owners later would create an origin-specific path and would let a
        // provider-neutral resolver silently turn an interface member into virtual dispatch.
        let mut symbols = FrontendSymbols::default();
        let mut contract = class("demo/Contract");
        contract.flags = contract.flags.with_interface(true);
        contract
            .methods
            .insert("run".into(), vec![sig(vec![], Ty::Int)]);
        symbols.insert_class(contract);

        let source = ModuleSymbols::new(&symbols);
        let selected = declared(&source, Ty::obj("demo/Contract"), "run")
            .into_parts()
            .0
            .overloads
            .into_iter()
            .next()
            .expect("interface member overload");
        assert!(selected.callable.owner_is_interface);
        assert!(selected.member_with_return(Ty::Int).is_interface());
    }

    #[test]
    fn inheritance_shape_tracks_modality_and_callable_no_arg_constructors() {
        let mut st = FrontendSymbols::default();

        let mut defaulted = class("demo/Defaulted");
        defaulted.ctor_params = vec![Ty::Int];
        defaulted.ctor_defaults = vec![Some(CtorDefaultValue::Int(1))];
        st.insert_class(defaulted);

        let mut secondary = class("demo/Secondary");
        secondary.has_primary_ctor = false;
        secondary.ctor_params = vec![Ty::Int];
        secondary.ctor_defaults = vec![None];
        secondary.secondary_ctors = vec![vec![]];
        st.insert_class(secondary);

        let mut final_required = class("demo/FinalRequired");
        final_required.has_primary_ctor = false;
        final_required.ctor_params = vec![Ty::Int];
        final_required.ctor_defaults = vec![None];
        final_required.secondary_ctors = vec![vec![Ty::Int]];
        final_required.set_is_final(true);
        st.insert_class(final_required);

        let mut abstract_with_interface = class("demo/AbstractWithInterface");
        abstract_with_interface.set_is_abstract(true);
        abstract_with_interface.interfaces = vec![type_name("demo/RequiredInterface")].into();
        st.insert_class(abstract_with_interface);

        let mut sealed = class("demo/Sealed");
        sealed.set_is_abstract(true);
        sealed.set_is_sealed(true);
        st.insert_class(sealed);

        let source = ModuleSymbols::new(&st);
        let defaulted = source.classifier(type_name("demo/Defaulted")).unwrap();
        assert!(defaulted.inheritance.is_extensible);
        assert!(defaulted.inheritance.has_no_arg_constructor);

        let secondary = source.classifier(type_name("demo/Secondary")).unwrap();
        assert!(secondary.inheritance.is_extensible);
        assert!(secondary.inheritance.has_no_arg_constructor);

        let final_required = source.classifier(type_name("demo/FinalRequired")).unwrap();
        assert!(!final_required.inheritance.is_extensible);
        assert!(!final_required.inheritance.has_no_arg_constructor);

        let abstract_with_interface = source
            .classifier(type_name("demo/AbstractWithInterface"))
            .unwrap();
        assert!(
            !abstract_with_interface
                .inheritance
                .supports_external_subclassing
        );

        let sealed = source.classifier(type_name("demo/Sealed")).unwrap();
        assert!(!sealed.inheritance.supports_external_subclassing);
        assert!(!sealed.inheritance.has_no_arg_constructor);
    }
}
