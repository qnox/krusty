//! Executable object reads joined to canonical declaration and initialization evidence.

use super::*;
use crate::libraries::KlibBodyClassContract;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::types::TypeName;

pub(super) fn source_objects(
    source: &CheckedIrFile<'_>,
    target_represents: &dyn Fn(TypeName) -> bool,
) -> Result<Vec<KlibBodyClassContract>, String> {
    let mut objects = Vec::new();
    for expression in crate::ir::emitted_body_roots(&source.ir)
        .into_iter()
        .flat_map(|root| crate::ir::value_namespace_expressions(&source.ir, root))
    {
        let IrExpr::SingletonValue { classifier } = source.ir.expr(expression) else {
            continue;
        };
        if source.ir.class_id_by_name(*classifier).is_some()
            || target_represents(*classifier)
            || source.classifiers.module().declares_instance(
                &crate::backend::instance_links::InstanceEntry::Object(*classifier),
            )
        {
            continue;
        }
        // A source module's own object can be declared in another file; its module already
        // certifies its instance entry. It has no serialized dependency initializer.
        let class = source
            .callables
            .object_declaration(*classifier)
            .ok_or_else(|| {
                format!(
                    "an emitted dependency object without its frozen declaration: {classifier:?}"
                )
            })?;
        if !objects.contains(class) {
            objects.push(class.clone());
        }
    }
    Ok(objects)
}

pub(super) struct ObjectObligations<'a> {
    pub(super) pending: &'a mut VecDeque<(KlibDependencyCallable, Option<KlibBodyClassContract>)>,
    pub(super) dispatched: &'a mut HashSet<KlibDependencyCallable>,
    pub(super) declarations:
        &'a mut Vec<(crate::fir::ExternalCallableId, ExternalCallableRealization)>,
    pub(super) declared: &'a mut HashSet<KlibDependencyCallable>,
}

#[derive(Default)]
pub(super) struct ObjectClosure {
    pub(super) classes: Vec<KlibBodyClassContract>,
    pub(super) published: Vec<(TypeName, crate::libraries::KlibClassIdentity)>,
    conflicts: HashSet<TypeName>,
}

impl ObjectClosure {
    pub(super) fn body(
        &mut self,
        identity: &KlibDependencyCallable,
        platform: &dyn SemanticPlatform,
        bodies: &KlibDeclarationBodies,
        capabilities: &super::super::DependencyLeafCapabilities<'_>,
        obligations: &mut ObjectObligations<'_>,
    ) -> Result<(), String> {
        for signature in
            bodies.emitted_body_objects(identity, crate::klib_lowering::EMITTED_CALL_FORMS)
        {
            self.serialized(
                (&identity.module, &signature),
                platform,
                bodies,
                capabilities,
                obligations,
            )?;
        }
        Ok(())
    }

    pub(super) fn admit(
        &mut self,
        class: KlibBodyClassContract,
        platform: &dyn SemanticPlatform,
        bodies: &KlibDeclarationBodies,
        capabilities: &super::super::DependencyLeafCapabilities<'_>,
        obligations: &mut ObjectObligations<'_>,
    ) -> Result<(), String> {
        if (capabilities.target_represents_classifier)(class.classifier) {
            return Ok(());
        }
        if self.conflicts.contains(&class.classifier) {
            return Err("conflicting object producer contracts".into());
        }
        if let Some(old) = self
            .classes
            .iter()
            .find(|old| old.classifier == class.classifier)
        {
            return if old == &class {
                Ok(())
            } else {
                self.conflicts.insert(class.classifier);
                Err("conflicting object producer contracts".into())
            };
        }
        if !class.is_object() {
            return Err("an object use selected a non-object declaration".into());
        }
        if (capabilities.has_object_instance)(&class) {
            self.published.push((class.classifier, class.identity()));
            self.classes.push(class);
            return Ok(());
        }
        let primary = class
            .primary_constructor
            .clone()
            .ok_or("an object without a private primary constructor")?;
        let identity = KlibDependencyCallable {
            module: class.module.clone(),
            declaration: crate::libraries::KlibDeclarationSignature::Public(primary),
        };
        let superclasses = superclass_realizations(platform, bodies, &identity);
        let facts = with_realized_declarations(capabilities.callees.clone(), &superclasses);
        let initialization = facts
            .object_initialization_callees(&class, bodies)
            .map_err(|reason| format!("an invalid object initialization: {reason:?}"))?
            .ok_or("an object without admitted ordinary initialization")?;
        let object_reads = facts
            .object_initialization_objects(&class, bodies)
            .map_err(|reason| format!("an invalid object initializer read: {reason:?}"))?;
        obligations.pending.extend(
            initialization
                .into_iter()
                .map(|identity| (identity, Some(class.clone()))),
        );
        obligations.dispatched.extend(
            facts
                .object_dispatched_members(&class, bodies)
                .map_err(|reason| format!("an invalid object member: {reason:?}"))?,
        );
        for overridden in facts
            .object_overridden_declarations(&class, bodies)
            .map_err(|reason| format!("an invalid object override: {reason:?}"))?
        {
            if obligations.declared.insert(overridden.clone()) {
                obligations.declarations.push(
                    platform
                        .external_callable_by_dependency(&overridden)
                        .ok_or_else(|| {
                            format!(
                                "an overridden object declaration has no provider: {overridden:?}"
                            )
                        })?,
                );
            }
        }
        let module = class.module.clone();
        self.classes.push(class);
        for signature in object_reads {
            self.serialized(
                (&module, &signature),
                platform,
                bodies,
                capabilities,
                obligations,
            )?;
        }
        Ok(())
    }

    pub(super) fn serialized(
        &mut self,
        origin: (&str, &KlibPublicIdSignature),
        platform: &dyn SemanticPlatform,
        bodies: &KlibDeclarationBodies,
        capabilities: &super::super::DependencyLeafCapabilities<'_>,
        obligations: &mut ObjectObligations<'_>,
    ) -> Result<(), String> {
        let (module, signature) = origin;
        let classifier = crate::klib_lowering::classifier_name(signature)
            .map_err(|reason| format!("an invalid object class symbol: {reason:?}"))?;
        if (capabilities.target_represents_classifier)(classifier) {
            return Ok(());
        }
        let class = platform
            .klib_body_class_contract(classifier)
            .ok_or("a serialized object read without its selected class declaration")?;
        let origin = bodies
            .class_origin(module, signature)
            .map_err(|reason| format!("an ambiguous serialized object producer: {reason}"))?;
        let exact_origin = match origin {
            Some(origin) => origin == class.identity(),
            // A source object emitted earlier may not have serialized IR in these inputs. Only
            // its exact, successfully defined instance receipt replaces that missing body proof.
            None => {
                (capabilities.has_object_instance)(&class)
                    && bodies.body_provider_modules(module).contains(&class.module)
            }
        };
        if class.signature != *signature
            || class.classifier != classifier
            || !class.is_object()
            || !exact_origin
        {
            return Err("a serialized object read disagrees with its producer class".into());
        }
        self.admit(class, platform, bodies, capabilities, obligations)
    }
}
