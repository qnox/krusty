//! Package members a Kotlin facade class declares under one source name.
//!
//! Extension discovery is @Metadata-driven (the source of truth), NOT a scan of JVM statics: the
//! package's PUBLIC facades' metadata carry each extension's SOURCE receiver, parameters, return
//! (with nullability), visibility, and generic signature. The JVM method (`@JvmName`-mangled name +
//! descriptor) is only the emit handle, rooted at the PUBLIC facade — kotlinc's `invokestatic`
//! target — so a package-private multifile PART never leaks a `false` visibility. Receiver-coupled
//! JVM specifics (element-variant `sumOfInt`, value-class mangling) are the emitter's job.

use super::*;

impl JvmLibraries {
    /// The extension functions and properties `facade` declares under `name`. A facade is decoded
    /// only for a kind whose name scan lists `name`; functions and properties are asked separately
    /// because a multifile facade merges each kind from its parts on its own.
    pub(super) fn facade_package_callables(
        &self,
        facade: TypeName,
        namespace: SymbolNamespace,
        name: &str,
        overloads: &mut Vec<FunctionInfo>,
        props: &mut Vec<PropertyInfo>,
    ) {
        if self.cp.facade_declares_function(facade, name) {
            self.facade_extension_functions(facade, namespace, name, overloads);
        }
        if self.cp.facade_declares_property(facade, name) {
            self.facade_properties(facade, namespace, name, props);
        }
    }

    fn facade_extension_functions(
        &self,
        facade: TypeName,
        namespace: SymbolNamespace,
        name: &str,
        overloads: &mut Vec<FunctionInfo>,
    ) {
        let functions = self.cp.meta_functions_name(facade);
        let lambda_return_overload = self.cp.lambda_return_overloads(facade).contains(name);
        for mf in functions.iter() {
            if mf.kotlin_name != name || !mf.is_extension() {
                continue;
            }
            if self.since_kotlin_withheld(mf.since_kotlin()) {
                if let Some(receiver) = mf.receiver_class {
                    self.note_api_withheld(receiver, name);
                }
            }
            if self.hides_function(mf) {
                continue;
            }
            let raw_receiver = mf.generic_sig.as_ref().and_then(|g| g.receiver);
            let receiver = raw_receiver
                .map(|r| match r.kind() {
                    TyKind::TyParam(..) => r,
                    _ => ty_subst(r, &crate::types::TyParamBindings::default()),
                })
                .or_else(|| mf.receiver_class.map(kotlin_type_name_to_ty))
                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
            // Emit handle: the JVM method + descriptor on the public facade. Prefer the metadata
            // `method_signature`. When it is absent, resolve the real bytecode method by name against the
            // facade's super chain, trying: the metadata name, then the ELEMENT-materialized name kotlinc
            // gives an `@OverloadResolutionByLambdaReturnType` reduction (`sum` on `Iterable<Byte>` →
            // `sumOfByte`) — derived from the receiver's element type and VERIFIED against the bytecode,
            // no hardcoded name list. The discovered method's real name becomes the emit `jvm_name`.
            let elem_mangled = receiver
                .type_args()
                .first()
                .copied()
                .and_then(Ty::kotlin_class_internal)
                .map(|element| format!("{}Of{}", mf.jvm_name, element.segment_ref()));
            let lambda_return_mangled = lambda_return_overload
                .then_some(mf.ret_class)
                .flatten()
                .map(|ret| format!("{}{}", mf.kotlin_name, ret.segment()));
            // The receiver's erased descriptor disambiguates same-named overloads on the facade
            // (`maxOrNull([I)` vs `maxOrNull([D)`); the return descriptor disambiguates same-receiver
            // overloads (`maxOrNull(Iterable)Double` vs `…Comparable`). A type-var return has no class.
            let recv_desc = type_descriptor(
                <Self as crate::libraries::SemanticPlatform>::library_value_form(
                    self,
                    raw_receiver.unwrap_or(receiver),
                ),
            );
            let ret_desc = mf.ret_class.map(|r| {
                type_descriptor(
                    <Self as crate::libraries::SemanticPlatform>::library_value_form(
                        self,
                        kotlin_type_name_to_ty(r),
                    ),
                )
            });
            // The value parameters' ERASED JVM descriptors, from the metadata generic signature, so a
            // bytecode lookup disambiguates same-receiver/same-return overloads by their VALUE param —
            // both a concrete type (`appendLine(StringBuilder, int)` vs `appendLine(StringBuilder)`) and
            // a FUNCTION type (`any(Iterable, Function1)` vs `any(Iterable)`). Type variables erase
            // through their bounds. `None` (match by receiver alone) means no generic signature.
            let value_param_descs: Option<Vec<String>> = mf.generic_sig.as_ref().map(|g| {
                g.params
                    .iter()
                    .map(|p| {
                        type_descriptor(
                            <Self as crate::libraries::SemanticPlatform>::library_value_form(
                                self, *p,
                            ),
                        )
                    })
                    .collect()
            });
            let by_name = |n: &str| {
                self.cp.facade_method(
                    facade,
                    n,
                    Some(&recv_desc),
                    ret_desc.as_deref(),
                    value_param_descs.as_deref(),
                )
            };
            // Match the bytecode method to recover its descriptor and inline implementation details.
            // A metadata JVM signature names the method exactly: a value-class receiver
            // (`Result<T>.getOrThrow`) erases to its carrier there, which the receiver-keyed
            // overload lookup would miss, losing the method's privacy (`@InlineOnly`).
            let cand = if let Some(d) = mf.jvm_desc {
                self.cp.facade_static(facade, &mf.jvm_name, d)
            } else {
                by_name(&mf.jvm_name)
                    .or_else(|| lambda_return_mangled.as_ref().and_then(|n| by_name(n)))
                    .or_else(|| elem_mangled.as_ref().and_then(|n| by_name(n)))
            };
            let Some(cand) = cand else {
                continue;
            };
            let (jvm_name, descriptor) = (cand.name.clone(), cand.descriptor.clone());
            crate::trace_compiler!(
                "resolve",
                "extension emit handle {} metadata_params={:?} expected_descs={value_param_descs:?} selected={jvm_name}{descriptor}",
                mf.kotlin_name,
                mf.generic_sig.as_ref().map(|signature| &signature.params),
            );
            let bytecode_public = cand.public;
            // A `suspend fun`'s physical method appends a `Continuation` parameter and erases the
            // return to `Object`; present the LOGICAL signature (drop the continuation) so a
            // normal call resolves — the same rule the top-level and member paths apply. The
            // coroutine pass re-threads the CPS `Continuation` at the emitted call.
            let descriptor = if mf.is_suspend() {
                parameter_plans::logical_suspend_descriptor(&descriptor)
            } else {
                descriptor
            };
            let Some((physical_params, pret)) = parse_method_desc(&descriptor) else {
                continue;
            };
            if physical_params.is_empty() {
                continue;
            }
            let call_sig = mf.extension_call_sig();
            let ret_class = mf.ret_class.map(kotlin_type_name_to_ty);
            let ret = match ret_class {
                Some(t) if mf.ret_nullable() && t.is_jvm_scalar() => Ty::nullable(t),
                Some(t) => t,
                None if mf.ret_nullable() && pret.is_jvm_scalar() => Ty::nullable(pret),
                None => pret,
            };
            // The metadata-primary generic signature drives lambda-parameter and return binding.
            let generic_sig = mf.generic_sig.clone();
            let params = generic_sig
                .as_ref()
                .map(|signature| {
                    signature
                        .receiver
                        .into_iter()
                        .chain(signature.params.iter().copied())
                        .collect::<Vec<_>>()
                })
                .filter(|params| params.len() == physical_params.len())
                .unwrap_or_else(|| physical_params.clone());
            // The extension's DECLARED receiver source type — carried so the value-class pass can unbox
            // a boxed receiver (`fun Result<T>.getOrThrow` on a boxed `kotlin.Result`). `None` for a
            // type-VARIABLE receiver (`fun <T> T.let` — erases to `Object`, no value-class identity).
            let source_receiver = match generic_sig.as_ref().and_then(|g| g.receiver) {
                Some(r) if r.is_ty_param() => None,
                None => None,
                _ => Some(receiver),
            };
            let inline = metadata_inline(
                mf.is_inline(),
                mf.has_reified_type_params(),
                bytecode_public,
            );
            let declared_ret = (!mf.ret_nullable())
                .then(|| {
                    generic_sig
                        .as_ref()
                        .map(|signature| signature.ret)
                        .or(ret_class)
                })
                .flatten();
            let mut callable = LibraryCallable {
                annotations: mf.annotations.clone(),
                inline,
                suspend: mf.is_suspend(),
                source_receiver,
                // Preserve the metadata declaration independently of its Object-erased JVM
                // handle. `Result<T>` is the canonical ambiguous carrier: its physical return
                // and a boxed generic result are both `Object`, and only this selected
                // declaration fact tells value-class lowering which representation it received.
                declared_ret,
                context_count: mf.context_count(),
                contract: mf.contract.clone(),
                generic_sig: generic_sig.clone().map(Box::new),
                reified_type_parameter_ordinals: call_sig
                    .reified_type_parameter_ordinals
                    .clone()
                    .into_boxed_slice(),
                declared_params: generic_sig
                    .as_ref()
                    .map(|signature| signature.parameters_with_receiver(mf.context_count())),
                // Carry the bytecode generic `Signature` so a reified extension can bind explicit
                // type arguments instead of invoking its throwing inline-only method.
                signature: cand.signature.clone(),
                // The name this extension is DECLARED under, beside the JVM method it is
                // realized as: `@JvmName` renames the method, and a value-class signature
                // appends kotlinc's hash (`UInt.downTo` is `downTo-J1ME1BU`). Neither is
                // recoverable from the spelling, so the provider publishes both, as a
                // classifier member does (`LibraryCallable::classifier_member`).
                reflection_name: Some(mf.kotlin_name.clone()),
                physical_name: (jvm_name != mf.kotlin_name).then(|| jvm_name.clone()),
                ..LibraryCallable::library(
                    facade,
                    mf.kotlin_name.clone(),
                    params,
                    ret,
                    pret,
                    descriptor,
                )
            };
            callable.physical_params = physical_params;
            if call_sig.param_defaults.iter().any(|default| *default) {
                callable.default_realization =
                    self.top_level_default_realization(&callable).map(Box::new);
            }
            callable.inline_body_plan = self.inline_body_plan(&callable).map(Box::new);
            let paired = cand.paired_common;
            overloads.push(FunctionInfo {
                ret: ReturnInfo::new(mf.ret_nullable(), ret_class),
                visibility: mf.visibility,
                generic_sig,
                context_count: mf.context_count(),
                flags: FnFlags {
                    inline,
                    reified: mf.has_reified_type_params(),
                    suspend: mf.is_suspend(),
                    operator: mf.is_operator(),
                    infix: mf.is_infix(),
                    is_abstract: false,
                    is_final: mf.is_final(),
                    deprecated_hidden: false,
                    inherited_by_delegation: false,
                    return_value_status: Some(mf.return_value_status),
                },
                call_sig,
                ..FunctionInfo::plain(FnKind::Extension, Some(receiver), callable)
            });
            self.publish_function_deprecation(overloads.last_mut(), mf.deprecation());
            let common = &self.common_expectations;
            normalize_tail(&self.cp, common, namespace, name, paired, overloads);
        }
    }

    /// Receiver-less top-level properties (`val plugin: Plugin`) and extension properties
    /// (`arr.lastIndex`, `list.indices`) differ only by whether the accessors take a receiver.
    /// Metadata records each property with its real accessor names (`JvmPropertySignature`), so the
    /// getter name is authoritative, never a `getX` guess.
    fn facade_properties(
        &self,
        facade: TypeName,
        namespace: SymbolNamespace,
        name: &str,
        props: &mut Vec<PropertyInfo>,
    ) {
        let mprops = self.cp.meta_properties_name(facade);
        let matching_properties = mprops
            .iter()
            .filter(|property| property.name == name)
            .count();
        if matching_properties != 0 {
            crate::trace_compiler!(
                "metadata_properties",
                "property symbols namespace={namespace:?} name={name} facade={} decoded={} matching={matching_properties}",
                facade.render(),
                mprops.iter().count(),
            );
        }
        for mp in mprops.iter().filter(|property| property.name == name) {
            let package = match namespace {
                SymbolNamespace::Package(package) => Some(package),
                SymbolNamespace::Classifier(_) => None,
            };
            let accessor = |jvm_name: &str, descriptor: &str| {
                self.cp
                    .facade_static(facade, jvm_name, descriptor)
                    .map(|method| StaticAccessor {
                        owner: method.owner,
                        public: method.public,
                    })
            };
            props.extend(self.static_metadata_property(mp, facade, package, accessor));
        }
    }
}
