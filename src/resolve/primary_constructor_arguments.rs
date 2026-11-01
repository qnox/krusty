//! Primary constructor delegation arguments and their construction-site lexical scope.
//!
//! An anonymous object's base arguments execute before its dispatch instance exists. Named
//! classifiers instead retain the constructor-parameter rung supplied by class checking.

use super::*;

pub(super) struct PrimaryConstructorSite<'a> {
    pub(super) class: &'a ClassDecl,
    pub(super) declaration: DeclId,
    pub(super) owner: Option<TypeName>,
    pub(super) anonymous: bool,
}

impl Checker<'_> {
    pub(super) fn check_primary_constructor_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        lexical_scope: &CheckerScope<'_>,
        site: PrimaryConstructorSite<'_>,
    ) {
        let PrimaryConstructorSite {
            class: cl,
            declaration: d,
            owner: current_owner,
            anonymous: is_anonymous_object,
        } = site;
        // An anonymous constructor header sees lexical values without its instance-property
        // overlay. Retain its authentic dispatch rung solely as the unavailable constructor
        // prefix: removing that rung would renumber enclosing receivers in the capture ledger.
        let anonymous_scope = is_anonymous_object.then(|| {
            lexical_scope.dispatch_receiver_child(
                scope
                    .this_ty()
                    .expect("a checked anonymous classifier has its dispatch receiver"),
            )
        });
        let scope = anonymous_scope.as_ref().unwrap_or(scope);
        // Base class constructor args are evaluated before the body and may reference ctor params.
        // A LAMBDA or callable reference arg is typed against the selected super ctor's parameter
        // type (a receiver lambda's `this` scope and a reference's function shape come from that
        // expected type), so it is postponed here and checked below once the ctor is resolved.
        // An empty `arrayOf()` is postponed for the same reason: only the selected parameter
        // supplies its element type. Anything else is typed up front.
        let file = self.file;
        let is_postponed = |checker: &Self, scope: &CheckerScope<'_>, arg: ExprId| {
            matches!(
                file.expr(arg),
                Expr::Lambda { .. } | Expr::CallableRef { .. }
            ) || checker.postponed_empty_reference_array_factory(scope, arg)
        };
        let static_this = current_owner
            .and_then(|owner| self.applied_declared_supertype(owner))
            .and_then(Ty::kotlin_class_internal)
            .and_then(|owner| {
                crate::symbol_resolver::classifier_companion_instance(
                    &self.fed_source(),
                    Ty::obj_name(owner),
                )
            });
        let singleton_this = current_owner.and_then(|owner| self.enclosing_object_singleton(owner));
        crate::trace_compiler!(
            "resolve",
            "constructor header class={} owner={current_owner:?} base={:?} companion={static_this:?}",
            cl.name,
            current_owner
                .and_then(|owner| self.applied_declared_supertype(owner))
                .and_then(Ty::kotlin_class_internal),
        );
        {
            let header_scope = scope.singleton_receiver_child(
                singleton_this
                    .as_ref()
                    .map(|instance| Ty::obj_name(instance.classifier))
                    .or_else(|| {
                        static_this
                            .as_ref()
                            .map(|instance| Ty::obj_name(instance.companion))
                    }),
            );
            let header_scope = &header_scope;
            if is_anonymous_object {
                self.constructor_receiver_frames.push(
                    super::constructor_receiver_frames::ConstructorReceiverFrame::new(
                        d,
                        header_scope,
                        lexical_scope,
                    ),
                );
            }
            let previous_static_this =
                std::mem::replace(&mut self.static_companion_this, static_this);
            let previous_singleton_this =
                std::mem::replace(&mut self.static_singleton_this, singleton_this);
            // The dispatch instance is uninitialized in `super(…)` arguments. A selected
            // enclosing object or superclass companion is the only constructor-header `this`;
            // a receiver function argument may introduce a still-nearer receiver of its own.
            self.with_this_unavailable(|c| {
                // Property reads in an anonymous super-constructor argument run at the
                // construction site. Counting them here would capture an enclosing
                // instance the anonymous constructor does not use.
                let previous_capture_accounting = c.suppress_receiver_capture_accounting;
                c.suppress_receiver_capture_accounting = is_anonymous_object;
                for &arg in &cl.base_args {
                    if !is_postponed(c, header_scope, arg) {
                        c.expr(header_scope, arg);
                    }
                }
                c.suppress_receiver_capture_accounting = previous_capture_accounting;
                // Resolve the primary constructor's `Base(args)` clause through the SAME
                // delegation candidates and argument-slot mapper used by secondary
                // `super(args)` calls. Besides keeping same-file, sibling-module, and
                // classpath bases on one path, the resulting source-order `argument_types`
                // are essential for named/vararg arguments: zipping the source arguments
                // with parameter order would contextually type the wrong lambda.
                if cl.base_class.is_some() {
                    let internal = type_name(&class_internal(c.file, &cl.name));
                    let names = cl
                        .base_args
                        .first()
                        .and_then(|argument| c.file.base_arg_names.get(&argument.0))
                        .cloned()
                        .unwrap_or_else(|| vec![None; cl.base_args.len()]);
                    let arguments = CtorDelegationCall {
                        args: cl.base_args.clone(),
                        names,
                        trailing_lambda: false,
                    };
                    let candidates = c.super_ctor_delegation_candidates(current_owner);
                    crate::trace_compiler!(
                        "fir",
                        "primary constructor delegation class={} arguments={:?} candidates={:?}",
                        cl.name,
                        cl.base_args
                            .iter()
                            .map(|argument| c.expr_types[argument.0 as usize])
                            .collect::<Vec<_>>(),
                        candidates
                            .iter()
                            .map(|candidate| candidate.target.params())
                            .collect::<Vec<_>>(),
                    );
                    if let SourceConstructorSelection::Selected {
                        resolved: selected, ..
                    } = c.select_source_constructor(header_scope, &arguments, &candidates)
                    {
                        let selected = *selected;
                        c.record_whole_array_delegation_arguments(&arguments, &selected);
                        if let Some(receiver) = &selected.outer_receiver {
                            c.mark_selected_implicit_receiver_used(header_scope, receiver, d);
                        }
                        crate::trace_compiler!(
                            "fir",
                            "selected primary constructor delegation class={} target={:?}",
                            cl.name,
                            selected.target,
                        );
                        // A receiver lambda's implicit label is the source-level base class's SIMPLE
                        // name. Split every internal/nested separator so `Outer.Base` and `Outer$Base`
                        // both expose `Base`, rather than exposing a JVM-internal spelling as a label.
                        let label = cl.base_class.as_deref().map(|base| {
                            base.rsplit(['/', '.', '$'])
                                .next()
                                .unwrap_or(base)
                                .to_string()
                        });
                        c.suppress_receiver_capture_accounting = is_anonymous_object;
                        for (((&arg, &expected), &declared), &has_receiver) in cl
                            .base_args
                            .iter()
                            .zip(&selected.argument_types)
                            .zip(&selected.argument_declared_types)
                            .zip(&selected.argument_lambda_receivers)
                        {
                            // Selection fixes the base constructor's semantic parameter
                            // just as an ordinary call does. Commit that expectation for
                            // every argument form: a nested generic constructor may have no
                            // argument-side evidence of its own (`Base<String>(Value("x"))`)
                            // and must publish `Value<String>` before checked FIR validates
                            // its substitutions.
                            let actual = if matches!(file.expr(arg), Expr::CallableRef { .. })
                                && !expected.non_null().is_fun()
                            {
                                c.expr(header_scope, arg)
                            } else {
                                c.expr_types[arg.0 as usize]
                            };
                            c.expect_call_arg_labeled(
                                header_scope,
                                expected,
                                declared,
                                arg,
                                actual,
                                has_receiver,
                                label.as_deref(),
                            );
                        }
                        c.suppress_receiver_capture_accounting = previous_capture_accounting;
                        c.super_ctor_params
                            .insert(internal, selected.target.params().to_vec());
                        c.resolved_primary_ctor_delegations.insert(d, selected);
                    } else {
                        crate::trace_compiler!(
                            "fir",
                            "failed primary constructor delegation class={}",
                            cl.name,
                        );
                        // No unique ctor match: type the deferred lambda args without an expected type.
                        c.suppress_receiver_capture_accounting = is_anonymous_object;
                        for &arg in &cl.base_args {
                            if is_postponed(c, header_scope, arg) {
                                c.expr(header_scope, arg);
                            }
                        }
                        c.suppress_receiver_capture_accounting = previous_capture_accounting;
                    }
                }
                if is_anonymous_object {
                    c.publish_anonymous_super_forwards(d, &cl.base_args);
                }
                if !is_anonymous_object {
                    for &argument in &cl.base_args {
                        c.finish_executable_root(argument);
                    }
                }
            });
            self.static_companion_this = previous_static_this;
            self.static_singleton_this = previous_singleton_this;
        }
        if is_anonymous_object {
            self.constructor_receiver_frames
                .pop()
                .expect("anonymous constructor receiver frame");
        }
    }
}
