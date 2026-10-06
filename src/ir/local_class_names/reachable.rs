//! Classifier remapping for one reachable inline expansion.
//!
//! This owns detaching lambda implementations shared with an inline template and remapping the
//! copied expression/function/class facts. Whole-file and physical local-class naming stay in the
//! parent module.

use super::super::IrFile;
use super::*;

impl IrFile {
    /// Rename classifiers reachable from one inline expansion.
    ///
    /// [`Self::remap_classifier_identities`] rewrites the whole file, including the declaration a
    /// copy was taken from. This walk uses that same type, expression, and class-field contract on
    /// `roots`, on functions whose bodies those roots own, and on classes named by a value of
    /// `names`.
    ///
    /// Cloning an expression shares a lambda's `impl_fn` with the inline template. Remapping that
    /// function in place would retarget every call site. An implementation referenced from outside
    /// this walk is cloned once. `detached_impls` records those clones so a later walk of the same
    /// expansion updates them in place instead of cloning again.
    pub(crate) fn remap_reachable_classifier_identities(
        &mut self,
        names: &HashMap<TypeName, TypeName>,
        roots: impl IntoIterator<Item = ExprId>,
        detached_impls: &mut HashSet<FunId>,
    ) -> Vec<DetachedLambdaImplementation> {
        if names.is_empty() {
            return Vec::new();
        }
        let class_ids = self
            .classes
            .iter()
            .enumerate()
            .filter_map(|(index, class)| {
                let target = names.get(&class.fq_name).copied()?;
                let target = self.class_id_by_name(target)?;
                Some((index as ClassId, target))
            })
            .collect::<HashMap<_, _>>();
        let lambda_sites = self
            .exprs
            .iter()
            .enumerate()
            .filter_map(|(index, expr)| match expr {
                IrExpr::Lambda { impl_fn, .. } => Some((index as ExprId, *impl_fn)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut closure = HashSet::new();
        let mut pending = roots.into_iter().collect::<Vec<_>>();
        while let Some(expression) = pending.pop() {
            if !closure.insert(expression) {
                continue;
            }
            super::super::for_each_child(&self.exprs, expression, &mut |child| pending.push(child));
        }

        let mut seen = HashSet::new();
        let mut pending = closure.iter().copied().collect::<Vec<_>>();
        let mut local_clones = HashMap::new();
        let mut new_detached = Vec::new();
        let mut reachable_implementations = HashSet::new();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            if let IrExpr::Lambda {
                impl_fn: source,
                inline_body,
                ..
            } = self.exprs[expression as usize]
            {
                let impl_fn = if let Some(existing) = detached_impl(
                    source,
                    expression,
                    &lambda_sites,
                    &closure,
                    &local_clones,
                    detached_impls,
                ) {
                    existing
                } else {
                    let parent = self
                        .callable_reference_enclosures
                        .get(&expression)
                        .and_then(|enclosure| match enclosure {
                            super::super::IrEnclosure::Lambda(owner) => {
                                local_clones.get(owner).copied()
                            }
                            _ => None,
                        });
                    let cloned = detach_lambda_impl(self, source);
                    local_clones.insert(source, cloned);
                    detached_impls.insert(cloned);
                    new_detached.push(DetachedLambdaImplementation {
                        source,
                        target: cloned,
                        parent,
                        order: expression,
                    });
                    cloned
                };
                if let IrExpr::Lambda { impl_fn: slot, .. } = &mut self.exprs[expression as usize] {
                    *slot = impl_fn;
                }
                // An anonymous function cannot splice its body directly because its local return
                // must stay local. Its inline body is therefore a direct call to the same
                // implementation recorded on the Lambda node. When a shared implementation is
                // detached for this copy, keep those two stable identity edges together; leaving
                // the call on the source function gives it the declaration-class descriptor.
                if impl_fn != source {
                    if let Some(inline_body) = inline_body {
                        retarget_direct_inline_implementation(
                            &mut self.exprs[inline_body as usize],
                            source,
                            impl_fn,
                        );
                    }
                }
                reachable_implementations.insert(impl_fn);
                if let Some(body) = self.functions[impl_fn as usize].body {
                    pending.push(body);
                }
            }
            remap_expression(&mut self.exprs[expression as usize], names);
            remap_expression_class_ids(&mut self.exprs[expression as usize], &class_ids);
            let mut children = Vec::new();
            super::super::for_each_child(&self.exprs, expression, &mut |child| {
                children.push(child)
            });
            pending.extend(children);
        }

        let mut owned = self
            .functions
            .iter()
            .enumerate()
            .filter_map(|(index, function)| {
                function
                    .body
                    .is_some_and(|body| seen.contains(&body))
                    .then_some(index as FunId)
            })
            .collect::<Vec<_>>();
        // Common inline lowering can consume an implementation body before an enclosing
        // anonymous class is copied. Its signature still belongs to this reachable copy: a
        // bodyless static helper may take the anonymous object as a capture parameter, and leaving
        // that parameter on the declaration class makes the copied call site unverifiable.
        owned.extend(reachable_implementations);
        owned.sort_unstable();
        owned.dedup();
        for function in owned {
            remap_owned_function(self, function, names);
        }

        let placeholders = names.values().copied().collect::<HashSet<_>>();
        let copies = self
            .classes
            .iter()
            .enumerate()
            .filter_map(|(index, class)| {
                placeholders
                    .contains(&class.fq_name)
                    .then_some(index as ClassId)
            })
            .collect::<Vec<_>>();
        for class in copies {
            remap_class(&mut self.classes[class as usize], names);
            if let Some(provenance) = self.local_class_name_provenance.get_mut(&class) {
                local_class_name_provenance(provenance, names);
            }
            if let Some(specialized) = self.specialized_anonymous_classes.get_mut(&class) {
                specialized
                    .bindings
                    .values_mut()
                    .for_each(|value| *value = ty(*value, names));
                specialized
                    .reified_bindings
                    .values_mut()
                    .for_each(|value| *value = ty(*value, names));
            }
            remap_class_value(&mut self.shared_class_capture_fields, class, names);
            remap_class_value(&mut self.shared_super_capture_parameters, class, names);
            remap_secondary_capture(self, class, names);
        }
        for expression_id in seen {
            remap_expression_facts(self, expression_id, names);
        }
        new_detached
    }
}

fn retarget_direct_inline_implementation(expression: &mut IrExpr, source: FunId, target: FunId) {
    if let IrExpr::Call {
        callee: Callee::Local(function),
        ..
    } = expression
    {
        if *function == source {
            *function = target;
        }
    }
}

fn detached_impl(
    impl_fn: FunId,
    expression: ExprId,
    lambda_sites: &[(ExprId, FunId)],
    closure: &HashSet<ExprId>,
    local_clones: &HashMap<FunId, FunId>,
    detached_impls: &HashSet<FunId>,
) -> Option<FunId> {
    if detached_impls.contains(&impl_fn) {
        return Some(impl_fn);
    }
    if let Some(cloned) = local_clones.get(&impl_fn).copied() {
        return Some(cloned);
    }
    let shared = lambda_sites.iter().any(|(site, function)| {
        *function == impl_fn && *site != expression && !closure.contains(site)
    });
    if shared {
        None
    } else {
        Some(impl_fn)
    }
}

fn detach_lambda_impl(ir: &mut IrFile, source: FunId) -> FunId {
    let mut shape = ir.functions[source as usize].clone();
    shape.body = shape
        .body
        .map(|body| super::super::clone_expression_dag(ir, body).0);
    let target = super::super::clone::clone_class_method(ir, source, shape, &HashMap::new());
    // `inline_only` and `must_inline` describe how the source lambda's existing sites were
    // consumed. This detached implementation belongs to a newly copied site and must remain
    // available until that site's own representation pass either consumes or realizes it. Keeping
    // the source state can emit a continuation for the copy while dropping the method it re-enters.
    ir.inline_only_fns.remove(&target);
    ir.must_inline_lambdas.remove(&target);
    target
}

fn remap_owned_function(ir: &mut IrFile, function: FunId, names: &HashMap<TypeName, TypeName>) {
    {
        let shape = &mut ir.functions[function as usize];
        tys(&mut shape.params, names);
        shape.ret = ty(shape.ret, names);
        if let Some(owner) = &mut shape.dispatch_receiver {
            name(owner, names);
        }
    }
    if let Some(applied) = ir.function_annotations.get_mut(&function) {
        annotations(applied, names);
    }
    if let Some(parameters) = ir.fn_param_annotations.get_mut(&function) {
        for parameter in parameters {
            annotations(parameter, names);
        }
    }
    ir.remap_lambda_type_parameter_classifiers(function, |value| ty(value, names));
    if let Some(signature) = ir.signatures.get_mut(&function) {
        generic_signature(signature, names);
    }
    if let Some(spellings) = ir.fn_declared_spellings.get_mut(&function) {
        declared_spellings(spellings, names);
    }
    if let Some(parameters) = ir.callable_bound_type_parameters.get_mut(&function) {
        type_parameters(parameters, names);
    }
    if let Some((parameters, result)) = ir.member_semantic_sigs.get_mut(&function) {
        tys(parameters, names);
        *result = ty(*result, names);
    }
    if let Some((parameters, result)) = ir.suspend_declared_sigs.get_mut(&function) {
        tys(parameters, names);
        *result = ty(*result, names);
    }
    if let Some((_, parameters, result)) = ir.vc_declared_sigs.get_mut(&function) {
        tys(parameters, names);
        *result = ty(*result, names);
    }
    if let Some(parameters) = ir.default_stub_boxed_params.get_mut(&function) {
        for (_, parameter) in parameters {
            *parameter = ty(*parameter, names);
        }
    }
    if let Some(result) = ir.value_class_suspend_returns.get_mut(&function) {
        value_class_suspend_result(result, names);
    }
    if let Some((classifier, _)) = ir.jvm_suspend_impl_bodies.get_mut(&function) {
        name(classifier, names);
    }
    if let Some((parameters, result)) = ir.lambda_sam_signature.get_mut(&function) {
        tys(parameters, names);
        *result = ty(*result, names);
    }
    if let Some((parameters, result)) = ir.lambda_sam_jvm_signature.get_mut(&function) {
        tys(parameters, names);
        *result = ty(*result, names);
    }
    if let Some(origin) = ir.lambda_origins.get_mut(&function) {
        origin
            .lexical_owner
            .iter_mut()
            .for_each(|value| name(value, names));
        if let Some(provenance) = &mut origin.class_provenance {
            local_class_name_provenance(provenance, names);
        }
    }
    if let Some(classifier) = ir.lambda_class_names.get_mut(&function) {
        name(classifier, names);
    }
    let captures = ir
        .shared_capture_parameters
        .iter()
        .filter(|((owner, _), _)| *owner == function)
        .map(|((_, ordinal), value)| (*ordinal, *value))
        .collect::<Vec<_>>();
    for (ordinal, value) in captures {
        ir.shared_capture_parameters
            .insert((function, ordinal), ty(value, names));
    }
}

fn remap_class_value<K>(
    map: &mut HashMap<(ClassId, K), Ty>,
    class: ClassId,
    names: &HashMap<TypeName, TypeName>,
) where
    K: Eq + std::hash::Hash + Copy,
{
    let keys = map
        .keys()
        .copied()
        .filter(|(owner, _)| *owner == class)
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(value) = map.get_mut(&key) {
            *value = ty(*value, names);
        }
    }
}

fn remap_secondary_capture(ir: &mut IrFile, class: ClassId, names: &HashMap<TypeName, TypeName>) {
    let keys = ir
        .shared_secondary_super_capture_parameters
        .keys()
        .copied()
        .filter(|(owner, _, _)| *owner == class)
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(value) = ir.shared_secondary_super_capture_parameters.get_mut(&key) {
            *value = ty(*value, names);
        }
    }
}
