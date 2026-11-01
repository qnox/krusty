//! The functions a classpath classifier declares under one name, as the declaration surface
//! exposes them: its ordinary members, with a Java method a renamed builtin covers answering to the
//! Kotlin name, and separately its member extensions, each with its semantic extension receiver.

use super::*;

impl JvmLibraries {
    pub(super) fn member_functions(&self, receiver: Ty, name: &str) -> FunctionSet {
        let Some(classifier) = receiver.kotlin_class_internal() else {
            return FunctionSet::default();
        };
        let function_renames = self.mapped_collection_function_renames(classifier);
        self.member_functions_with_renames(receiver, name, &function_renames)
    }

    /// The member-extension functions named `name` that `receiver`'s classifier declares, each with
    /// its semantic extension receiver.
    pub(super) fn member_extension_declarations(
        &self,
        receiver: Ty,
        name: &str,
    ) -> Vec<FunctionInfo> {
        let Some(owner) = receiver.kotlin_class_internal() else {
            return Vec::new();
        };
        let Some(classifier) = self.classifier_record(owner) else {
            return Vec::new();
        };
        classifier
            .members
            .iter()
            .filter(|member| member.name == name && member.is_member_extension())
            .map(|member| {
                let extension_receiver = member
                    .semantic_extension_receiver()
                    .expect("a member-extension declaration publishes its semantic receiver");
                let mut function =
                    FunctionInfo::classifier_member(FnKind::Extension, owner, member.clone());
                function.receiver = Some(extension_receiver);
                function
            })
            .collect()
    }

    pub(super) fn member_functions_with_renames(
        &self,
        receiver: Ty,
        name: &str,
        function_renames: &[MappedBuiltinMember],
    ) -> FunctionSet {
        // Exact declarations on this classifier. The resolver assigns inheritance distance.
        let mut overloads = Vec::new();
        if let Some(cn) = receiver.kotlin_class_internal() {
            if let Some(t) = self.classifier_record(cn) {
                // Compute the provider-owned rename shapes once per hierarchy rung. A raw Java
                // method is renamed only when its physical name AND full erased descriptor match a
                // mapped interface obligation carried by this receiver's actual class hierarchy.
                // This keeps overload identity precise without a classpath-only reverse-name table.
                for m in &t.members {
                    // A member extension has TWO receivers: the declaring class supplies the implicit
                    // dispatch receiver and the method's first JVM parameter is the extension receiver.
                    // It is therefore not an ordinary member of `receiver`, even though both shapes are
                    // encoded as instance methods in the class file. The dedicated semantic
                    // member-extension query consumes this declaration; exposing it here as well would
                    // incorrectly accept `scope.invoke("x", body)` and create two resolution paths for
                    // the same declaration.
                    if m.is_member_extension() {
                        continue;
                    }
                    // The name this member is visible under in the receiver's SOURCE scope. Normally
                    // its own; for a Java method that a renamed builtin covers, the KOTLIN name
                    // INSTEAD of the JVM one — so `ArrayList.remove(int)` answers `removeAt` and is
                    // invisible to `remove`, exactly as kotlinc's Java member scope resolves it. The
                    // emitted callable is unaffected: it reads `m.physical_name`/`m.name` below, so the
                    // call still goes to `remove(I)`. A member that already carries a `physical_name`
                    // is a synthesized alias (a value-class mangled member, or a builtin whose Kotlin
                    // name was recorded there), never a raw Java method — leave it alone.
                    let scope_name = m.physical_name.as_ref().map_or_else(
                        || {
                            function_renames
                                .iter()
                                .find(|mapping| {
                                    mapping.physical_name == m.name
                                        && mapping.descriptor == m.descriptor
                                })
                                .map_or(m.name.as_str(), |mapping| mapping.source_name.as_str())
                        },
                        |_| m.name.as_str(),
                    );
                    if scope_name == name {
                        crate::trace_compiler!(
                            "resolve",
                            "member declaration {}.{} desc={} sig={:?}",
                            cn.render(),
                            m.name,
                            m.descriptor,
                            m.signature
                        );
                        // The provider-normalized signature is the single source of truth for every
                        // carrier. For a `.class` member it also restores receiver function-type marks
                        // that the raw JVM `Signature` attribute cannot spell; for a
                        // `.kotlin_builtins` member there is no raw attribute at all, so the provider's
                        // decoded signature is what preserves and binds its type parameters. Re-parsing
                        // `m.signature` here would therefore lose facts in the former case and fail to
                        // produce any signature in the latter.
                        //
                        // A member signature can mention its OWNER's type variables in its value
                        // parameters, including under a function/SAM argument. Bind those variables
                        // from the applied dispatch type before call-site inference, exactly as return
                        // recovery below does. This is deliberately independent of whether the
                        // provider represented a receiver in `GenericSig`: metadata signatures carry
                        // one while a raw JVM `Signature` cannot, but both describe the same semantic
                        // need. The partial substitution policy leaves the member's own formals open.
                        let generic_sig = m.generic_sig.clone().map(|signature| {
                            let bindings = self.member_receiver_bindings_name(
                                receiver,
                                cn,
                                &signature.formals,
                            );
                            if bindings.is_empty() {
                                return signature;
                            }
                            GenericSig {
                                params: signature
                                    .params
                                    .iter()
                                    .map(|param| ty_subst_keep_unbound(*param, &bindings))
                                    .collect(),
                                ..signature
                            }
                        });
                        // A `suspend fun` member's physical method appends a `Continuation` parameter
                        // and erases its return to `Object`; the provider-normalized member already
                        // exposes the logical parameter list. The coroutine pass re-derives the CPS
                        // form for emission.
                        let suspend = m.suspend();
                        let params = m.params.clone();
                        // `build_library_type` normalized this selected member's physical CPS
                        // descriptor exactly once. Reapplying that operation here would remove the
                        // final real source parameter from the already-logical descriptor.
                        let descriptor = m.descriptor.clone();
                        let meta_name = m.physical_name.as_deref().unwrap_or(&m.name);
                        let metadata_ret = m.declared_ret.or_else(|| {
                            self.cp
                                .metadata_property_ret_ty_name(cn, meta_name, &m.descriptor)
                        });
                        let suspend_ret_nullable = suspend && m.ret_nullable();
                        let ret = if suspend {
                            // A generic `suspend` member returns a type parameter (`byId(): T`) via
                            // `Continuation<T>`; bind `T` to the receiver's concrete argument
                            // (`Repo<Cfg>` → `T = Cfg`) so the return isn't erased to `Any`.
                            let recv_binds = generic_sig
                                .as_ref()
                                .map(|signature| {
                                    self.member_receiver_bindings_name(
                                        receiver,
                                        cn,
                                        &signature.formals,
                                    )
                                })
                                .unwrap_or_default();
                            let base = generic_sig
                                .as_ref()
                                .and_then(|g| suspend_return_from_gsig(g, &recv_binds))
                                .unwrap_or(m.ret);
                            // `suspend_return_from_gsig` canonicalized a collection return to its
                            // READ-ONLY Kotlin form (the JVM signature erases read-only vs mutable).
                            // Recover the EXACT source form (`List` vs `MutableList`, …) from the
                            // member's `@Metadata` return type — which preserves it at every nesting
                            // level — under the same-JVM-internal guard, so `.add(…)` on a declared
                            // `MutableList` (or on its `MutableSet` element) return still resolves.
                            // Ordinary and suspend members must not grow separate metadata/JVM merge
                            // policies: both arms overlay through the same guarded projection; only
                            // the way each obtains `base` differs (Continuation generic argument
                            // here, ordinary generic signature below).
                            let base = metadata_ret
                                .map_or(base, |meta| overlay_metadata_collection_names(base, meta));
                            crate::trace_compiler!(
                                "suspend",
                                "suspend return {cn}.{}: gsig={:?} base={:?} nullable={}",
                                m.name,
                                generic_sig,
                                base,
                                suspend_ret_nullable
                            );
                            // The `Continuation<T>` generic argument carries a PRIMITIVE return
                            // BOXED (generics erase primitives to their wrapper); unbox it to the
                            // source Kotlin primitive (`java/lang/Long` → `Ty::Long`) so
                            // `val n: Long = r.count()` type-checks. Nullability is applied by
                            // `ret_nullable` below (which mirrors the non-suspend member path).
                            base.obj_internal()
                                .and_then(crate::jvm::jvm_class_map::wrapper_to_kotlin_prim_name)
                                .map(kotlin_name_to_ty)
                                .unwrap_or(base)
                        } else {
                            let recovered = generic_sig.as_ref().and_then(|signature| {
                                concrete_generic_ret(signature).or_else(|| {
                                    let bindings = self.member_receiver_bindings_name(
                                        receiver,
                                        cn,
                                        &signature.formals,
                                    );
                                    (!bindings.is_empty())
                                        .then(|| ty_subst(signature.ret, &bindings))
                                })
                            });
                            crate::trace_compiler!(
                                "resolve",
                                "member return {}.{}: recovered={:?} erased={:?} (gsig={})",
                                receiver.name(),
                                m.name,
                                recovered,
                                m.ret,
                                generic_sig.is_some()
                            );
                            // The JVM signature erases read-only vs mutable (`List`/`MutableList`
                            // both spell `java/util/List`) at every nesting level; the member's
                            // `@Metadata` return type preserves it — for a FUNCTION and for a
                            // property GETTER (which is not a metadata function) alike. Overlay the
                            // metadata classifiers under the same-JVM-internal guard, per level —
                            // the same projection the suspend arm applies.
                            let base = recovered.unwrap_or(m.ret);
                            metadata_ret
                                .map_or(base, |meta| overlay_metadata_collection_names(base, meta))
                        };
                        let call_sig = m.call_sig.clone();
                        // A generic-return builtin member (`Map.get(K): V?`) resolves to the erased
                        // classpath method (`java/util/Map.get` → `Object`), which carries no Kotlin
                        // nullability. Recover the source `V?` from the builtin `@Metadata`. Applied
                        // only for a PRIMITIVE return (a nullable primitive is a distinct BOXED type,
                        // so `m[k] ?: d` must null-check before unboxing); a nullable REFERENCE already
                        // null-checks regardless, and keeps its plain erased `Ty` (see below).
                        // `cn` may already be the front-end `kotlin/collections/…` name or the erased
                        // JVM form (`java/util/Map`, when the member is found on a classpath supertype);
                        // map both to the builtin whose `@Metadata` declares the nullability.
                        let builtin_cn =
                            crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(cn)
                                .unwrap_or(cn);
                        let builtin_ret_nullable = !ret.is_reference()
                            && self.cp.builtin_member_ret_nullable_name(
                                builtin_cn,
                                &m.name,
                                params.len(),
                            );
                        // Exact builtin declarations already carry their role from the shared
                        // declaration normalizer. A builtin scalar whose metadata declares no
                        // `hashCode` (`Int`) is the one additional JVM projection: its wrapper
                        // method realizes the `Any` declaration that the scalar inherits.
                        let inherited_wrapper_role = (m.realization
                            == crate::libraries::MemberRealization::Dispatch
                            && m.name == "hashCode"
                            && params.is_empty()
                            && crate::jvm::jvm_class_map::wrapper_internal(Ty::obj_name(
                                builtin_cn,
                            ))
                            .is_some())
                        .then_some(crate::types::SemanticCallRole::KotlinAnyHashCode);
                        let semantic_role = m.semantic_role.or(inherited_wrapper_role);
                        let physical_owner = erased_top_member_owner(cn, m.owner.as_ref().copied());
                        let collection_barrier = collection_barrier_role(
                            &self.cp,
                            builtin_cn,
                            scope_name,
                            &m.descriptor,
                        );
                        let callable = LibraryCallable {
                            annotations: m.annotations.clone(),
                            deprecation: m.deprecation.clone(),
                            reflection_name: Some(m.name.clone()),
                            physical_name: m.physical_name.clone(),
                            inline: m.inline,
                            suspend,
                            context_count: m.context_count,
                            member_realization: m.realization,
                            compiler_intrinsic: m.realization.compiler_intrinsic(),
                            semantic_role,
                            collection_barrier,
                            signature: m.signature.clone(),
                            // Preserve the declaration-level return recovered when the class member
                            // was aligned with metadata. This overload view is the common input to
                            // instance-member selection (named calls and operator `invoke` alike);
                            // rebuilding a callable without the fact makes a later specialized
                            // `Object` return indistinguishable from a genuinely boxed generic slot.
                            declared_ret: m.declared_ret,
                            declared_params: m.generic_sig.as_ref().map(|signature| {
                                signature.parameters_with_receiver(m.context_count)
                            }),
                            equality_bound: m.equality_bound,
                            // Whether the dispatch owner is an interface is the MEMBER's fact here.
                            // For a mapped builtin resolved with no JDK on the classpath the JVM
                            // owner (`java/util/List`) has no class file, so the call site cannot
                            // recover it later — carry it on the selected callable.
                            owner_is_interface: m.is_interface(),
                            default_realization: self
                                .member_default_realization(physical_owner, m)
                                .map(Box::new),
                            nonvirtual_realization: m.nonvirtual_realization.clone(),
                            ..LibraryCallable::library(
                                physical_owner,
                                m.name.clone(),
                                params,
                                ret,
                                m.physical_ret,
                                descriptor,
                            )
                        };
                        let mut callable = callable;
                        // `scope_name` is the provider-normalized Kotlin declaration name. A raw
                        // Java method may carry only its physical spelling on `m`; attach the
                        // inherited-call policy here while both views still name the same exact
                        // declaration. Later phases consume only the external identity and this
                        // typed candidate set.
                        callable.overridden_call_realizations =
                            crate::jvm::mapped_builtin_declarations::overridden_call_realizations_for_declaration(
                                scope_name,
                                &callable.descriptor,
                                crate::jvm::mapped_builtin_declarations::MappedBuiltinMemberKind::Function,
                            )
                            .into_boxed_slice();
                        // `params` is the call-site-specialized Kotlin declaration shape; the
                        // classfile descriptor remains the physical ABI. In particular, an
                        // object-erased value-class parameter (`Result<T>` -> `Object`) must not be
                        // rebuilt as a physically boxed `Result<T>` merely because both facts were
                        // normalized through this member-overload view.
                        callable.physical_params = m.physical_params.clone();
                        callable.physical_parameter_plan = m.physical_parameter_plan.clone();
                        callable.inline_body_plan = self.inline_body_plan(&callable).map(Box::new);
                        let inline_body_plan = callable.inline_body_plan.clone();
                        // Java erasure governs raw Java declarations and Kotlin mapped-builtin
                        // slots. An ordinary Kotlin metadata declaration keeps Kotlin override
                        // identity even when its physical descriptor happens to erase like an
                        // unrelated Java member (notably `Int` versus the mangled `UInt`).
                        let provider_override_slot = (!t.is_kotlin
                            || crate::jvm::jvm_class_map::maps_to_distinct_jvm_internal(cn))
                        .then(|| {
                            override_slots::slot(
                                self,
                                // `physical_owner` remains the semantic classifier for most mapped
                                // builtins so ordinary call emission can retain its selected Kotlin
                                // declaration identity. Java override scope is different: its owner is
                                // the mapped Java classifier whose erased member table defines the
                                // slot (`AbstractMutableCollection` -> `AbstractCollection`).
                                crate::jvm::jvm_class_map::to_jvm_type_name(physical_owner),
                                callable
                                    .physical_name
                                    .clone()
                                    .unwrap_or_else(|| callable.name.clone()),
                                callable.physical_params.iter().copied(),
                            )
                        });
                        overloads.push(FunctionInfo {
                            provider_override_slot,
                            ret: ReturnInfo::new(
                                m.ret_nullable()
                                    || builtin_ret_nullable
                                    || (suspend_ret_nullable && !ret.is_reference()),
                                None,
                            ),
                            visibility: m.visibility,
                            receiver_rank: 0,
                            overload_rank: descriptor_narrowing(&m.descriptor) as u32,
                            generic_sig,
                            call_sig,
                            context_count: m.context_count,
                            flags: FnFlags {
                                inline: m.inline,
                                reified: m.reified,
                                suspend,
                                operator: m.is_operator(),
                                infix: m.is_infix(),
                                is_abstract: m.is_abstract(),
                                is_final: m.is_final(),
                                deprecated_hidden: m.deprecated_hidden(),
                                inherited_by_delegation: m.inherited_by_delegation(),
                                return_value_status: m.return_value_status,
                            },
                            ..FunctionInfo::plain(FnKind::Member, Some(receiver), callable)
                        });
                        if let Some(function) = overloads.last_mut() {
                            // Keep the selected member record and callable handle structurally
                            // identical; either lowering seam may consume the declaration.
                            function.callable.inline_body_plan = inline_body_plan;
                        }
                    }
                }
            }
        }
        FunctionSet { overloads }
    }
}
