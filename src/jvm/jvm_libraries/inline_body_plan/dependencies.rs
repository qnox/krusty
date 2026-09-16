//! Metadata normalization for physical calls referenced by decoded inline bodies.

use super::*;

impl JvmLibraries {
    /// Match an exact physical constructor target to one metadata-normalized constructor
    /// declaration. Value-class `constructor-impl` remains merely the JVM realization attached to
    /// that declaration; consumers receive the semantic constructor identity.
    pub(super) fn inline_plan_constructor(
        &self,
        target: MethodTarget<'_>,
    ) -> Option<(crate::types::TypeName, LibraryMember)> {
        let (physical_owner_text, name, descriptor, interface) = target;
        if interface {
            return None;
        }
        let physical_owner = type_name(physical_owner_text);
        let owner = crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(physical_owner)
            .unwrap_or(physical_owner);
        let classifier = self.build_library_type(owner)?;
        let mut matches = classifier.constructors.iter().filter(|constructor| {
            constructor.physical_name.as_deref().unwrap_or("<init>") == name
                && physical_descriptor(constructor) == descriptor
        });
        let mut constructor = matches.next()?.clone();
        if matches.next().is_some() {
            return None;
        }
        constructor.owner = Some(owner);
        Some((owner, constructor))
    }

    /// Match an invoked physical target to exactly one metadata-normalized Kotlin member. JVM
    /// descriptors identify the realization only; they never supply semantic parameter/result types.
    pub(super) fn inline_plan_member(&self, target: MethodTarget<'_>) -> Option<LibraryMember> {
        let (physical_owner_text, name, descriptor, interface) = target;
        let physical_owner = type_name(physical_owner_text);
        let mapped_owners =
            crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_declarations(physical_owner);
        let owners = if !mapped_owners.is_empty() {
            mapped_owners
        } else {
            std::slice::from_ref(&physical_owner)
        };
        // Decode every semantic declaration in the physical owner's erasure group directly.
        // Going through `classifier_record` here can observe a recursively-building cache entry
        // while the enclosing top-level inline declaration is being normalized. For collections,
        // unioning read-only and mutable declarations is essential: the same JVM interface owns
        // both families, while metadata alone tells which declaration owns the invoked member.
        let mut matches = Vec::new();
        for owner in owners {
            let classifier = self.build_library_type(*owner)?;
            matches.extend(classifier.members.iter().filter_map(|member| {
                let declared_name = member
                    .physical_name
                    .as_deref()
                    .unwrap_or(member.name.as_str());
                (crate::jvm::names::mapped_builtin_virtual_name(
                    physical_owner_text,
                    declared_name,
                    descriptor,
                ) == name
                    && physical_descriptor(member) == descriptor)
                    .then(|| (*owner, member.clone()))
            }));
        }
        // A member common to both collection faces has one physical realization; retain the
        // canonical declaration used throughout source typing. A mutable-only member has no such
        // match and therefore remains the sole candidate from the complete erasure family.
        if matches.len() > 1 {
            let canonical =
                crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(physical_owner);
            matches.retain(|(owner, _)| Some(*owner) == canonical);
        }
        let [(owner, member)] = matches.as_mut_slice() else {
            return None;
        };
        // Retain the invoked descriptor only as a physical realization. A suspend call's common
        // shape excludes its CPS continuation; the suspend pass appends that operand exactly once.
        let (mut physical_params, physical_ret) = super::super::parse_method_desc(descriptor)?;
        if member.suspend()
            && !physical_params.pop().is_some_and(|parameter| {
                parameter
                    .obj_internal()
                    .is_some_and(|name| name.matches("kotlin/coroutines/Continuation"))
            })
        {
            return None;
        }
        member.owner = Some(*owner);
        // The body supplied the exact physical target. Preserve its spelling when metadata exposes
        // a different Kotlin name (`CharSequence.get` is `charAt` on the JVM); otherwise stable
        // registration would retain the semantic name as though it were the emitted method.
        member.physical_name = (member.name != name).then(|| name.to_owned());
        member.descriptor = if member.suspend() {
            super::super::strip_continuation_param(descriptor)
        } else {
            descriptor.to_string()
        };
        member.physical_params = physical_params;
        member.physical_ret = physical_ret;
        member.set_is_interface(interface);
        Some(member.clone())
    }

    /// Resolve one receiver-less static target through the ordinary top-level provider path. The
    /// bytecode target selects a realization; semantic parameter/result facts still come from the
    /// exact Kotlin declaration normalized by that provider.
    pub(super) fn inline_plan_static_top_level(
        &self,
        target: MethodTarget<'_>,
    ) -> InlineDependency<LibraryCallable> {
        let (owner, name, descriptor, interface) = target;
        if interface {
            return InlineDependency::Rejected;
        }
        let owner = type_name(owner);
        let Some(package) = owner.parent() else {
            return InlineDependency::Rejected;
        };
        let mut matches = self
            .top_level_overloads(name, package)
            .into_iter()
            .filter(|function| {
                function.kind == FnKind::TopLevel
                    && function.callable.owner == owner
                    && function.callable.name == name
                    && function.callable.descriptor == descriptor
            });
        let Some(function) = matches.next() else {
            return InlineDependency::Unavailable;
        };
        if matches.next().is_some() {
            return InlineDependency::Rejected;
        }
        InlineDependency::Found(function.callable)
    }

    /// Normalize one physically static call that metadata declares as an extension. The descriptor
    /// selects the declaration at this provider boundary; all semantic parameter/result facts come
    /// from that exact Kotlin metadata declaration.
    pub(super) fn inline_plan_static_extension(
        &self,
        target: MethodTarget<'_>,
    ) -> InlineDependency<LibraryCallable> {
        let (owner, name, descriptor, interface) = target;
        if interface {
            return InlineDependency::Rejected;
        }
        let owner = type_name(owner);
        let Some(candidate) = self.cp.facade_static(owner, name, descriptor) else {
            return InlineDependency::Unavailable;
        };
        let Some((physical_params, physical_ret)) = super::super::parse_method_desc(descriptor)
        else {
            return InlineDependency::Rejected;
        };
        let facts = self.cp.metadata_call_facts_name(
            owner,
            name,
            &physical_params,
            &physical_ret,
            true,
            &|name| self.metadata_value_class_underlying(name),
        );
        if facts.visibility.is_none()
            || facts.deprecated_hidden
            || facts.kept_params != Some(physical_params.len())
            || facts.context_count != 0
        {
            return InlineDependency::Rejected;
        }
        let Some(signature) = facts.generic_sig else {
            return InlineDependency::Rejected;
        };
        let Some(receiver) = signature.receiver else {
            return InlineDependency::Rejected;
        };
        let params = facts.declared_params.unwrap_or_else(|| {
            signature
                .receiver
                .into_iter()
                .chain(signature.params.iter().copied())
                .collect()
        });
        if params.len() != physical_params.len() {
            return InlineDependency::Rejected;
        }
        let ret = facts.declared_ret.unwrap_or(signature.ret);
        let mut callable = LibraryCallable {
            inline: metadata_inline(
                facts.is_inline,
                facts.has_reified_type_params,
                candidate.public,
            ),
            suspend: facts.suspend,
            source_receiver: (!receiver.is_ty_param()).then_some(receiver),
            declared_ret: facts.declared_ret,
            context_count: facts.context_count,
            contract: facts.contract,
            generic_sig: Some(Box::new(signature.clone())),
            declared_params: Some(params.clone().into_boxed_slice()),
            signature: candidate.signature,
            ..LibraryCallable::library(
                owner,
                name.to_owned(),
                params,
                ret,
                physical_ret,
                descriptor.to_owned(),
            )
        };
        callable.physical_params = physical_params;
        InlineDependency::Found(callable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        InlineBodyPlan, InlineIterationIndex, InlineIterationTraversal, LibraryMember,
    };
    use crate::symbol_source::SymbolNamespace;

    fn iterable_for_each_indexed(libraries: &JvmLibraries) -> crate::libraries::FunctionInfo {
        libraries
            .symbols(
                SymbolNamespace::Package(type_name("kotlin/collections")),
                "forEachIndexed",
            )
            .callables
            .functions()
            .iter()
            .find(|function| {
                function.callable.descriptor
                    == "(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function2;)V"
            })
            .cloned()
            .expect("stdlib must declare Iterable.forEachIndexed")
    }

    fn iteration(
        libraries: &JvmLibraries,
        package: &str,
        name: &str,
        descriptor: &str,
    ) -> crate::libraries::FunctionInfo {
        libraries
            .symbols(SymbolNamespace::Package(type_name(package)), name)
            .callables
            .functions()
            .iter()
            .find(|function| function.callable.descriptor == descriptor)
            .cloned()
            .unwrap_or_else(|| panic!("missing {package}.{name}{descriptor}"))
    }

    fn assert_current_member_identity(libraries: &JvmLibraries, member: &LibraryMember) {
        let identity = member
            .external_identity
            .expect("cached traversal member must receive a per-classpath identity");
        let realization = libraries
            .cp
            .external_callable(identity)
            .expect("traversal identity must resolve in the consuming classpath");
        assert_eq!(
            realization.kind,
            crate::jvm::classpath::ExternalCallableKind::Member
        );
        assert_eq!(realization.callable.external_identity, Some(identity));
        assert_eq!(realization.callable.params, member.params);
        assert_eq!(realization.callable.ret, member.ret);
    }

    #[test]
    fn physical_collection_member_selects_its_exact_metadata_declaration() {
        let stdlib = crate::toolchain::stdlib_jar()
            .expect("inline dependency test requires the repository Kotlin stdlib");
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )));
        let member = libraries
            .inline_plan_member(("java/util/Collection", "add", "(Ljava/lang/Object;)Z", true))
            .expect("Collection.add must normalize through its mutable Kotlin declaration");

        assert_eq!(
            member.owner,
            Some(type_name("kotlin/collections/MutableCollection"))
        );
        assert_eq!(member.name, "add");
        assert_eq!(member.params.len(), 1);
        assert_eq!(member.ret, Ty::Boolean);
        assert!(member.is_interface());

        let shared = libraries
            .inline_plan_member((
                "java/lang/Iterable",
                "iterator",
                "()Ljava/util/Iterator;",
                true,
            ))
            .expect("a shared member must retain its canonical Kotlin declaration");
        assert_eq!(shared.owner, Some(type_name("kotlin/collections/Iterable")));
    }

    #[test]
    fn cached_iteration_dependencies_receive_each_classpaths_identity() {
        let version = crate::toolchain::kotlin_version();
        if !matches!(version.as_str(), "2.4.0" | "2.4.10") {
            return;
        }
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let warm = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib.clone()],
        )));
        assert!(iterable_for_each_indexed(&warm)
            .callable
            .inline_body_plan
            .is_some());
        assert!(iteration(
            &warm,
            "kotlin/collections",
            "forEach",
            "(Ljava/util/Map;Lkotlin/jvm/functions/Function1;)V",
        )
        .callable
        .inline_body_plan
        .is_some());
        assert!(iteration(
            &warm,
            "kotlin/text",
            "forEach",
            "(Ljava/lang/CharSequence;Lkotlin/jvm/functions/Function1;)V",
        )
        .callable
        .inline_body_plan
        .is_some());

        let current = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )));
        let function = iterable_for_each_indexed(&current);
        let Some(InlineBodyPlan::Iteration {
            index: Some(InlineIterationIndex::Checked { overflow }),
            ..
        }) = function.callable.inline_body_plan.as_deref()
        else {
            panic!("cached Iterable.forEachIndexed must retain its checked index plan")
        };
        let identity = overflow
            .callable
            .external_identity
            .expect("cached overflow call must receive a per-classpath identity");
        let realization = current
            .cp
            .external_callable(identity)
            .expect("overflow identity must resolve in the consuming classpath");
        assert_eq!(
            realization.kind,
            crate::jvm::classpath::ExternalCallableKind::TopLevel
        );
        assert_eq!(realization.callable.name, "throwIndexOverflow");
        assert!(realization.callable.params.is_empty());
        assert_eq!(realization.callable.ret, Ty::Unit);

        let map = iteration(
            &current,
            "kotlin/collections",
            "forEach",
            "(Ljava/util/Map;Lkotlin/jvm/functions/Function1;)V",
        );
        let Some(InlineBodyPlan::Iteration {
            traversal:
                InlineIterationTraversal::Iterator {
                    prepare,
                    has_next,
                    next,
                },
            ..
        }) = map.callable.inline_body_plan.as_deref()
        else {
            panic!("cached Map.forEach must retain its exact iterator chain")
        };
        assert_eq!(
            prepare.len(),
            2,
            "Map traversal includes entries and iterator"
        );
        for member in prepare.iter().chain([has_next.as_ref(), next.as_ref()]) {
            assert_current_member_identity(&current, member);
        }

        let text = iteration(
            &current,
            "kotlin/text",
            "forEach",
            "(Ljava/lang/CharSequence;Lkotlin/jvm/functions/Function1;)V",
        );
        let Some(InlineBodyPlan::Iteration {
            traversal: InlineIterationTraversal::Counted { size, get },
            ..
        }) = text.callable.inline_body_plan.as_deref()
        else {
            panic!("cached CharSequence.forEach must retain its exact counted calls")
        };
        assert_current_member_identity(&current, size);
        assert_current_member_identity(&current, get);
    }
}
