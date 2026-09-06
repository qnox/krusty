use crate::fir::{
    BodyOwnerId, CallableId, CheckedBodySink, DeclarationId, DeclarationKind, FirBody,
    InlineBodyStore, OriginId, ResolvedModuleIndex, SourceFileId,
};
use crate::ir::{FnParamInfo, IrFile, IrFunction};

use super::{
    constructors::{
        accept_constructor_body, finalize_constructor_field_indices, finalize_constructors,
        predeclare_constructors,
    },
    data_classes::finalize_data_classes,
    finish_callable_body,
    generics::{attach_callable_generic_facts, attach_classifier_generic_facts},
    initialization::{accept_non_callable_body, finalize_enum_entries},
    interface_delegation::{
        finalize_interface_delegations, predeclare_interface_delegation_fields,
    },
    lower_body_with_context,
    properties::{accept_property_body, finalize_properties, predeclare_properties},
    tailrec::finish_tailrec_body,
    FirLoweringFailure, LocalCallableLoweringContext,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirFileLoweringFailure {
    Body(FirLoweringFailure),
    MissingCallable(DeclarationId),
    MissingProperty(DeclarationId),
    MissingSourceOrder(DeclarationId),
    MissingSourcePackage(crate::fir::SourceFileId),
    UnsupportedPropertyShape(DeclarationId),
    MissingClassifier(DeclarationId),
    MissingAnnotationPolicy(DeclarationId),
    MissingModuleClassifier(crate::types::TypeName),
    UnsupportedCallableOwner(DeclarationId),
    UnsupportedInlinePayloadOwner {
        root: DeclarationId,
        declaration: DeclarationId,
    },
    MissingResultType(DeclarationId),
    ResultTypeMismatch(DeclarationId),
    DuplicateBody(CallableId),
    DuplicateNonCallableBody(DeclarationId),
    InlineBodyOwnerMismatch(CallableId),
    InvalidDelegatedCallShape {
        expected: u32,
        actual: u32,
    },
    UndeterminedType(crate::ir::UndeterminedIrType),
    ValueIdentityOverflow,
}

fn lower_property_override_plans(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Vec<crate::ir::IrPropertyOverride> {
    index
        .property_overrides(declaration)
        .iter()
        .map(|edge| crate::ir::IrPropertyOverride {
            implementation: edge.implementation,
            implementation_owner: edge.implementation_owner,
            overridden: edge.overridden,
            overridden_owner: edge.overridden_owner,
            overridden_is_interface: edge.overridden_is_interface,
            name: edge.name.to_string(),
            declared_type: edge.declared_type.get(),
            applied_type: edge.applied_type.get(),
            implementation_type: edge.implementation_type.get(),
            overridden_mutable: edge.overridden_mutable,
            implementation_mutable: edge.implementation_mutable,
            depth: edge.depth,
        })
        .collect()
}

fn lower_function_override_plans(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Vec<crate::ir::IrFunctionOverride> {
    index
        .function_overrides(declaration)
        .iter()
        .map(|edge| crate::ir::IrFunctionOverride {
            implementation: edge.implementation,
            implementation_function: None,
            implementation_owner: edge.implementation_owner,
            overridden: edge.overridden,
            overridden_owner: edge.overridden_owner,
            overridden_is_interface: edge.overridden_is_interface,
            name: edge.name.to_string(),
            declared_parameters: edge.declared_parameters.iter().map(|ty| ty.get()).collect(),
            declared_result: edge.declared_result.get(),
            applied_parameters: edge.applied_parameters.iter().map(|ty| ty.get()).collect(),
            applied_result: edge.applied_result.get(),
            implementation_parameters: edge
                .implementation_parameters
                .iter()
                .map(|ty| ty.get())
                .collect(),
            implementation_result: edge.implementation_result.get(),
            suspend: edge.suspend,
            depth: edge.depth,
        })
        .collect()
}

/// One-file consuming FIR sink. It owns no syntax or checker side table: stable module headers
/// predeclare common-IR functions, and each accepted body is lowered and attached exactly once.
pub struct CommonIrBodySink<'a> {
    source: SourceFileId,
    ir: &'a mut IrFile,
    failure: Option<FirFileLoweringFailure>,
    local_callables: LocalCallableLoweringContext,
    inline_payload_declarations: std::collections::HashSet<DeclarationId>,
    materialized_inline_callables: std::collections::HashSet<CallableId>,
}

impl<'a> CommonIrBodySink<'a> {
    pub fn new(
        index: &ResolvedModuleIndex,
        source: SourceFileId,
        ir: &'a mut IrFile,
    ) -> Result<Self, FirFileLoweringFailure> {
        let mut sink = Self {
            source,
            ir,
            failure: None,
            local_callables: LocalCallableLoweringContext::default(),
            inline_payload_declarations: std::collections::HashSet::new(),
            materialized_inline_callables: std::collections::HashSet::new(),
        };
        sink.predeclare_classifiers(index, true)?;
        predeclare_properties(
            index,
            sink.source,
            &sink.inline_payload_declarations,
            sink.ir,
        )?;
        predeclare_interface_delegation_fields(
            index,
            sink.source,
            &sink.inline_payload_declarations,
            sink.ir,
        )?;
        sink.predeclare_functions(index)?;
        predeclare_constructors(
            index,
            sink.source,
            &sink.inline_payload_declarations,
            sink.ir,
            true,
        )?;
        super::package_declarations::publish(index, sink.source, sink.ir)?;
        Ok(sink)
    }

    pub fn finish(mut self, index: &ResolvedModuleIndex) -> Result<(), FirFileLoweringFailure> {
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        // The initial sink is created before Pass 2 has entered any body-local lexical rung. Every
        // such classifier must be complete now; repeat the idempotent declaration publication with
        // deferral disabled so an unvisited/missing local header is an error rather than silently
        // producing partial common IR.
        self.predeclare_classifiers(index, false)?;
        predeclare_properties(
            index,
            self.source,
            &self.inline_payload_declarations,
            self.ir,
        )?;
        predeclare_interface_delegation_fields(
            index,
            self.source,
            &self.inline_payload_declarations,
            self.ir,
        )?;
        predeclare_constructors(
            index,
            self.source,
            &self.inline_payload_declarations,
            self.ir,
            false,
        )?;
        self.predeclare_functions(index)?;
        finalize_constructors(index, self.ir)?;
        finalize_enum_entries(self.ir)?;
        finalize_properties(index, self.ir)?;
        finalize_constructor_field_indices(index, self.ir)?;
        finalize_interface_delegations(index, self.ir)?;
        finalize_data_classes(index, self.ir)?;
        super::module_declarations::publish_referenced(index, self.ir)?;
        self.ir
            .validate_determined_types()
            .map_err(FirFileLoweringFailure::UndeterminedType)
    }

    pub fn attached_body_count(&self) -> usize {
        self.ir
            .functions
            .iter()
            .filter(|function| function.body.is_some())
            .count()
    }

    pub(crate) fn ir_mut(&mut self) -> &mut IrFile {
        self.ir
    }

    /// Add body-local callables/properties whose pending-free signatures were published by the
    /// active declaration group. Body-local classifier headers are also published at this Pass-2
    /// boundary, so their skeletons must precede constructors, properties, and methods from the
    /// same group.
    pub fn refresh_body_local_declarations(
        &mut self,
        index: &ResolvedModuleIndex,
    ) -> Result<(), FirFileLoweringFailure> {
        self.predeclare_classifiers(index, true)?;
        predeclare_properties(
            index,
            self.source,
            &self.inline_payload_declarations,
            self.ir,
        )?;
        predeclare_interface_delegation_fields(
            index,
            self.source,
            &self.inline_payload_declarations,
            self.ir,
        )?;
        super::constructors::predeclare_constructors(
            index,
            self.source,
            &self.inline_payload_declarations,
            self.ir,
            true,
        )?;
        self.predeclare_functions(index)
    }

    /// Consume retained Pass-1 inline FIR before any ordinary caller is accepted.
    pub fn accept_inline_bodies(
        &mut self,
        index: &ResolvedModuleIndex,
        bodies: &InlineBodyStore,
    ) -> Result<(), FirFileLoweringFailure> {
        let mut callables = bodies
            .bodies_for_source(index, self.source)
            .into_iter()
            .map(|(callable, _)| callable)
            .collect::<Vec<_>>();
        callables.sort_unstable_by_key(|callable| callable.raw());
        let mut visiting = std::collections::HashSet::new();
        for callable in callables {
            self.accept_inline_payload_tree(index, bodies, callable, &mut visiting)?;
        }
        Ok(())
    }

    pub(crate) fn accept_inline_dependencies(
        &mut self,
        index: &ResolvedModuleIndex,
        bodies: &InlineBodyStore,
        caller: &FirBody,
    ) -> Result<(), FirFileLoweringFailure> {
        let mut pending = std::collections::HashSet::new();
        caller.collect_referenced_module_callables(&mut pending);
        let mut pending = pending.into_iter().collect::<Vec<_>>();
        pending.sort_unstable_by_key(|callable| callable.raw());
        let mut visiting = std::collections::HashSet::new();
        for callable in pending {
            self.accept_inline_payload_tree(index, bodies, callable, &mut visiting)?;
        }
        Ok(())
    }

    /// Materialize retained inline templates dependency-first. Their checked FIR is stored in a
    /// hash map, so iteration order cannot define whether a nested inline call sees its callee's
    /// common-IR template. A recursive inline cycle is left as a selected physical call at the
    /// cycle edge; valid acyclic chains are fully expanded independent of map order.
    fn accept_inline_payload_tree(
        &mut self,
        index: &ResolvedModuleIndex,
        bodies: &InlineBodyStore,
        callable: CallableId,
        visiting: &mut std::collections::HashSet<CallableId>,
    ) -> Result<(), FirFileLoweringFailure> {
        if self.materialized_inline_callables.contains(&callable) {
            return Ok(());
        }
        let Some(body) = bodies.get(callable).cloned() else {
            return Ok(());
        };
        if !visiting.insert(callable) {
            return Ok(());
        }
        let declaration = DeclarationId::from_raw(body.owner().raw());
        let anchor = index
            .declaration_anchor(declaration)
            .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
        if index
            .callable_for_declaration(declaration)
            .is_none_or(|header| header.id != callable || !header.is_inline())
        {
            return Err(FirFileLoweringFailure::InlineBodyOwnerMismatch(callable));
        }
        if anchor.source != self.source {
            self.predeclare_foreign_inline_template(index, callable)?;
        }
        self.predeclare_inline_payload(index, declaration, &body)?;
        let mut dependencies = std::collections::HashSet::new();
        body.collect_referenced_module_callables(&mut dependencies);
        let mut dependencies = dependencies.into_iter().collect::<Vec<_>>();
        dependencies.sort_unstable_by_key(|dependency| dependency.raw());
        for dependency in dependencies {
            self.accept_inline_payload_tree(index, bodies, dependency, visiting)?;
        }
        visiting.remove(&callable);

        let nested = body.inline_nested_declaration_bodies().to_vec();
        if let Err(error) = self.accept_body(index, body.owner(), body) {
            crate::trace_compiler!(
                "lower",
                "inline payload root lowering failed callable={callable:?} declaration={declaration:?}: {error:?}",
            );
            return Err(error);
        }
        for body in nested {
            let nested_declaration = DeclarationId::from_raw(body.owner().raw());
            if let Err(error) = self.accept_body(index, body.owner(), body) {
                crate::trace_compiler!(
                    "lower",
                    "inline payload nested lowering failed callable={callable:?} declaration={nested_declaration:?}: {error:?}",
                );
                return Err(error);
            }
        }
        self.materialized_inline_callables.insert(callable);
        Ok(())
    }

    pub(crate) fn accept_streamed_body(
        &mut self,
        index: &ResolvedModuleIndex,
        inline_bodies: &InlineBodyStore,
        owner: BodyOwnerId,
        body: FirBody,
    ) -> Result<(), FirFileLoweringFailure> {
        self.accept_inline_dependencies(index, inline_bodies, &body)?;
        self.accept_body(index, owner, body)
    }

    fn predeclare_inline_payload(
        &mut self,
        index: &ResolvedModuleIndex,
        root: DeclarationId,
        body: &FirBody,
    ) -> Result<(), FirFileLoweringFailure> {
        // Accessor bodies are lexically owned by their property: declarations discovered inside a
        // getter/setter therefore reach the property, rather than the accessor callable, when we
        // walk their stable ownership chain. The property is the payload boundary in that shape;
        // it is not itself a declaration copied by the inline accessor.
        let lexical_root = index
            .declaration_header(root)
            .and_then(|header| {
                (header.kind == DeclarationKind::Accessor)
                    .then_some(header.owner)
                    .flatten()
            })
            .unwrap_or(root);
        let mut declarations = std::collections::HashSet::new();
        body.collect_inline_local_declarations(&mut declarations);
        for nested in body.inline_nested_declaration_bodies() {
            let mut declaration = DeclarationId::from_raw(nested.owner().raw());
            loop {
                if declaration == root || declaration == lexical_root {
                    break;
                }
                declarations.insert(declaration);
                let Some(owner) = index
                    .local_classifier_lexical_root(declaration)
                    .or_else(|| {
                        index
                            .declaration_header(declaration)
                            .and_then(|header| header.owner)
                    })
                else {
                    return Err(FirFileLoweringFailure::UnsupportedInlinePayloadOwner {
                        root,
                        declaration,
                    });
                };
                declaration = owner;
            }
        }
        // A property synthesized from a local-class constructor parameter has no independent body,
        // so it cannot be discovered from the retained nested-body list. Close the declaration set
        // over stable ownership to carry every header belonging to the selected local classifiers.
        loop {
            let mut changed = false;
            for raw in 0..index.declaration_count() {
                let declaration = DeclarationId::from_raw(raw as u32);
                let Some(owner) = index
                    .declaration_anchor(declaration)
                    .and_then(|anchor| anchor.owner)
                else {
                    continue;
                };
                if declarations.contains(&owner) && declarations.insert(declaration) {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.inline_payload_declarations.extend(declarations);
        self.predeclare_classifiers(index, false)?;
        predeclare_properties(
            index,
            self.source,
            &self.inline_payload_declarations,
            self.ir,
        )?;
        predeclare_interface_delegation_fields(
            index,
            self.source,
            &self.inline_payload_declarations,
            self.ir,
        )?;
        predeclare_constructors(
            index,
            self.source,
            &self.inline_payload_declarations,
            self.ir,
            false,
        )?;
        self.predeclare_functions(index)
    }

    fn predeclare_foreign_inline_template(
        &mut self,
        index: &ResolvedModuleIndex,
        callable_id: crate::fir::CallableId,
    ) -> Result<(), FirFileLoweringFailure> {
        if self
            .ir
            .checked_callable_functions
            .contains_key(&callable_id)
        {
            return Ok(());
        }
        let callable =
            index
                .callable(callable_id)
                .ok_or(FirFileLoweringFailure::MissingCallable(
                    DeclarationId::from_raw(callable_id.raw()),
                ))?;
        let declaration = callable.declaration;
        let signature = index
            .signature(declaration)
            .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
        let flags = index
            .declaration_header(declaration)
            .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?
            .flags;
        let companion_associated = flags.has(crate::fir::DeclarationFlags::COMPANION);
        let mut params = signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        let mut names = (0..index.callable_parameter_name_count(callable.id))
            .filter_map(|ordinal| {
                index
                    .callable_parameter_name(callable.id, ordinal as u32)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        if !companion_associated {
            if let Some(receiver) = callable.shape.extension_receiver {
                let position = callable.shape.context_parameter_count as usize;
                if position > params.len() || position > names.len() {
                    return Err(FirFileLoweringFailure::MissingCallable(declaration));
                }
                params.insert(position, receiver.get());
                names.insert(position, "$this$inline".to_string());
            }
        }
        let dispatch_receiver = index
            .enclosing_classifier(declaration)
            .map(|classifier| classifier.classifier);
        let function = self.ir.add_fun(IrFunction {
            name: index
                .callable_name(callable.id)
                .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?
                .to_owned(),
            param_checks: vec![None; params.len()],
            params,
            ret: signature.result.get(),
            body: None,
            is_static: dispatch_receiver.is_none(),
            dispatch_receiver,
        });
        self.ir.inline_fns.insert(function);
        self.ir.inline_only_fns.insert(function);
        self.ir.foreign_inline_templates.insert(function);
        if callable.shape.extension_receiver.is_some() && !companion_associated {
            self.ir.extension_receiver_fns.insert(function);
        }
        if callable.shape.context_parameter_count != 0 {
            self.ir
                .fn_context_counts
                .insert(function, callable.shape.context_parameter_count as usize);
        }
        if flags.has(crate::fir::DeclarationFlags::SUSPEND) {
            self.ir.suspend_funs.push(function);
        }
        self.ir
            .fn_params
            .insert(function, FnParamInfo::names(names));
        attach_callable_generic_facts(index, declaration, function, self.ir);
        self.ir
            .checked_callable_functions
            .insert(callable.id, function);
        Ok(())
    }

    /// Consume Pass-1 checked parameter defaults after the owning ordinary constructor/function
    /// body has been attached. The store is drained per source, so no default FIR survives its
    /// target file's common-lowering boundary.
    pub fn accept_default_arguments(
        &mut self,
        index: &ResolvedModuleIndex,
        bodies: &mut crate::fir::DefaultArgumentStore,
    ) -> Result<(), FirFileLoweringFailure> {
        for (callable, body) in bodies.take_for_source(index, self.source) {
            let declaration = DeclarationId::from_raw(body.owner().raw());
            if index
                .callable_for_declaration(declaration)
                .is_none_or(|header| header.id != callable)
            {
                return Err(FirFileLoweringFailure::MissingCallable(declaration));
            }
            self.accept_body(index, body.owner(), body)?;
        }
        Ok(())
    }

    pub fn indexed<'sink>(
        &'sink mut self,
        index: &'sink ResolvedModuleIndex,
    ) -> IndexedCommonIrBodySink<'sink, 'a> {
        IndexedCommonIrBodySink { sink: self, index }
    }

    fn predeclare_classifiers(
        &mut self,
        index: &ResolvedModuleIndex,
        allow_deferred_body_local: bool,
    ) -> Result<(), FirFileLoweringFailure> {
        let mut newly_declared = std::collections::HashSet::new();
        for raw in 0..index.declaration_count() {
            let declaration = DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable declarations for a packed id"),
            );
            let Some(anchor) = index.declaration_anchor(declaration) else {
                continue;
            };
            crate::trace_compiler!(
                "lower",
                "predeclare declaration={declaration:?} source={:?} active_source={:?} kind={:?} owner={:?} name={:?}",
                anchor.source,
                self.source,
                anchor.kind,
                anchor.owner,
                index.declaration_name(declaration),
            );
            if (anchor.source != self.source
                && !self.inline_payload_declarations.contains(&declaration))
                || anchor.kind != DeclarationKind::Classifier
            {
                continue;
            }
            if self
                .ir
                .checked_classifier_classes
                .contains_key(&declaration)
            {
                if index.declaration_header(declaration).is_some_and(|header| {
                    header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                }) && index.has_property_override_plan(declaration)
                    && index.has_function_override_plan(declaration)
                {
                    let classifier_identity = index
                        .classifier_header(declaration)
                        .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?
                        .classifier;
                    // A local classifier may have a stable skeleton before an inferred member is
                    // checked. Refresh only the semantic override payload once Pass 2 marks both
                    // plans complete; class identity and all already-attached bodies stay intact.
                    self.ir.property_overrides.insert(
                        classifier_identity,
                        lower_property_override_plans(index, declaration),
                    );
                    self.ir.function_overrides.insert(
                        classifier_identity,
                        lower_function_override_plans(index, declaration),
                    );
                }
                continue;
            }
            // Multiplatform actualization deliberately retains stable source anchors while removing
            // matched `expect` headers and their body work before Pass 2. Such an anchor has no common
            // declaration to realize; a present declaration header without its classifier header is
            // still an invalid index and remains an error below.
            let Some(declaration_header) = index.declaration_header(declaration) else {
                continue;
            };
            let Some(header) = index.classifier_header(declaration) else {
                if allow_deferred_body_local
                    && declaration_header
                        .flags
                        .has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                {
                    continue;
                }
                return Err(FirFileLoweringFailure::MissingClassifier(declaration));
            };
            let classifier_identity = header.classifier;
            let mut class = crate::ir::IrClass::source_skeleton(header, declaration_header.flags);
            if declaration_header.visibility != crate::types::Visibility::Public {
                assert!(
                    self.ir
                        .class_visibilities
                        .insert(classifier_identity, declaration_header.visibility)
                        .is_none(),
                    "a stable classifier may publish one visibility into common IR"
                );
            }
            if declaration_header
                .flags
                .has(crate::fir::DeclarationFlags::ANNOTATION_CLASS)
            {
                class.annotation_retention = Some(
                    index
                        .annotation_retention(classifier_identity)
                        .ok_or(FirFileLoweringFailure::MissingAnnotationPolicy(declaration))?,
                );
            }
            attach_classifier_generic_facts(index, declaration, header, &mut class, self.ir);
            let hierarchy = index
                .classifier_hierarchy(declaration)
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?
                .iter()
                .map(|entry| crate::ir::IrAppliedClassifier {
                    classifier: entry.classifier,
                    applied: entry.applied.get(),
                    depth: entry.depth,
                })
                .collect::<Vec<_>>();
            assert!(
                self.ir
                    .classifier_hierarchies
                    .insert(classifier_identity, hierarchy)
                    .is_none(),
                "a source classifier may publish one applied hierarchy per IR file"
            );
            let property_overrides = lower_property_override_plans(index, declaration);
            assert!(
                self.ir
                    .property_overrides
                    .insert(classifier_identity, property_overrides)
                    .is_none(),
                "a source classifier may publish property override edges once"
            );
            let function_overrides = lower_function_override_plans(index, declaration);
            assert!(
                self.ir
                    .function_overrides
                    .insert(classifier_identity, function_overrides)
                    .is_none(),
                "a source classifier may publish function override edges once"
            );
            let class = self.ir.add_class(class);
            assert!(
                self.ir
                    .checked_classifier_classes
                    .insert(declaration, class)
                    .is_none(),
                "a stable classifier has one common-IR realization per file"
            );
            newly_declared.insert(declaration);
            let mut type_aliases = Vec::new();
            for alias_raw in 0..index.declaration_count() {
                let alias = DeclarationId::from_raw(
                    u32::try_from(alias_raw).expect("too many stable declarations for a packed id"),
                );
                let Some(alias_anchor) = index.declaration_anchor(alias) else {
                    continue;
                };
                if alias_anchor.owner != Some(declaration)
                    || alias_anchor.kind != DeclarationKind::TypeAlias
                {
                    continue;
                }
                let Some(alias_header) = index.type_alias_header(alias) else {
                    if declaration_header
                        .flags
                        .has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    {
                        continue;
                    }
                    return Err(FirFileLoweringFailure::UnsupportedCallableOwner(alias));
                };
                let name = index
                    .declaration_name(alias)
                    .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(alias))?
                    .to_owned();
                let mut formals = Vec::new();
                for ordinal in 0.. {
                    let Some(parameter) = index.type_parameter(alias, ordinal) else {
                        break;
                    };
                    formals.push(
                        index
                            .type_parameter_name(parameter)
                            .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(alias))?
                            .to_owned(),
                    );
                }
                type_aliases.push(crate::ir::IrTypeAlias {
                    name,
                    formals,
                    expansion: alias_header.expansion.get(),
                    visibility: index
                        .declaration_header(alias)
                        .expect("a published alias must retain its declaration header")
                        .visibility,
                    expansion_spelling: alias_header.expansion_spelling.clone(),
                    source_order: index
                        .source_order(alias)
                        .ok_or(FirFileLoweringFailure::MissingSourceOrder(alias))?,
                });
            }
            if !type_aliases.is_empty() {
                assert!(
                    self.ir
                        .class_type_aliases
                        .insert(classifier_identity, type_aliases)
                        .is_none(),
                    "a source classifier may publish its type aliases only once"
                );
            }
            for entry_raw in 0..index.declaration_count() {
                let entry = DeclarationId::from_raw(
                    u32::try_from(entry_raw).expect("too many stable declarations for a packed id"),
                );
                let Some(entry_anchor) = index.declaration_anchor(entry) else {
                    continue;
                };
                if entry_anchor.owner != Some(declaration)
                    || entry_anchor.kind != DeclarationKind::EnumEntry
                {
                    continue;
                }
                let name = index
                    .declaration_name(entry)
                    .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(entry))?
                    .to_owned();
                let has_body = (0..index.declaration_count()).any(|child_raw| {
                    let child = DeclarationId::from_raw(
                        u32::try_from(child_raw)
                            .expect("too many stable declarations for a packed id"),
                    );
                    index
                        .declaration_anchor(child)
                        .is_some_and(|child_anchor| child_anchor.owner == Some(entry))
                });
                let subclass = has_body.then(|| header.classifier.nested_child(&name));
                self.ir.classes[class as usize]
                    .enum_entries
                    .push(crate::ir::IrEnumEntry {
                        name,
                        argument_prelude: Vec::new(),
                        args: Vec::new(),
                        default_parameters: Vec::new(),
                        decl_line: 0,
                        subclass,
                    });
                if let Some(subclass) = subclass {
                    let mut entry_class = crate::ir::IrClass::synthetic(subclass);
                    entry_class.superclass = header.classifier;
                    // The selected enum-entry constructor is executable FIR and may be a secondary
                    // constructor with a different parameter list. Finalization installs that
                    // already-selected list after the entry body arrives; predeclaration only marks
                    // this skeleton as an enum-entry subclass.
                    entry_class.enum_entry_of = Some(Vec::new());
                    let entry_class = self.ir.add_class(entry_class);
                    assert!(
                        self.ir
                            .property_overrides
                            .insert(subclass, lower_property_override_plans(index, entry))
                            .is_none(),
                        "an enum-entry subclass may publish property override edges once"
                    );
                    assert!(
                        self.ir
                            .function_overrides
                            .insert(subclass, lower_function_override_plans(index, entry))
                            .is_none(),
                        "an enum-entry subclass may publish function override edges once"
                    );
                    assert!(
                        self.ir
                            .checked_enum_entry_classes
                            .insert(entry, entry_class)
                            .is_none(),
                        "a stable enum entry has one common-IR subclass per file"
                    );
                }
            }
        }
        for raw in 0..index.declaration_count() {
            let declaration = DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable declarations for a packed id"),
            );
            if !newly_declared.contains(&declaration) {
                continue;
            }
            let Some(header) = index.declaration_header(declaration) else {
                continue;
            };
            if !header.flags.has(crate::fir::DeclarationFlags::INNER) {
                continue;
            }
            let anchor = index
                .declaration_anchor(declaration)
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
            if (anchor.source != self.source
                && !self.inline_payload_declarations.contains(&declaration))
                || anchor.kind != DeclarationKind::Classifier
            {
                continue;
            }
            let outer = anchor
                .owner
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
            let class = self
                .ir
                .checked_classifier_classes
                .get(&declaration)
                .copied()
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
            // The stable lexical owner may be an enum-entry declaration rather than a classifier
            // header. Pass 1 has already made that ownership exact; map it to the entry subclass
            // skeleton created above instead of walking out to the parent enum or guessing by name.
            let outer_ty = if let Some(outer) = index.classifier_header(outer) {
                crate::types::Ty::obj_name(outer.classifier)
            } else if let Some(outer) = self.ir.checked_enum_entry_classes.get(&outer).copied() {
                crate::types::Ty::obj_name(self.ir.classes[outer as usize].fq_name)
            } else {
                return Err(FirFileLoweringFailure::MissingClassifier(outer));
            };
            let class = &mut self.ir.classes[class as usize];
            for argument in &mut class.ctor_args {
                if let Some(field) = &mut argument.field_index {
                    *field = field
                        .checked_add(1)
                        .expect("inner-class field index overflow");
                }
            }
            class.fields.insert(
                0,
                crate::ir::IrField::new("this$0".to_owned(), outer_ty).with_is_final(true),
            );
            class.ctor_args.insert(
                0,
                crate::ir::IrCtorArg {
                    // The enclosing instance is a physical constructor prefix, not a Kotlin value
                    // parameter. Leaving it unnamed keeps it out of constructor metadata while the
                    // JVM descriptor and field store still retain the slot.
                    name: None,
                    ty: outer_ty,
                    declared_ty: None,
                    is_field: true,
                    field_index: Some(0),
                    has_default: false,
                    is_vararg: false,
                    type_param: None,
                    check: None,
                },
            );
            class.ctor_param_count += 1;
            class.constructor_prefix_count += 1;
            class.pre_super_param_fields.push((0, 0));
        }
        for raw in 0..index.declaration_count() {
            let declaration = DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable declarations for a packed id"),
            );
            if !newly_declared.contains(&declaration) {
                continue;
            }
            let Some(header) = index.declaration_header(declaration) else {
                continue;
            };
            if !header.flags.has(crate::fir::DeclarationFlags::COMPANION) {
                continue;
            }
            let anchor = index
                .declaration_anchor(declaration)
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
            // The flag also marks associated companion-extension functions/properties. Only an
            // actual classifier declaration contributes an outer-to-companion singleton edge.
            if (anchor.source != self.source
                && !self.inline_payload_declarations.contains(&declaration))
                || anchor.kind != DeclarationKind::Classifier
            {
                continue;
            }
            let outer_declaration = anchor
                .owner
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
            let companion = index
                .classifier_header(declaration)
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?
                .classifier;
            let outer = self
                .ir
                .checked_classifier_classes
                .get(&outer_declaration)
                .copied()
                .ok_or(FirFileLoweringFailure::MissingClassifier(outer_declaration))?;
            self.ir.classes[outer as usize].companion_class = Some(companion);
        }
        Ok(())
    }

    fn predeclare_functions(
        &mut self,
        index: &ResolvedModuleIndex,
    ) -> Result<(), FirFileLoweringFailure> {
        for raw in 0..index.declaration_count() {
            let declaration = DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable declarations for a packed id"),
            );
            let Some(anchor) = index.declaration_anchor(declaration) else {
                continue;
            };
            if (anchor.source != self.source
                && !self.inline_payload_declarations.contains(&declaration))
                || anchor.kind != DeclarationKind::Function
            {
                continue;
            }
            let Some(declaration_header) = index.declaration_header(declaration) else {
                // An actualized-away `expect` retains its stable anchor but has no published header.
                continue;
            };
            if index.is_suppressed_generated_callable(declaration) {
                continue;
            }
            let Some(callable) = index.callable_for_declaration(declaration) else {
                if index.is_body_local_declaration(declaration) {
                    continue;
                }
                return Err(FirFileLoweringFailure::MissingCallable(declaration));
            };
            if self
                .ir
                .checked_callable_functions
                .contains_key(&callable.id)
            {
                continue;
            }
            let Some(signature) = index.signature(declaration) else {
                if index.is_body_local_declaration(declaration) {
                    continue;
                }
                return Err(FirFileLoweringFailure::MissingCallable(declaration));
            };
            let compiler_generated = declaration_header
                .flags
                .has(crate::fir::DeclarationFlags::COMPILER_GENERATED);
            // A companion-block declaration is represented during lookup as an extension on the
            // associated classifier, but the checked FIR deliberately carries no value receiver.
            // The association is a name-resolution coordinate, not a common-IR parameter.
            let companion_associated = declaration_header
                .flags
                .has(crate::fir::DeclarationFlags::COMPANION);
            let mut params = signature
                .parameters
                .iter()
                .map(|parameter| {
                    if compiler_generated {
                        crate::types::stored_value_ty(parameter.get())
                    } else {
                        parameter.get()
                    }
                })
                .collect::<Vec<_>>();
            let mut names = (0..index.callable_parameter_name_count(callable.id))
                .map(|ordinal| {
                    index
                        .callable_parameter_name(callable.id, ordinal as u32)
                        .expect("published parameter-name count must address every name")
                        .to_owned()
                })
                .collect::<Vec<_>>();
            if !companion_associated {
                if let Some(receiver) = callable.shape.extension_receiver {
                    let position = callable.shape.context_parameter_count as usize;
                    if position > params.len() || position > names.len() {
                        return Err(FirFileLoweringFailure::MissingCallable(declaration));
                    }
                    params.insert(position, receiver.get());
                    names.insert(
                        position,
                        format!(
                            "$this${}",
                            index.callable_name(callable.id).unwrap_or("extension")
                        ),
                    );
                }
            }
            let enclosing = index.enclosing_classifier(declaration);
            let entry_class = anchor
                .owner
                .and_then(|owner| self.ir.checked_enum_entry_classes.get(&owner).copied());
            let class = entry_class
                .map(Ok)
                .or_else(|| {
                    enclosing.map(|classifier| {
                        self.ir
                            .checked_classifier_classes
                            .get(&classifier.declaration)
                            .copied()
                            .ok_or(FirFileLoweringFailure::MissingClassifier(
                                classifier.declaration,
                            ))
                    })
                })
                .transpose()?;
            let function = self.ir.add_fun(IrFunction {
                name: index
                    .callable_name(callable.id)
                    .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?
                    .to_owned(),
                param_checks: vec![None; params.len()],
                params,
                ret: if compiler_generated {
                    crate::types::stored_value_ty(signature.result.get())
                } else {
                    signature.result.get()
                },
                body: None,
                is_static: class.is_none(),
                dispatch_receiver: class.map(|class| self.ir.classes[class as usize].fq_name_id()),
            });
            self.ir.fn_source_order.insert(
                function,
                index
                    .source_order(declaration)
                    .ok_or(FirFileLoweringFailure::MissingSourceOrder(declaration))?,
            );
            if let Some(bound) = index.callable_equality_bound(callable.id) {
                self.ir.fn_equality_bounds.insert(function, bound.get());
            }
            if callable.shape.extension_receiver.is_some() && !companion_associated {
                self.ir.extension_receiver_fns.insert(function);
            }
            if callable.shape.context_parameter_count != 0 {
                self.ir
                    .fn_context_counts
                    .insert(function, callable.shape.context_parameter_count as usize);
            }
            if declaration_header
                .flags
                .has(crate::fir::DeclarationFlags::SUSPEND)
            {
                self.ir.suspend_funs.push(function);
            }
            if declaration_header
                .flags
                .has(crate::fir::DeclarationFlags::INLINE)
            {
                self.ir.inline_fns.insert(function);
            }
            if declaration_header
                .flags
                .has(crate::fir::DeclarationFlags::OPERATOR)
            {
                self.ir.operator_fns.insert(function);
            }
            if declaration_header
                .flags
                .has(crate::fir::DeclarationFlags::INFIX)
            {
                self.ir.infix_fns.insert(function);
            }
            if let Some(class) = class {
                self.ir.classes[class as usize].methods.push(function);
            }
            if let Some(header) = index.declaration_header(declaration) {
                if header.flags.has(crate::fir::DeclarationFlags::OPEN)
                    || header.flags.has(crate::fir::DeclarationFlags::ABSTRACT)
                {
                    self.ir.open_methods.insert(function);
                }
                if header.visibility.is_private() {
                    self.ir.private_methods.insert(function);
                }
                if header.visibility == crate::types::Visibility::Internal {
                    self.ir.internal_methods.insert(function);
                }
                if callable.is_inline() {
                    if class.is_none() {
                        self.ir.top_level_inline_functions.insert(function);
                    }
                    if header.visibility.is_public() {
                        self.ir.public_inline_functions.insert(function);
                    }
                }
            }
            self.ir
                .fn_params
                .insert(function, FnParamInfo::names(names));
            if let Some(plugin) = index.callable_behavior(callable.id).plugin_expression {
                self.ir
                    .plugin_declaration_functions
                    .entry(plugin)
                    .or_default()
                    .push(function);
            }
            if let Some(vararg) = (0..signature.parameters.len()).find(|ordinal| {
                index
                    .callable_parameter(callable.id, *ordinal as u32)
                    .is_some_and(|parameter| parameter.flags().is_vararg())
            }) {
                self.ir.fn_vararg_index.insert(function, vararg);
            }
            attach_callable_generic_facts(index, declaration, function, self.ir);
            assert!(
                self.ir
                    .checked_callable_functions
                    .insert(callable.id, function)
                    .is_none(),
                "a stable callable has one realization per common-IR file"
            );
        }
        Ok(())
    }

    fn accept_body(
        &mut self,
        index: &ResolvedModuleIndex,
        owner: BodyOwnerId,
        body: FirBody,
    ) -> Result<(), FirFileLoweringFailure> {
        let declaration = DeclarationId::from_raw(owner.raw());
        let anchor = index.declaration_anchor(declaration).ok_or(
            FirFileLoweringFailure::UnsupportedCallableOwner(declaration),
        )?;
        if anchor.kind == DeclarationKind::Constructor {
            return accept_constructor_body(
                declaration,
                body,
                index,
                self.ir,
                &mut self.local_callables,
            );
        }
        if anchor.kind == DeclarationKind::Property || anchor.kind == DeclarationKind::Accessor {
            return accept_property_body(
                declaration,
                body,
                index,
                self.ir,
                &mut self.local_callables,
            );
        }
        if matches!(
            anchor.kind,
            DeclarationKind::Initializer | DeclarationKind::EnumEntry | DeclarationKind::Script
        ) {
            return accept_non_callable_body(
                declaration,
                body,
                index,
                self.ir,
                &mut self.local_callables,
            );
        }
        let default_fragment = body.is_default_fragment();
        let callable = index
            .callable_for_declaration(declaration)
            .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
        let function = self
            .ir
            .checked_callable_functions
            .get(&callable.id)
            .copied()
            .ok_or(FirFileLoweringFailure::UnsupportedCallableOwner(
                declaration,
            ))?;
        let declaration_header = index
            .declaration_header(declaration)
            .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
        let companion_associated = declaration_header
            .flags
            .has(crate::fir::DeclarationFlags::COMPANION);
        if !default_fragment && self.ir.functions[function as usize].body.is_some() {
            return Err(FirFileLoweringFailure::DuplicateBody(callable.id));
        }
        let origin = body
            .roots()
            .first()
            .and_then(|root| body.statement(*root))
            .map_or(OriginId::from_raw(0), |statement| statement.origin);
        let expected_result = self.ir.functions[function as usize].ret;
        let lowered = lower_body_with_context(body, index, self.ir, &mut self.local_callables)
            .map_err(FirFileLoweringFailure::Body)?;
        let result = lowered
            .result_type
            .ok_or(FirFileLoweringFailure::MissingResultType(declaration))?;
        if result != expected_result {
            return Err(FirFileLoweringFailure::ResultTypeMismatch(declaration));
        }
        if default_fragment {
            if !lowered.roots.is_empty() || lowered.implicit_return || lowered.defaults.is_empty() {
                return Err(FirFileLoweringFailure::DuplicateBody(callable.id));
            }
            return self.attach_callable_defaults(
                callable,
                function,
                lowered.defaults.into_vec(),
                companion_associated,
            );
        }
        let roots = lowered.roots.into_vec();
        let body = if declaration_header
            .flags
            .has(crate::fir::DeclarationFlags::TAILREC)
            && self.ir.functions[function as usize].is_static
            && callable.shape.extension_receiver.is_none()
            && callable.shape.context_parameter_count == 0
        {
            finish_tailrec_body(
                self.ir,
                roots,
                function,
                self.ir.functions[function as usize].params.len(),
                origin,
            )
            .map_err(FirFileLoweringFailure::Body)?
        } else {
            finish_callable_body(
                self.ir,
                roots,
                result,
                lowered.implicit_return,
                false,
                origin,
            )
            .map_err(FirFileLoweringFailure::Body)?
        };
        self.ir.functions[function as usize].body = Some(body);

        self.attach_callable_defaults(
            callable,
            function,
            lowered.defaults.into_vec(),
            companion_associated,
        )?;
        Ok(())
    }

    fn attach_callable_defaults(
        &mut self,
        callable: crate::fir::ResolvedCallableHeader,
        function: u32,
        lowered: Vec<(u32, crate::ir::ExprId)>,
        companion_associated: bool,
    ) -> Result<(), FirFileLoweringFailure> {
        if lowered.is_empty() {
            return Ok(());
        }
        if self
            .ir
            .fn_params
            .get(&function)
            .is_some_and(|parameters| parameters.defaults.is_some())
        {
            return Err(FirFileLoweringFailure::DuplicateBody(callable.id));
        }
        let physical_count = self.ir.functions[function as usize].params.len();
        let mut defaults = vec![None; physical_count];
        for (parameter, value) in lowered {
            let mut position = parameter as usize;
            if callable.shape.extension_receiver.is_some()
                && !companion_associated
                && position >= callable.shape.context_parameter_count as usize
            {
                position += 1;
            }
            let Some(slot) = defaults.get_mut(position) else {
                return Err(FirFileLoweringFailure::MissingCallable(
                    callable.declaration,
                ));
            };
            *slot = Some(value);
        }
        let names = self
            .ir
            .fn_params
            .get(&function)
            .map(|info| info.names.clone())
            .unwrap_or_default();
        self.ir
            .fn_params
            .insert(function, FnParamInfo::defaults(names, defaults));
        Ok(())
    }
}

/// Short-lived view that supplies the current stable module index only while one checked body is
/// consumed. The persistent file sink therefore does not pin an immutable index borrow across
/// Pass-2 local-signature publication.
pub struct IndexedCommonIrBodySink<'sink, 'ir> {
    sink: &'sink mut CommonIrBodySink<'ir>,
    index: &'sink ResolvedModuleIndex,
}

impl CheckedBodySink for IndexedCommonIrBodySink<'_, '_> {
    fn accept_finalized(&mut self, owner: BodyOwnerId, body: FirBody) {
        if self.sink.failure.is_none() {
            self.sink.failure = self.sink.accept_body(self.index, owner, body).err();
            if let Some(failure) = &self.sink.failure {
                crate::trace_compiler!(
                    "lower",
                    "checked FIR body lowering failed owner={owner:?} failure={failure:?}",
                );
            }
        }
    }
}
