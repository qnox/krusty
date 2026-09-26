//! Semantic Kotlin types spelled as JVM generic-signature elements: declaration-site variance
//! realized as use-site wildcards, primitives boxed, and each classifier's outer chain written out.

use super::*;

mod type_arguments;

/// Whether declaration-site variance becomes a JVM wildcard at the position being formatted.
///
/// kotlinc writes those wildcards in PARAMETER positions only: a return type and a field type get the
/// invariant spelling, at EVERY nesting depth (`fun <U> deep(a: Map<String, List<U>>): Map<String,
/// List<U>>` signs its parameter `Ljava/util/Map<Ljava/lang/String;+Ljava/util/List<+TU;>;>;` and its
/// return `Ljava/util/Map<Ljava/lang/String;Ljava/util/List<TU;>;>;`). An explicit `in`/`out`
/// projection the user wrote is not declaration-site variance and renders in either mode.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Wildcards {
    /// A parameter position: realize declaration-site variance as `+`/`-`.
    Declared,
    /// A return or field position: spell every argument invariantly.
    Suppressed,
}

/// Format backend-agnostic semantic types into JVM generic-signature elements. The ordinary JVM
/// descriptor and the optional generic `Signature` attribute are separate classfile declarations: the
/// former supplies runtime calling types, while this formatter preserves type parameters, type
/// arguments, and declaration-site variance for classpath readers.
pub(super) struct JvmSignatureFormatter<'a> {
    ir: &'a IrFile,
    symbols: &'a dyn BackendClassifierSource,
    run: &'a EmitRun,
}

impl<'a> JvmSignatureFormatter<'a> {
    pub(super) fn new(ir: &'a IrFile, env: &'a EmitEnv<'_>) -> Self {
        Self {
            ir,
            symbols: env.signature_symbols,
            run: env.run,
        }
    }

    /// A formatter over `symbols` where no [`EmitEnv`] is at hand.
    pub(super) fn with_symbols(
        ir: &'a IrFile,
        symbols: &'a dyn BackendClassifierSource,
        run: &'a EmitRun,
    ) -> Self {
        Self { ir, symbols, run }
    }

    fn current_class(&self, classifier: TypeName) -> Option<&crate::ir::IrClass> {
        self.ir
            .classes
            .iter()
            .find(|candidate| candidate.fq_name == classifier)
    }

    fn classifier_signature_chain(
        &self,
        owner: TypeName,
    ) -> Option<Vec<(TypeName, Vec<TypeVariance>)>> {
        let mut chain = Vec::new();
        let mut current = owner;
        loop {
            let (variances, outer) = if let Some(classifier) = self.current_class(current) {
                let variances = if classifier.type_params.is_empty() {
                    Vec::new()
                } else {
                    let Some(signature) = self.ir.class_signature_name(current) else {
                        self.run.set_emit_error(format!(
                            "internal: current IR classifier '{}' has type arguments but no checked generic signature",
                            current.render()
                        ));
                        return None;
                    };
                    signature
                        .type_params
                        .iter()
                        .map(|parameter| parameter.variance)
                        .collect()
                };
                let outer = if classifier.is_inner_class {
                    let Some(outer) = current.nested_owner() else {
                        self.run.set_emit_error(format!(
                            "internal: inner classifier '{}' has no semantic enclosing classifier",
                            current.render()
                        ));
                        return None;
                    };
                    Some(outer)
                } else {
                    None
                };
                (variances, outer)
            } else {
                let Some(classifier) = self.symbols.classifier(current) else {
                    self.run.set_emit_error(format!(
                        "internal: JVM signature references classifier '{}' absent from checked symbols",
                        current.render()
                    ));
                    return None;
                };
                let own_count = classifier.own_type_parameter_count;
                if own_count > classifier.type_param_variances.len() {
                    self.run.set_emit_error(format!(
                        "internal: classifier '{}' publishes {} own type parameters but only {} semantic variances",
                        current.render(),
                        own_count,
                        classifier.type_param_variances.len()
                    ));
                    return None;
                }
                (
                    classifier.type_param_variances[..own_count].to_vec(),
                    classifier.outer_instance,
                )
            };
            chain.push((current, variances));
            let Some(outer) = outer else { break };
            current = outer;
        }
        chain.reverse();
        Some(chain)
    }

    /// JVM generic applications contain parameters declared by the classifier and its non-static
    /// inner-class chain. A body-local classifier's semantic application additionally carries type
    /// parameters captured from its enclosing function so the frontend can check its members and
    /// supertypes. Those captures are not JVM class parameters (kotlinc uses the local/anonymous
    /// class raw at use sites), so remove exactly the explicitly published captured suffix.
    fn classifier_usage_arguments<'b>(
        &self,
        owner: TypeName,
        arguments: &'b [Ty],
        declared: usize,
    ) -> Option<&'b [Ty]> {
        if arguments.len() == declared {
            return Some(arguments);
        }
        let captured = if let Some(classifier) = self.current_class(owner) {
            classifier.captured_type_params.len()
        } else {
            let classifier = self.symbols.classifier(owner)?;
            classifier
                .type_param_variances
                .len()
                .checked_sub(classifier.own_type_parameter_count)
                .unwrap_or_else(|| {
                    self.run.set_emit_error(format!(
                        "internal: classifier '{}' has more own type parameters than semantic parameters",
                        owner.render()
                    ));
                    0
                })
        };
        if captured != 0 && arguments.len() == declared + captured {
            return Some(&arguments[..declared]);
        }
        self.run.set_emit_error(format!(
            "internal: JVM signature supplies {} type arguments to classifier '{}' with {} type parameters across its inner-class chain",
            arguments.len(),
            owner.render(),
            declared
        ));
        None
    }

    fn declaration_variance(&self, owner: TypeName, index: usize) -> Option<TypeVariance> {
        let chain = self.classifier_signature_chain(owner)?;
        let total: usize = chain.iter().map(|(_, variances)| variances.len()).sum();
        let Some(variance) = chain
            .iter()
            .flat_map(|(_, variances)| variances.iter().copied())
            .nth(index)
        else {
            self.run.set_emit_error(format!(
                "internal: JVM signature supplies type argument {} to classifier '{}' with {} type parameters across its inner-class chain",
                index + 1,
                owner.render(),
                total
            ));
            return None;
        };
        Some(variance)
    }

    fn classifier_is_closed(&self, owner: TypeName) -> Option<bool> {
        if let Some(classifier) = self.current_class(owner) {
            return Some(
                classifier.is_object
                    || classifier.is_annotation
                    || (!classifier.is_interface && !classifier.is_abstract && !classifier.is_open),
            );
        }
        let Some(classifier) = self.symbols.classifier(owner) else {
            self.run.set_emit_error(format!(
                "internal: JVM wildcard optimization references classifier '{}' absent from checked symbols and current IR",
                owner.render()
            ));
            return None;
        };
        Some(match classifier.kind {
            crate::libraries::TypeKind::Object | crate::libraries::TypeKind::Annotation => true,
            crate::libraries::TypeKind::Class => {
                !classifier.is_abstract && !classifier.is_extensible
            }
            crate::libraries::TypeKind::Enum => !classifier.is_extensible,
            crate::libraries::TypeKind::Interface => false,
        })
    }

    fn can_have_subtypes_ignoring_nullability(&self, ty: Ty) -> Option<bool> {
        let ty = match ty {
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => *inner,
            ty => ty,
        };
        if ty == Ty::Nothing {
            return Some(false);
        }
        if matches!(ty, Ty::TyParam(..)) {
            return Some(true);
        }
        // Core's COMPACT variants for final Kotlin classifiers (`Unit`, `String`, the scalars and
        // unsigned types) have no `kotlin_class_internal`, and the permissive fallback below would
        // hand them a spurious `? extends` (`Function1<…, +Lkotlin/Unit;>` where kotlinc writes the
        // invariant spelling) — they are final, so answer directly.
        if matches!(ty, Ty::Unit | Ty::String) || ty.is_jvm_scalar() || ty.is_unsigned() {
            return Some(false);
        }
        // Core has compact variants for common Kotlin classifiers, but that storage choice does not
        // change the JVM wildcard rule. Ask for their semantic classifier identity exactly as for an
        // `Obj`; otherwise final `String`/numeric arguments would incorrectly gain `? extends`.
        let Some(owner) = ty.kotlin_class_internal() else {
            return Some(true);
        };
        let arguments = ty.type_args();
        let chain = self.classifier_signature_chain(owner)?;
        let declared_arguments = chain.iter().map(|(_, variances)| variances.len()).sum();
        let arguments = self.classifier_usage_arguments(owner, arguments, declared_arguments)?;
        let is_closed = self.classifier_is_closed(owner)?;
        if !is_closed {
            return Some(true);
        }
        for (index, argument) in arguments.iter().copied().enumerate() {
            let declaration = self.declaration_variance(owner, index)?;
            let (use_site, argument) = match argument {
                Ty::InProjection(inner) => (TypeVariance::In, *inner),
                Ty::OutProjection(inner) => (TypeVariance::Out, *inner),
                // A star is already the complete JVM wildcard. Its frontend read bound is not a
                // classfile type argument and must not participate in wildcard optimization.
                Ty::StarProjection(_) => return Some(true),
                argument => (TypeVariance::Invariant, argument),
            };
            let effective = if use_site == TypeVariance::Invariant {
                declaration
            } else {
                use_site
            };
            if effective == TypeVariance::Out
                && self.can_have_subtypes_ignoring_nullability(argument)?
            {
                return Some(true);
            }
            if effective == TypeVariance::In && argument.non_null() != Ty::obj("kotlin/Any") {
                return Some(true);
            }
        }
        Some(false)
    }

    fn wildcard_is_redundant(&self, variance: TypeVariance, argument: Ty) -> Option<bool> {
        match variance {
            TypeVariance::Invariant => Some(true),
            TypeVariance::Out => self
                .can_have_subtypes_ignoring_nullability(argument)
                .map(|can_have_subtypes| !can_have_subtypes),
            TypeVariance::In => Some(argument.non_null() == Ty::obj("kotlin/Any")),
        }
    }

    fn function_ty(&self, signature: &crate::types::FnSig, wildcards: Wildcards) -> Option<String> {
        let arity = signature.params.len() + usize::from(signature.suspend);
        if arity > 22 {
            // `FunctionN` has only its covariant result parameter. Kotlin metadata carries the
            // complete parameter list; the Java generic Signature records only this JVM carrier.
            let result = if signature.suspend {
                Ty::nullable(Ty::obj("kotlin/Any"))
            } else {
                signature.ret
            };
            return Some(format!(
                "Lkotlin/jvm/functions/FunctionN<{}>;",
                self.type_argument(TypeVariance::Out, result, wildcards)?
            ));
        }
        let mut rendered = format!("Lkotlin/jvm/functions/Function{arity}<");
        for parameter in &signature.params {
            rendered.push_str(&self.type_argument(TypeVariance::In, *parameter, wildcards)?);
        }
        if signature.suspend {
            // The continuation's own `in` projection is part of the type; the wildcards on the
            // `FunctionN` arguments follow the position, like every other argument's.
            if wildcards == Wildcards::Declared {
                rendered.push('-');
            }
            rendered.push_str("Lkotlin/coroutines/Continuation<-");
            rendered.push_str(&self.ty_at(&signature.ret, wildcards)?);
            rendered.push_str(">;");
            if wildcards == Wildcards::Declared {
                rendered.push('+');
            }
            rendered.push_str("Ljava/lang/Object;");
        } else {
            rendered.push_str(&self.type_argument(TypeVariance::Out, signature.ret, wildcards)?);
        }
        rendered.push_str(">;");
        Some(rendered)
    }

    /// One parameter or return position in a method `Signature`. Positions without generic structure
    /// use their exact JVM descriptor spelling; structured positions are rendered from the semantic
    /// type. This is a structural choice, not a recovery path after semantic formatting failed.
    pub(super) fn method_ty(&self, ty: &Ty, wildcards: Wildcards) -> Option<String> {
        let semantic = match ty {
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => inner,
            ty => ty,
        };
        match semantic {
            Ty::TyParam(..) | Ty::Fun(_) => self.ty_at(ty, wildcards),
            Ty::Obj(_, arguments) if !arguments.is_empty() => self.ty_at(ty, wildcards),
            Ty::InProjection(_)
            | Ty::OutProjection(_)
            | Ty::StarProjection(_)
            | Ty::Null
            | Ty::Error => {
                self.run.set_emit_error(format!(
                    "internal: invalid semantic method-signature type {ty:?}"
                ));
                None
            }
            _ => Some(ir_type_desc(ty)),
        }
    }

    /// Translate one semantic Kotlin type into a JVM generic-signature element. Kotlin declaration-
    /// site variance has no classfile equivalent, so the JVM backend realizes it as a wildcard on
    /// each otherwise-unprojected use-site argument. Explicit Kotlin `in`/`out` projections already
    /// carry their own direction and take precedence.
    /// A type in a PARAMETER position (declaration-site variance becomes a wildcard).
    pub(super) fn ty(&self, ty: &Ty) -> Option<String> {
        self.ty_at(ty, Wildcards::Declared)
    }

    pub(super) fn ty_at(&self, ty: &Ty, wildcards: Wildcards) -> Option<String> {
        if let Ty::Nullable(inner) | Ty::PlatformNullable(inner) = ty {
            return self.ty_at(inner, wildcards);
        }
        if let Ty::TyParam(name, _) = ty {
            return Some(format!(
                "T{};",
                crate::types::type_parameter_source_name(name)
            ));
        }
        if ty.non_null().is_jvm_scalar() {
            return Some(boxed_descriptor(ty.non_null()));
        }
        match *ty {
            Ty::String => Some("Ljava/lang/String;".to_string()),
            Ty::Unit => Some("Lkotlin/Unit;".to_string()),
            Ty::Nothing => Some("Lkotlin/Nothing;".to_string()),
            Ty::InProjection(inner) => Some(format!("-{}", self.ty_at(inner, wildcards)?)),
            Ty::OutProjection(inner) => Some(format!("+{}", self.ty_at(inner, wildcards)?)),
            Ty::Fun(signature) => self.function_ty(signature, wildcards),
            // `kotlin.Array<E>` has no JVM class: its realization is the ARRAY type `[E`, and that is
            // how a signature must spell it. Writing `Lkotlin/Array<…>;` names a class no loader can
            // resolve, so any reader of the attribute (reflection, a Java consumer, a decompiler)
            // fails on it. kotlinc writes `[` + the element's signature — and, since a signature that
            // adds nothing over the descriptor is omitted entirely, `Array<String>` ends up with no
            // attribute at all while `Array<T>` keeps `[TT;`.
            Ty::Obj(owner, arguments) if owner.matches("kotlin/Array") && arguments.len() == 1 => {
                // The element's own variance is not written: a JVM array type has no argument list to
                // put it on. `Array<out String>` erases to `[Ljava/lang/String;`, as kotlinc emits.
                let element = match &arguments[0] {
                    Ty::InProjection(inner)
                    | Ty::OutProjection(inner)
                    | Ty::StarProjection(inner) => inner,
                    argument => argument,
                };
                Some(format!("[{}", self.ty_at(element, wildcards)?))
            }
            Ty::Obj(owner, arguments) if self.is_written_raw(owner, arguments)? => Some(format!(
                "L{};",
                crate::jvm::names::classfile_internal_name(&owner.render())
            )),
            Ty::Obj(owner, arguments) => {
                let chain = self.classifier_signature_chain(owner)?;
                let declared_arguments: usize =
                    chain.iter().map(|(_, variances)| variances.len()).sum();
                let arguments =
                    self.classifier_usage_arguments(owner, arguments, declared_arguments)?;
                let (outer, _) = chain.first()?;
                let outer_internal = outer.render();
                let jvm = crate::jvm::names::classfile_internal_name(&outer_internal);
                let mut signature = format!("L{jvm}");
                let mut argument_index = 0;
                for (segment_index, (classifier, variances)) in chain.iter().enumerate() {
                    if segment_index != 0 {
                        signature.push('.');
                        signature.push_str(classifier.nested_segment_ref());
                    }
                    if !variances.is_empty() {
                        signature.push('<');
                        for &variance in variances {
                            signature.push_str(&self.type_argument(
                                variance,
                                arguments[argument_index],
                                wildcards,
                            )?);
                            argument_index += 1;
                        }
                        signature.push('>');
                    }
                }
                signature.push(';');
                Some(signature)
            }
            _ => None,
        }
    }
}
