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
#[cfg(test)]
use crate::types::Visibility;
use crate::types::{stored_value_ty, type_name, Ty, TypeName};
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

    pub(crate) fn type_alias_expansion(&self, identity: TypeName) -> Option<(Vec<String>, Ty)> {
        self.syms.source_alias_expansions.get(&identity).cloned()
    }

    pub(crate) fn type_parameter_extra_bounds(&self, identity: &str) -> Vec<Ty> {
        self.syms
            .classes
            .values()
            .find_map(|class| {
                let index = class
                    .type_params()
                    .iter()
                    .position(|parameter| parameter == identity)?;
                class.type_parameter_extra_bounds.get(index).cloned()
            })
            .unwrap_or_default()
    }

    pub(crate) fn annotation_retention(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::AnnotationRetention> {
        self.syms.annotation_retention(classifier)
    }

    pub(crate) fn annotation_targets(
        &self,
        classifier: TypeName,
    ) -> crate::types::AnnotationTargets {
        self.syms.annotation_targets(classifier)
    }

    /// Temporary Pass-1 graph access for inference-only checker operations. The streamed module
    /// provider has no corresponding operation and therefore cannot retain this graph.
    pub(crate) fn pass_one_symbols(&self) -> &'a FrontendSymbols {
        self.syms
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
    fn classifier_record(&self, internal: TypeName) -> Option<std::sync::Arc<LibraryType>> {
        if self.syms.module_cache_enabled() {
            if let Some(shape) = self.syms.module_shape_cache.borrow().get(&internal) {
                return shape.clone();
            }
        }
        let shape = self
            .class_by_type_name(internal)
            .map(|c| std::sync::Arc::new(self.type_shape_for(c)))
            .or_else(|| {
                let target = *self.syms.source_alias_fqns.get(&internal)?;
                let mut shape = self.type_shape_for(self.class_by_type_name(target)?);
                shape.alias_target = Some(target);
                Some(std::sync::Arc::new(shape))
            })
            // Signature collection seeds every source classifier identity before it walks any
            // declaration body. Surface that identity through the SAME namespace record as a fully
            // collected classifier; consumers that only need to walk a qualified name must not grow
            // a second, source-only existence query. A fresh ModuleSymbols is built for each early
            // inference, so this temporary header cannot outlive the immutable table snapshot.
            .or_else(|| {
                self.syms
                    .source_class_header_shape(internal)
                    .map(std::sync::Arc::new)
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
                // A member extension is an instance method carrying the extension receiver among its
                // method parameters, AFTER the leading context parameters. Publish the same shape as
                // metadata providers: contexts + receiver + values.
                let signature = extension.signature();
                member.params.insert(
                    signature.context_count.min(member.params.len()),
                    extension.receiver_ty(),
                );
                member.set_is_member_extension(true);
                member.set_is_operator(extension.signature().is_operator());
                member.set_is_infix(extension.signature().is_infix());
                members.push(member);
            }
        }
        let companion = Vec::new();
        // The primary constructor (+ secondaries) as `<init>` members returning Unit.
        let mut constructors = Vec::new();
        let constructor_generic_signature = |params: Vec<Ty>| {
            (!c.type_params.is_empty()).then(|| GenericSig {
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
                params,
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
            })
        };
        let exposes_constructors = !c.is_interface()
            && !c.is_object()
            && self.syms.enum_entries_of(c.internal_name()).is_none();
        if exposes_constructors && c.has_primary_ctor {
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
            constructor.generic_sig = constructor_generic_signature(
                c.ctor_param_shapes
                    .iter()
                    .map(|(parameter, _)| *parameter)
                    .collect(),
            );
            constructors.push(constructor);
        }
        for (index, params) in c
            .secondary_ctors
            .iter()
            .enumerate()
            .filter(|_| exposes_constructors)
        {
            let mut constructor = LibraryMember::new(
                "<init>".to_string(),
                params.clone(),
                Ty::Unit,
                String::new(),
            );
            if let Some(call_sig) = c.secondary_ctor_call_sigs.get(index) {
                constructor.call_sig = call_sig.clone();
            }
            constructor.generic_sig = constructor_generic_signature(
                c.secondary_ctor_shapes
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| params.clone()),
            );
            constructors.push(constructor);
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
        let named_parameter_lists = (c.has_primary_ctor
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
            annotation: None,
        })
        .into_iter()
        .collect();
        let mut shape = LibraryType {
            // A current-module classifier is by definition a Kotlin declaration.
            is_kotlin: true,
            access: c.visibility.into(),
            source_file: Some(c.source_file),
            stable_declaration: c.stable_declaration,
            is_nested: c.internal_name().contains("$"),
            outer_instance: c.inner_of,
            kind,
            inheritance: crate::libraries::ClassifierInheritance {
                is_abstract: c.is_abstract() || c.is_interface(),
                is_extensible: !c.is_interface() && !c.is_final(),
                has_no_arg_constructor: !c.is_sealed() && c.has_no_arg_constructor(),
            },
            supertypes: supertypes.into(),
            supertype_templates,
            constructors,
            hidden_member_properties: Default::default(),
            declared_callables: HashMap::new(),
            declared_callable_order: Vec::new(),
            members,
            companion,
            // A completed source classifier is also the provider record consumed by later
            // modules. Preserve the Pass-1 constant payload here: annotation folding in a
            // dependent module must read the selected semantic constant from the provider rather
            // than reopen the dependency's source initializer.
            constants: c.constants.clone(),
            sam_eligible: c.is_fun_interface(),
            callable_signature: c.callable_signature,
            callable_signatures: c.callable_signatures.clone(),
            // Publish the source companion through the same classifier record as a dependency
            // companion. Core can then treat `Type(args)` → companion `operator fun invoke` as one
            // callable-tower case instead of retaining a source-only retry path.
            companion_object: c
                .companion_internal
                .map(|companion| (companion.nested_segment_ref().to_string(), companion)),
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
                    .enumerate()
                    .map(|(index, bound)| {
                        let mut bounds = (*bound != Ty::Error)
                            .then_some(vec![*bound])
                            .unwrap_or_default();
                        bounds.extend(
                            c.type_parameter_extra_bounds
                                .get(index)
                                .into_iter()
                                .flatten()
                                .copied(),
                        );
                        bounds
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
            own_type_parameter_count: c.type_params.len(),
            sealed_subclasses,
            enum_entries,
            enum_entries_accessor,
            named_parameter_lists,
            retention: None,
            annotation_targets: None,
        };
        debug_assert!(
            c.methods
                .keys()
                .chain(c.member_ext_funs.keys())
                .chain(c.member_ext_props.keys())
                .chain(
                    c.declared_props
                        .iter()
                        .filter(|(_, property)| property.source_visible)
                        .map(|(name, _)| name),
                )
                .chain(c.contextual_props.keys())
                .all(|name| c.declared_callable_order.contains(name)),
            "the source provider must publish every direct callable in lexical order",
        );
        for name in &c.declared_callable_order {
            let declarations = self.declared_callables_for(c, name);
            if !matches!(declarations, crate::libraries::Callables::None) {
                shape.insert_declared_callables(name.clone(), declarations);
            }
        }
        shape
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
        // An OVERRIDE hides what it overrides. The walk pushes a class's own members before its
        // supertypes', so the first entry for a given value-parameter shape is the nearest
        // declaration; keeping the overridden one too lets a caller read the BASE declaration's
        // return type, which is wrong whenever the override is covariant
        // (`override fun bar(): String` over `open fun bar(): Any` — `bar().length` then failed to
        // resolve). Overloads differ in their parameter shape and are all retained.
        let mut shapes = Vec::new();
        out.retain(|member| {
            let shape = member
                .params
                .iter()
                .map(|parameter| parameter.non_null().erased_recv())
                .collect::<Vec<_>>();
            if shapes.contains(&shape) {
                return false;
            }
            shapes.push(shape);
            true
        });
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
        let declared_receiver = Ty::obj_args_name(
            internal,
            &class
                .type_params
                .iter()
                .zip(&class.type_param_bounds)
                .chain(
                    class
                        .captured_type_parameters
                        .type_params
                        .iter()
                        .zip(&class.captured_type_parameters.type_param_bounds),
                )
                .map(|(name, bound)| Ty::ty_param(name, *bound))
                .collect::<Vec<_>>(),
        );
        let mut functions = class
            .methods_named(name)
            .iter()
            .map(|signature| {
                let mut function = fn_info(
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
                );
                if let Some(generic) = function.generic_sig.as_mut() {
                    generic.receiver.get_or_insert(declared_receiver);
                    function.callable.generic_sig = Some(Box::new(generic.clone()));
                }
                function
            })
            .collect::<Vec<_>>();
        functions.extend(class.member_ext_funs(name).iter().map(|declaration| {
            let mut function = fn_info(
                FnKind::Extension,
                declaration.signature(),
                Some(declaration.receiver_ty()),
                CallableOwner {
                    internal,
                    is_interface: class.is_interface(),
                },
                name,
                0,
                Origin::Module { facade: internal },
            );
            function.source_key = None;
            function
        }));
        let mut properties = class
            .declared_props
            .get(name)
            .filter(|property| property.source_visible)
            .map(|property| {
                let mut declaration = source_property(
                    internal,
                    name,
                    property,
                    class.is_interface() || class.is_annotation(),
                    0,
                );
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
                    .or_else(|| {
                        class.nullable_tparam_props.get(name).and_then(|&index| {
                            let parameter = class.type_params.get(index)?;
                            let bound = class
                                .type_param_bounds
                                .get(index)
                                .copied()
                                .filter(|bound| *bound != Ty::Error)
                                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                            Some(Ty::nullable(Ty::ty_param(parameter, bound)))
                        })
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
        properties.extend(class.contextual_props(name).iter().map(|property| {
            source_property(
                internal,
                name,
                property,
                class.is_interface() || class.is_annotation(),
                0,
            )
        }));
        properties.extend(class.member_ext_props(name).iter().map(|declaration| {
            let mut getter_params = vec![declaration.receiver_ty()];
            getter_params.extend_from_slice(declaration.context_params());
            let mut getter = source_property_getter(
                internal,
                crate::names::property_getter_name(name),
                getter_params.clone(),
                declaration.ret(),
                class.is_interface(),
            );
            getter.source_receiver = Some(declaration.receiver_ty());
            getter.context_count = declaration.context_params().len();
            let setter = declaration.is_var().then(|| {
                let mut params = getter_params;
                params.push(stored_value_ty(declaration.ret()));
                let mut setter = source_callable(
                    internal,
                    crate::names::property_setter_name(name),
                    params,
                    Ty::Unit,
                    class.is_interface(),
                );
                setter.source_receiver = Some(declaration.receiver_ty());
                setter.context_count = declaration.context_params().len();
                setter
            });
            PropertyInfo {
                name: name.to_string(),
                kind: PropKind::MemberExtension,
                receiver: Some(declaration.receiver_ty()),
                formals: declaration.type_params().to_vec(),
                ty: declaration.ret(),
                context_count: declaration.context_params().len(),
                context_param_names: Vec::new(),
                getter,
                setter,
                setter_visibility: declaration
                    .setter_visibility()
                    .unwrap_or_else(|| declaration.visibility()),
                is_const: false,
                compile_time_constant: None,
                visibility: declaration.visibility(),
                owner: internal,
                receiver_rank: 0,
                source_key: None,
                stable_declaration: declaration.stable_declaration(),
                getter_declaration: None,
                setter_declaration: None,
                source_member: declaration.source_member(),
                accessor_derived: false,
            }
        }));
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
    m.set_is_final(sig.is_final());
    m.set_suspend(sig.is_suspend());
    m.visibility = sig.visibility;
    m.inline = crate::libraries::InlineKind::from_flags(sig.is_inline(), sig.requires_splice());
    m.reified = sig.has_reified_type_params();
    m.call_sig = sig.call_sig();
    m.context_count = sig.context_count;
    m.equality_bound = sig.equality_bound;
    m.default_values = sig.param_default_values.clone();
    m.plugin_expression = sig.plugin_expression;
    m.stable_declaration = sig.stable_declaration;
    m.source_member = sig.source_member;
    m
}

/// Build a top-level / extension `FunctionInfo` from a user [`Signature`]. `receiver` is `Some` for an
/// extension, spliced into `params` at the library convention's receiver index — after the leading
/// context parameters (see [`crate::libraries::extension_receiver_index`]), so `params[0]` is the
/// receiver only for an extension that declares no `context(…)` clause.
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
    // The extension receiver follows the leading CONTEXT parameters, matching kotlinc's signature
    // layout `(contexts…, receiver, values…)`; `sig.params` is `(contexts…, values…)`.
    let mut params: Vec<Ty> = sig.params.clone();
    if let Some(r) = receiver {
        params.insert(sig.context_count.min(params.len()), r);
    }
    let declared_params = Some(params.clone().into_boxed_slice());
    let callable = LibraryCallable {
        external_identity: None,
        external_property_identity: None,
        owner,
        name: name.to_string(),
        reflection_name: Some(name.to_string()),
        compiler_intrinsic: None,
        inline_body_plan: None,
        plugin_expression: sig.plugin_expression,
        descriptor: String::new(),
        physical_params: if sig.is_companion_extension() {
            sig.params.clone()
        } else {
            params.clone()
        },
        params,
        ret: sig.ret,
        physical_ret: sig.ret,
        suspend: sig.is_suspend(),
        is_abstract: sig.is_abstract(),
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
        declared_params,
        context_count: sig.context_count,
        contract: sig.contract.clone(),
        equality_bound: sig.equality_bound,
        generic_sig: sig.generic_sig.clone().map(Box::new),
        singleton_dispatch: None,
        default_realization: None,
        constructor_realization: None,
        // A SOURCE callable's `ret` is already the declared type and its `physical_ret` is not yet
        // erased, so there is no carrier-vs-box question for the value-class pass to answer here — it
        // sees the declaration itself. The fact exists for callables read back from a class file.
        declared_ret: None,
    };
    FunctionInfo {
        companion_extension: sig.is_companion_extension(),
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
        stable_declaration: sig.stable_declaration,
        source_member: sig.source_member,
        flags: FnFlags {
            inline: InlineKind::from_flags(sig.is_inline(), sig.requires_splice()),
            reified: sig.has_reified_type_params(),
            // Same-file `suspend fun` — flows from the AST via `Signature.is_suspend` so the resolver
            // reports suspend-ness uniformly with classpath callees (whose flag comes from @Metadata).
            suspend: sig.is_suspend(),
            operator: sig.is_operator(),
            infix: sig.is_infix(),
            is_abstract: sig.is_abstract(),
            is_final: sig.is_final(),
        },
        visibility: sig.visibility,
        annotations: sig.annotations.clone(),
        ..FunctionInfo::plain(kind, receiver, callable)
    }
}

/// Normalize a transient source member declaration into the same candidate shape exposed by the
/// module symbol provider. Body-local classifiers use this while their inferred member results are
/// being checked in the active Pass-2 unit; the candidate and its parser coordinate are discarded
/// with that unit, while checked FIR retains only `stable_declaration`.
pub(crate) fn source_member_function(
    name: &str,
    signature: &Signature,
    receiver: Option<Ty>,
    owner: TypeName,
    owner_is_interface: bool,
) -> FunctionInfo {
    fn_info(
        if receiver.is_some() {
            FnKind::Extension
        } else {
            FnKind::Member
        },
        signature,
        receiver,
        CallableOwner {
            internal: owner,
            is_interface: owner_is_interface,
        },
        name,
        0,
        Origin::Module { facade: owner },
    )
}

fn source_callable(
    owner: TypeName,
    name: String,
    params: Vec<Ty>,
    ret: Ty,
    owner_is_interface: bool,
) -> LibraryCallable {
    let declared_params = Some(params.clone().into_boxed_slice());
    LibraryCallable {
        external_identity: None,
        external_property_identity: None,
        owner,
        reflection_name: Some(name.clone()),
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
        is_abstract: false,
        owner_is_interface,
        member_realization: crate::libraries::MemberRealization::Dispatch,
        inline: InlineKind::None,
        default_call: false,
        vararg_elem: None,
        vararg_index: None,
        signature: None,
        origin: Origin::Module { facade: owner },
        source_receiver: None,
        declared_params,
        context_count: 0,
        contract: None,
        equality_bound: None,
        generic_sig: None,
        singleton_dispatch: None,
        default_realization: None,
        constructor_realization: None,
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

fn source_property(
    owner: TypeName,
    name: &str,
    property: &FrontendDeclaredPropertySig,
    owner_is_interface: bool,
    receiver_rank: u32,
) -> PropertyInfo {
    let mut getter = source_property_getter(
        owner,
        property.getter_name.clone(),
        property.context_params.clone(),
        property.ty,
        owner_is_interface,
    );
    getter.is_abstract = property.is_abstract;
    let setter = property.setter_name.as_ref().map(|setter| {
        let mut params = property.context_params.clone();
        params.push(stored_value_ty(property.ty));
        let mut setter =
            source_callable(owner, setter.clone(), params, Ty::Unit, owner_is_interface);
        setter.is_abstract = property.is_abstract;
        setter
    });
    PropertyInfo {
        name: name.to_string(),
        kind: PropKind::Member,
        receiver: Some(Ty::obj_name(owner)),
        formals: Vec::new(),
        ty: property.ty,
        context_count: property.context_params.len(),
        context_param_names: Vec::new(),
        getter,
        setter,
        setter_visibility: property.setter_visibility.unwrap_or(property.visibility),
        is_const: property.is_const,
        compile_time_constant: None,
        visibility: property.visibility,
        owner,
        receiver_rank,
        source_key: None,
        stable_declaration: property.stable_declaration,
        getter_declaration: None,
        setter_declaration: None,
        source_member: property.source_member,
        accessor_derived: false,
    }
}

impl SymbolSource for ModuleSymbols<'_> {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        crate::types::existing_type_name_child(parent, name)
            .is_some_and(|package| self.syms.source_packages.contains(&package))
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
        let associated_owner = match namespace {
            SymbolNamespace::Classifier(owner) => Some(owner),
            SymbolNamespace::Package(_) => None,
        };
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
            // `ext_funs` groups overloads by receiver in a hash map; surface them in DECLARATION
            // order — kotlinc's first-declared rule at one scope level — not hash order.
            let mut declared: Vec<(&Ty, &Signature)> = families
                .iter()
                .flat_map(|(recv, sigs)| sigs.iter().map(move |sig| (recv, sig)))
                .collect();
            declared.sort_by_key(|(_, signature)| {
                (
                    signature.source_file.unwrap_or(u32::MAX),
                    signature
                        .source_decl
                        .map_or(u32::MAX, |declaration| declaration.0),
                )
            });
            for (recv, sig) in declared {
                let imported_associated = associated_owner.is_some_and(|owner| {
                    sig.is_companion_extension() && recv.non_null().obj_internal() == Some(owner)
                });
                let rank = if recv.non_null().is_ty_param() || recv.non_null() == any {
                    1
                } else {
                    0
                };
                // Surface EVERY overload registered for this (receiver, name) so the resolver's
                // overload picker can choose by arity/argument types (`fun R.f()` vs `fun R.f(x)`).
                if !package.is_some_and(|package| package.matches(&sig.package))
                    && !imported_associated
                {
                    continue;
                }
                overloads.push(fn_info(
                    if imported_associated {
                        FnKind::TopLevel
                    } else {
                        FnKind::Extension
                    },
                    sig,
                    (!imported_associated).then_some(*recv),
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
        let mut properties = Vec::new();
        // `import Owner.member` makes an object's ordinary member a receiver-less callable in the
        // import scope; dispatch still targets the singleton. Normalize it here, at the provider
        // boundary, so the checker sees the same `FunctionInfo` shape as a dependency object member
        // and ordinary overload selection remains origin-neutral.
        if let SymbolNamespace::Classifier(owner) = namespace {
            if let Some(classifier) = self.classifier_record(owner).filter(|ty| ty.is_object()) {
                let singleton = crate::libraries::SingletonDispatch { classifier: owner };
                let imported = classifier
                    .declared_callables
                    .get(&name)
                    .cloned()
                    .unwrap_or_default();
                let (imported_functions, imported_properties) = imported.into_parts();
                for mut function in imported_functions.overloads {
                    if function.kind == FnKind::Member {
                        function.kind = FnKind::TopLevel;
                        function.receiver = None;
                    }
                    function.source_key = None;
                    function.callable.singleton_dispatch = Some(Box::new(singleton.clone()));
                    overloads.push(function);
                }
                for mut property in imported_properties.overloads {
                    match property.kind {
                        PropKind::Member => {
                            property.kind = PropKind::TopLevel;
                            property.receiver = None;
                        }
                        PropKind::MemberExtension => property.kind = PropKind::Extension,
                        PropKind::Extension | PropKind::TopLevel => {}
                    }
                    property.getter.singleton_dispatch = Some(Box::new(singleton.clone()));
                    if let Some(setter) = &mut property.setter {
                        setter.singleton_dispatch = Some(Box::new(singleton.clone()));
                    }
                    properties.push(property);
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
            // An explicit backing field is a stable source-level smart-cast only while this
            // compilation can see the final property's declaration. The accessor ABI and metadata
            // remain nominal (`property.ty`); only the selected read expression gets the narrower
            // field type.
            let read_ty = (self.source_file == Some(source.0))
                .then_some(property.storage_ty)
                .flatten()
                .filter(|ty| !ty.mentions_pending() && !ty.mentions_error())
                .unwrap_or(property.ty);
            properties.push(PropertyInfo {
                name: name.clone(),
                kind: PropKind::TopLevel,
                receiver: None,
                formals: property.formals.clone(),
                ty: read_ty,
                context_count: property.context_params.len(),
                context_param_names: property.context_param_names.clone(),
                getter,
                setter,
                setter_visibility: property.setter_visibility,
                is_const: property.is_const,
                compile_time_constant: property.compile_time_constant.clone(),
                visibility: property.visibility,
                owner,
                receiver_rank: 0,
                source_key: Some(source),
                stable_declaration: property.stable_declaration,
                getter_declaration: None,
                setter_declaration: None,
                source_member: None,
                accessor_derived: false,
            });
        }
        for ((_, property_name), signatures) in &self.syms.ext_props {
            if property_name != &name {
                continue;
            }
            for property in signatures {
                let imported_associated = associated_owner.is_some_and(|owner| {
                    property.is_companion_extension
                        && property.receiver.non_null().obj_internal() == Some(owner)
                });
                if (!package.is_some_and(|package| package.matches(&property.package))
                    && !imported_associated)
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
                let mut getter = getter;
                let mut setter = setter;
                if let Some(generic) = property.generic_signature() {
                    getter.generic_sig = Some(Box::new(generic.clone()));
                    if let Some(setter) = &mut setter {
                        let mut setter_generic = generic;
                        setter_generic.params.push(property.ty);
                        setter_generic.ret = Ty::Unit;
                        setter.generic_sig = Some(Box::new(setter_generic));
                    }
                }
                if property.is_companion_extension {
                    if !getter.physical_params.is_empty() {
                        getter.physical_params.remove(0);
                    }
                    if let Some(setter) = &mut setter {
                        if !setter.physical_params.is_empty() {
                            setter.physical_params.remove(0);
                        }
                    }
                }
                if imported_associated {
                    if !getter.params.is_empty() {
                        getter.params.remove(0);
                    }
                    if let Some(setter) = &mut setter {
                        if !setter.params.is_empty() {
                            setter.params.remove(0);
                        }
                    }
                }
                properties.push(PropertyInfo {
                    name: property_name.clone(),
                    kind: if imported_associated {
                        PropKind::TopLevel
                    } else {
                        PropKind::Extension
                    },
                    receiver: (!imported_associated).then_some(property.receiver),
                    formals: property.formals.clone(),
                    ty: property.ty,
                    context_count: property.context_params.len(),
                    context_param_names: Vec::new(),
                    getter,
                    setter,
                    setter_visibility: property.visibility,
                    is_const: false,
                    compile_time_constant: None,
                    visibility: property.visibility,
                    owner,
                    receiver_rank: 0,
                    source_key: Some(property.source),
                    stable_declaration: property.stable_declaration,
                    getter_declaration: None,
                    setter_declaration: None,
                    source_member: None,
                    accessor_derived: false,
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
        let importable_declaration = match namespace {
            SymbolNamespace::Package(package) => {
                crate::types::existing_type_name_child(package, &name)
                    .is_some_and(|identity| self.syms.source_alias_fqns.contains_key(&identity))
            }
            SymbolNamespace::Classifier(owner) => {
                self.classifier_record(owner).is_some_and(|classifier| {
                    classifier.is_enum_entry(&name)
                        || classifier.constants.contains_key(&name)
                        || classifier
                            .companion_object
                            .as_ref()
                            .is_some_and(|(field, _)| field == &name)
                })
            }
        };
        let record = std::rc::Rc::new(ResolvedSymbols {
            classifier_name,
            classifier,
            callables,
            importable_declaration,
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
            annotations: Vec::new(),
            equality_bound: None,
            vararg_index: None,
            required: 0,
            param_defaults: vec![],
            exact_params: vec![],
            no_infer_params: vec![],
            implicit_integer_coercion: vec![],
            param_default_values: vec![],
            param_names: vec![],
            lambda_param_types: vec![],
            lambda_recv: vec![],
            visibility: crate::types::Visibility::Public,
            context_count: 0,
            source_decl: None,
            stable_declaration: None,
            source_file: None,
            source_member: None,
            source_receiver: None,
            package: String::new(),
            contract: None,
            plugin_expression: None,
        }
    }

    fn class(internal: &str) -> FrontendClassSig {
        FrontendClassSig {
            internal: internal.into(),
            stable_declaration: None,
            source_file: 0,
            source_decl: None,
            visibility: Visibility::Public,
            annotations: vec![],
            props: vec![],
            declared_props: HashMap::new(),
            contextual_props: HashMap::new(),
            constants: HashMap::new(),
            member_ext_props: HashMap::new(),
            member_ext_funs: HashMap::new(),
            has_primary_ctor: true,
            primary_constructor_declaration: None,
            primary_constructor_annotations: vec![],
            ctor_params: vec![],
            ctor_param_shapes: vec![],
            ctor_param_names: vec![],
            ctor_implicit_integer_coercion: vec![],
            ctor_vararg: None,
            methods: HashMap::new(),
            declared_callable_order: Vec::new(),
            source_methods: Vec::new(),
            flags: FrontendClassFlags::default(),
            inner_of: None,
            companion_internal: None,
            lateinit_props: HashSet::new(),
            interfaces: crate::types::TypeNameList::new(),
            interface_type_args: Vec::new(),
            delegated_interfaces: Vec::new(),
            callable_signature: None,
            callable_signatures: Vec::new(),
            super_internal: None,
            super_type_args: Vec::new(),
            super_ctor_params: Vec::new(),
            ctor_defaults: vec![],
            secondary_ctors: vec![],
            secondary_ctor_shapes: vec![],
            secondary_ctor_call_sigs: vec![],
            secondary_constructor_declarations: vec![],
            secondary_constructor_annotations: vec![],
            type_parameters: crate::types::TypeParameters::default(),
            type_parameter_extra_bounds: Vec::new(),
            captured_type_parameters: crate::types::TypeParameters::default(),
            metadata_captured_type_parameters: Vec::new(),
            generic_props: HashMap::new(),
            nullable_tparam_props: HashMap::new(),
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
    fn type_shapes_publish_source_constant_payloads_to_dependent_modules() {
        let mut symbols = FrontendSymbols::default();
        let mut object = class("sample/Class$Obj");
        object.flags = object.flags.with_object(true);
        object.constants.insert(
            "Const".to_string(),
            crate::libraries::LibraryConst {
                ty: Ty::String,
                value: crate::libraries::LibConst::Str("const".into()),
            },
        );
        symbols.insert_class(object);

        let source = ModuleSymbols::new(&symbols);
        let shape = source
            .classifier(type_name("sample/Class$Obj"))
            .expect("nested object classifier");
        assert_eq!(
            shape.constants.get("Const"),
            Some(&crate::libraries::LibraryConst {
                ty: Ty::String,
                value: crate::libraries::LibConst::Str("const".into()),
            })
        );
    }

    #[test]
    fn companion_properties_are_members_of_the_companion_classifier_record() {
        let mut symbols = FrontendSymbols::default();
        let mut sample = class("sample/Sample");
        sample.companion_internal = Some(type_name("sample/Sample$Companion"));
        let mut companion = class("sample/Sample$Companion");
        companion
            .declared_callable_order
            .push("maxValue".to_string());
        companion.declared_props.insert(
            "maxValue".to_string(),
            crate::resolve::DeclaredPropertySig {
                ty: Ty::Int,
                storage_ty: None,
                visibility: Visibility::Public,
                source_visible: true,
                is_const: false,
                annotations: Vec::new(),
                getter_name: "getMaxValue".to_string(),
                setter_name: None,
                setter_visibility: None,
                has_custom_getter: false,
                is_abstract: false,
                is_open: false,
                context_params: Vec::new(),
                source_member: None,
                stable_declaration: None,
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
            std::sync::Arc::ptr_eq(&first, &second),
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
    fn inserted_classifier_updates_the_module_package_namespace() {
        let mut symbols = FrontendSymbols::default();
        symbols.finish_module_mutation();

        let root = type_name("");
        let _demo = type_name("demo");
        assert!(!ModuleSymbols::new(&symbols).package_exists(root, "demo"));

        symbols.insert_class(class("demo/Widget"));

        assert!(
            ModuleSymbols::new(&symbols).package_exists(root, "demo"),
            "inserting a classifier must update the explicit package namespace even after a prior lookup"
        );
    }

    #[test]
    fn mutation_phase_package_lookup_does_not_scan_declarations() {
        let mut symbols = FrontendSymbols::default();
        for index in 0..4_096 {
            symbols.insert_class(class(&format!("package{index}/Widget")));
        }
        // Keep the module in its default mutation phase: this is where the symbol/shape caches are
        // intentionally disabled and the former derived package cache rebuilt all declarations for
        // every lookup. The generous ceiling separates a few thousand hash lookups from the old
        // tens of millions of package-set insertions without making normal machine variance relevant.
        let module = ModuleSymbols::new(&symbols);
        let root = type_name("");
        let started = std::time::Instant::now();
        for _ in 0..4_096 {
            assert!(std::hint::black_box(
                module.package_exists(root, "package0")
            ));
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "mutation-phase package lookup scanned declarations: {elapsed:?}"
        );
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
        base.declared_callable_order.push("greet".to_string());
        base.methods
            .insert("greet".into(), vec![sig(vec![], Ty::String)]);
        let mut sub = class("demo/Sub");
        sub.super_internal = Some(crate::types::type_name("demo/Base"));
        sub.declared_callable_order.push("own".to_string());
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
        base.declared_callable_order.push("state".to_string());
        base.declared_props.insert(
            "state".into(),
            FrontendDeclaredPropertySig {
                ty: Ty::String,
                storage_ty: None,
                visibility: Visibility::Protected,
                source_visible: true,
                is_const: false,
                annotations: Vec::new(),
                getter_name: "getState".into(),
                setter_name: Some("setState".into()),
                setter_visibility: Some(Visibility::Protected),
                has_custom_getter: false,
                is_abstract: false,
                is_open: false,
                context_params: Vec::new(),
                source_member: None,
                stable_declaration: None,
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
        assert!(getter.overloads.is_empty());
        assert_eq!(property.getter.name, "getState");
        assert!(property.getter.owner.matches("demo/Base"));
        assert!(matches!(property.getter.origin, Origin::Module { .. }));
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
        c.declared_callable_order.push("apply".to_string());
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
        // source declarations carry both facts even when their facade exists, and all module
        // callable projections must preserve them independently.
        signature.flags = signature
            .flags
            .with_is_inline(true)
            .with_requires_splice(true)
            .with_has_reified_type_params(true);
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
        assert!(functions[0].flags.reified);
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
                    formal_names: Vec::new(),
                    formals: Vec::new(),
                    formal_bounds: Vec::new(),
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
                    annotations: Vec::new(),
                    stable_declaration: None,
                    is_companion_extension: false,
                },
                FrontendExtPropSig {
                    formal_names: Vec::new(),
                    formals: Vec::new(),
                    formal_bounds: Vec::new(),
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
                    annotations: Vec::new(),
                    stable_declaration: None,
                    is_companion_extension: false,
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
        c.declared_callable_order.push("sum".to_string());
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
        contract.declared_callable_order.push("run".to_string());
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
        defaulted.ctor_param_names = vec![("value".to_string(), true)];
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
        assert!(abstract_with_interface.inheritance.is_abstract);

        let sealed = source.classifier(type_name("demo/Sealed")).unwrap();
        assert!(!sealed.inheritance.has_no_arg_constructor);
    }
}
