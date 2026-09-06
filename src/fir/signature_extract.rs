//! AST-to-constraint extraction for non-local inferred signatures.
//!
//! This is a structural pass only. It records compact operations and deferred lookups; ordinary
//! resolver/checker semantics remain behind `SignatureSemantics` during graph evaluation.

use crate::ast::{BinOp, Expr, ExprId, File, RangeKind, Stmt, TrFlags, TypeRef, UnOp};
use crate::types::Ty;
use std::collections::{HashMap, HashSet};

use super::coverage::ExpressionForm;
use super::{
    DeclarationId, DeclarationStub, DeferredCallableSelection, DeferredMemberSelection,
    DeferredValueSelection, OriginId, ResolvedTy, SigCallArgument, SigExpr, SigExprId,
    SignatureGraph, SignatureScope, SignatureScopeId, SourceFileId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureExtractionFailure {
    pub declaration: DeclarationId,
    pub form: ExpressionForm,
}

/// Builds only temporary signature constraints. It owns no source, AST ids, or body syntax after
/// `extract_file` returns.
#[derive(Default)]
pub struct SignatureConstraintExtractor {
    graph: SignatureGraph,
    failures: Vec<SignatureExtractionFailure>,
    source_classifiers: HashMap<crate::diag::Span, DeclarationId>,
    source_primary_constructors: HashMap<crate::diag::Span, DeclarationId>,
    source_functions: HashMap<crate::diag::Span, DeclarationId>,
    source_stubs: HashMap<crate::diag::Span, DeclarationStub>,
    direct_classifier_parents: HashMap<crate::ast::DeclId, crate::ast::DeclId>,
    lexical_values: Vec<HashMap<Box<str>, SigExprId>>,
    lexical_callables: Vec<HashMap<Box<str>, CompactLexicalCallable>>,
    lexical_types: Vec<HashMap<Box<str>, CompactLexicalType>>,
    local_classifier_stack: Vec<crate::ast::DeclId>,
    captured_local_classifier_headers: HashSet<DeclarationId>,
    extracting_local_effects: HashSet<DeclarationId>,
    lambda_returns: Vec<CompactLambdaReturnScope>,
    approximate_anonymous_result: bool,
}

#[derive(Clone)]
struct CompactLocalTypeAlias {
    formals: Vec<String>,
    target: TypeRef,
}

#[derive(Clone, Copy)]
struct CompactLexicalCallable {
    value: SigExprId,
    has_receiver: bool,
}

#[derive(Clone)]
enum CompactLexicalType {
    Alias(CompactLocalTypeAlias),
    Classifier(DeclarationId),
}

#[derive(Default)]
struct CompactLambdaReturnScope {
    label: Option<Box<str>>,
    values: Vec<SigExprId>,
}

impl SignatureConstraintExtractor {
    fn lambda_return_matches(&self, label: Option<&str>) -> bool {
        label.is_some()
            && self
                .lambda_returns
                .last()
                .is_some_and(|scope| scope.label.as_deref() == label)
    }

    fn finish_lambda_result(
        &mut self,
        result: SigExprId,
        returns: CompactLambdaReturnScope,
        scope: SignatureScopeId,
        origin: OriginId,
    ) -> SigExprId {
        if returns.values.is_empty() {
            return result;
        }
        let mut values = returns.values;
        values.push(result);
        let operands = self.graph.add_operands(values);
        self.graph.add_expr(SigExpr::Join {
            operands,
            scope,
            origin,
        })
    }

    fn qualified_callable_spelling(&self, file: &File, expression: ExprId) -> Option<String> {
        fn collect(file: &File, expression: ExprId, names: &mut Vec<String>) -> bool {
            match file.expr(expression) {
                Expr::Name(name) => {
                    names.push(name.clone());
                    true
                }
                Expr::Member { receiver, name } => {
                    if !collect(file, *receiver, names) {
                        return false;
                    }
                    names.push(name.clone());
                    true
                }
                _ => false,
            }
        }

        let mut names = Vec::new();
        if !collect(file, expression, &mut names) || names.len() < 2 {
            return None;
        }
        if self
            .lexical_values
            .iter()
            .rev()
            .any(|values| values.contains_key(names[0].as_str()))
        {
            return None;
        }
        Some(names.join("."))
    }

    fn qualified_value(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: OriginId,
    ) -> Option<SigExprId> {
        let spelling = self.qualified_callable_spelling(file, expression)?;
        let spelling = self.graph.intern_name(&spelling);
        let selection = self
            .graph
            .add_value_selection(super::DeferredValueSelection {
                scope,
                spelling,
                origin,
                expected: None,
            });
        Some(self.graph.add_expr(SigExpr::Value(selection)))
    }

    pub fn extract_file(
        &mut self,
        file: &File,
        source: SourceFileId,
        stubs: &[DeclarationStub],
        mut origin: impl FnMut(crate::diag::Span) -> OriginId,
    ) {
        self.source_classifiers.clear();
        self.source_classifiers.extend(
            stubs
                .iter()
                .filter(|stub| stub.kind == super::DeclarationKind::Classifier)
                .map(|stub| (stub.range, stub.id)),
        );
        self.source_functions.clear();
        self.source_functions.extend(
            stubs
                .iter()
                .filter(|stub| stub.kind == super::DeclarationKind::Function)
                .map(|stub| (stub.range, stub.id)),
        );
        self.source_primary_constructors.clear();
        self.source_primary_constructors.extend(
            stubs
                .iter()
                .filter(|stub| stub.kind == super::DeclarationKind::Constructor)
                .filter(|stub| self.source_classifiers.contains_key(&stub.range))
                .map(|stub| (stub.range, stub.id)),
        );
        self.source_stubs.clear();
        self.source_stubs
            .extend(stubs.iter().map(|stub| (stub.range, *stub)));
        self.direct_classifier_parents.clear();
        let classifiers = file
            .decls
            .iter()
            .copied()
            .filter_map(|declaration| match file.decl(declaration) {
                crate::ast::Decl::Class(classifier) => Some((declaration, classifier.span)),
                crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_) => None,
            })
            .collect::<Vec<_>>();
        for &(child, child_span) in &classifiers {
            let parent = classifiers
                .iter()
                .copied()
                .filter(|(candidate, candidate_span)| {
                    *candidate != child
                        && candidate_span.lo <= child_span.lo
                        && child_span.hi <= candidate_span.hi
                })
                .min_by_key(|(_, span)| span.hi - span.lo)
                .map(|(parent, _)| parent);
            if let Some(parent) = parent {
                self.direct_classifier_parents.insert(child, parent);
            }
        }
        // Local-class members are not published in the module index, but their inferred result may
        // be demanded by an enclosing non-local signature (for example, a function returning the
        // result of a method on an anonymous-object property). Such a constraint is registered
        // while extracting that inferred non-local root, under its real lexical context. A local
        // member reached only from an explicit ordinary body belongs entirely to Pass 2 and must
        // not make that body enter the temporary signature graph.
        for stub in stubs
            .iter()
            .filter(|stub| stub.signature_inference.is_some())
        {
            // Extracting an enclosing signature may discover a local classifier and eagerly add
            // constraints for its inferred members. That same member still appears in the flat
            // stub stream later; its declaration identity makes the already-extracted constraint
            // authoritative, so do not add a duplicate graph root.
            if self.graph.constraint(stub.id).is_some() {
                continue;
            }
            if stub.flags.has(super::DeclarationFlags::LOCAL_CLASS) {
                continue;
            }
            let Some(expression) = source_signature_expression(file, stub) else {
                self.failures.push(SignatureExtractionFailure {
                    declaration: stub.id,
                    form: ExpressionForm::UnsupportedAnnotationArgument,
                });
                continue;
            };
            let expression_span = file
                .expr_span(expression)
                .expect("an inferred signature expression has a source span");
            let constraint_origin = origin(expression_span);
            let scope = self.graph.add_scope(SignatureScope {
                owner: stub.id,
                source,
            });
            self.lexical_values.clear();
            self.lexical_callables.clear();
            self.lexical_types.clear();
            self.local_classifier_stack.clear();
            self.extracting_local_effects.clear();
            let mut parameters = HashMap::new();
            let mut callables = HashMap::new();
            let function = source_function(file, stub.range);
            if let Some(function) = function {
                for (index, parameter) in function.params.iter().enumerate() {
                    let value = self.graph.add_expr(SigExpr::Parameter {
                        declaration: stub.id,
                        index: u32::try_from(index).expect("too many signature parameters"),
                    });
                    if parameter.ty.fun_has_receiver() {
                        callables.insert(
                            parameter.name.clone().into_boxed_str(),
                            CompactLexicalCallable {
                                value,
                                has_receiver: true,
                            },
                        );
                    }
                    parameters.insert(parameter.name.clone().into_boxed_str(), value);
                }
            }
            let enclosing_classifier = file
                .decls
                .iter()
                .copied()
                .filter_map(|declaration| match file.decl(declaration) {
                    crate::ast::Decl::Class(classifier)
                        if classifier.span.lo <= stub.range.lo
                            && stub.range.hi <= classifier.span.hi =>
                    {
                        Some((
                            classifier.span.hi - classifier.span.lo,
                            declaration,
                            classifier,
                        ))
                    }
                    crate::ast::Decl::Class(_)
                    | crate::ast::Decl::Fun(_)
                    | crate::ast::Decl::Property(_) => None,
                })
                .min_by_key(|(length, _, _)| *length);
            if let Some(function) = function {
                if let Some((_, _, classifier)) = enclosing_classifier {
                    if let Some(declaration) =
                        self.source_classifiers.get(&classifier.span).copied()
                    {
                        let receiver = self
                            .graph
                            .add_expr(SigExpr::ClassifierType { declaration, scope });
                        parameters.insert("this".into(), receiver);
                        if let Some(label) = classifier.name.rsplit('.').next() {
                            parameters.insert(format!("this@{label}").into_boxed_str(), receiver);
                        }
                    }
                }
                if let Some(receiver) = function
                    .receiver
                    .as_ref()
                    .map(|receiver| self.compact_type(receiver, scope, &mut origin))
                {
                    parameters.insert("this".into(), receiver);
                    parameters.insert(format!("this@{}", function.name).into_boxed_str(), receiver);
                }
            }
            if let Some((_, declaration, _)) = enclosing_classifier {
                self.local_classifier_stack.push(declaration);
            }
            self.lexical_values.push(parameters);
            self.lexical_callables.push(callables);
            self.lexical_types.push(HashMap::new());
            self.approximate_anonymous_result = stub.visibility
                != crate::types::Visibility::Private
                || function.is_some_and(|function| {
                    function.is_override()
                        && function.visibility != crate::types::Visibility::Private
                });
            match self.expression(file, expression, scope, &mut origin) {
                Ok(mut result) => {
                    if stub.signature_inference
                        == Some(super::InferredSignatureKind::BackingFieldInitializer)
                    {
                        let declared = source_property(file, stub.range)
                            .and_then(crate::ast::PropDecl::declared_ty)
                            .cloned();
                        if let Some(declared) = declared {
                            let expected = self.compact_type(&declared, scope, &mut origin);
                            self.graph.apply_result_expectation(result, expected);
                        }
                    }
                    result = if stub.signature_inference
                        == Some(super::InferredSignatureKind::DelegatedProperty)
                    {
                        let origin = constraint_origin;
                        self.graph.add_expr(SigExpr::Delegate {
                            declaration: stub.id,
                            delegate: result,
                            scope,
                            origin,
                            local: false,
                        })
                    } else {
                        result
                    };
                    self.graph
                        .add_inferred_constraint(stub, result, constraint_origin)
                }
                Err(form) => self.failures.push(SignatureExtractionFailure {
                    declaration: stub.id,
                    form,
                }),
            }
            self.lexical_values.clear();
            self.lexical_callables.clear();
            self.lexical_types.clear();
            self.local_classifier_stack.clear();
            self.extracting_local_effects.clear();
        }
    }

    pub fn graph(&self) -> &SignatureGraph {
        &self.graph
    }

    pub fn failures(&self) -> &[SignatureExtractionFailure] {
        &self.failures
    }

    pub fn finish(self) -> Result<SignatureGraph, Vec<SignatureExtractionFailure>> {
        if self.failures.is_empty() {
            Ok(self.graph)
        } else {
            Err(self.failures)
        }
    }

    pub fn into_parts(self) -> (SignatureGraph, Vec<SignatureExtractionFailure>) {
        assert!(
            self.lexical_values.is_empty()
                && self.lexical_callables.is_empty()
                && self.lexical_types.is_empty(),
            "signature extraction must not retain source-local spellings"
        );
        (self.graph, self.failures)
    }

    fn known(&mut self, ty: Ty) -> SigExprId {
        let ty = ResolvedTy::new(ty).expect("a built-in signature leaf must be publishable");
        self.graph.add_expr(SigExpr::Known(ty))
    }

    fn integer_literal(&mut self, value: i64) -> SigExprId {
        match i32::try_from(value) {
            Ok(value) => self.graph.add_expr(SigExpr::IntegerLiteral(value)),
            Err(_) => self.known(Ty::Int),
        }
    }

    fn lexical_receivers(&self) -> Vec<SigExprId> {
        self.lexical_values
            .iter()
            .rev()
            .filter_map(|values| {
                let receiver = values.get("this").copied()?;
                // A contextual lambda binds `this` to a deferred lookup because whether the
                // expected function type contributes an extension receiver is not known until
                // evaluation. That lookup is already evaluated while the contextual function's
                // receiver rung is active. Capturing it again as an unconditional scoped receiver
                // on every nested local declaration would make an ordinary `() -> T` lambda fail
                // merely because no extension receiver exists, even when the nested body never
                // uses `this`.
                (!matches!(self.graph.expr(receiver), Some(SigExpr::Value(_)))).then_some(receiver)
            })
            .collect()
    }

    fn wrap_lexical_receivers(
        &mut self,
        mut result: SigExprId,
        receivers: impl IntoIterator<Item = SigExprId>,
        scope: SignatureScopeId,
    ) -> SigExprId {
        for receiver in receivers {
            result = self.graph.add_expr(SigExpr::ScopedReceiver {
                receiver,
                result,
                scope,
            });
        }
        result
    }

    fn declaration_demands_since(&mut self, constraint_count: usize) -> Vec<SigExprId> {
        let declarations = self.graph.constraints()[constraint_count..]
            .iter()
            .map(|constraint| constraint.declaration)
            .collect::<Vec<_>>();
        declarations
            .into_iter()
            .map(|declaration| self.graph.add_expr(SigExpr::DeclarationType(declaration)))
            .collect()
    }

    fn register_local_member_effects(
        &mut self,
        file: &File,
        classifier: crate::ast::DeclId,
        spelling: &str,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<(), ExpressionForm> {
        if !file.is_local_declaration(classifier) && !file.is_anonymous_object_class(classifier) {
            return Ok(());
        }
        let crate::ast::Decl::Class(classifier_decl) = file.decl(classifier) else {
            return Ok(());
        };
        let classifier_declaration = self
            .source_classifiers
            .get(&classifier_decl.span)
            .copied()
            .ok_or(ExpressionForm::Call)?;
        let enclosing_receivers = self.lexical_receivers();
        let methods = classifier_decl
            .methods
            .iter()
            .filter(|method| method.name == spelling)
            .cloned()
            .collect::<Vec<_>>();
        for method in methods {
            let Some(declaration) = self.source_functions.get(&method.span).copied() else {
                continue;
            };
            if self.graph.local_effect(declaration).is_some()
                || !self.extracting_local_effects.insert(declaration)
            {
                continue;
            }
            if !method.type_params.is_empty() {
                // A generic member is selected through its stable ClassSig declaration, not as a
                // monomorphic compact function value. Its ordinary body can still contain nested
                // local/anonymous classifiers whose signatures belong to Pass 1, so walk it under
                // the member's own semantic scope and retain only those dependency constraints.
                // Rejecting the whole enclosing anonymous-object expression here made an explicit
                // generic override poison an otherwise fully known property signature.
                let method_scope = self.graph.add_scope(SignatureScope {
                    owner: declaration,
                    source: self
                        .graph
                        .scope(scope)
                        .expect("a local member effect must retain its source scope")
                        .source,
                });
                self.register_generic_local_function_dependencies(
                    file,
                    &method,
                    method_scope,
                    origin,
                );
                self.extracting_local_effects.remove(&declaration);
                continue;
            }
            let mut bindings = HashMap::new();
            for parameter in &method.params {
                let ty = self.compact_type(&parameter.ty, scope, origin);
                bindings.insert(parameter.name.clone().into_boxed_str(), ty);
            }
            let extension_receiver = if let Some(receiver) = &method.receiver {
                let receiver = self.compact_type(receiver, scope, origin);
                bindings.insert("this".into(), receiver);
                Some(receiver)
            } else {
                None
            };
            let dispatch_receiver = if self.local_classifier_stack.last() == Some(&classifier) {
                self.lexical_values
                    .iter()
                    .rev()
                    .find_map(|values| values.get("this").copied())
                    .expect("an active local classifier must bind its dispatch receiver")
            } else {
                self.graph.add_expr(SigExpr::ClassifierType {
                    declaration: classifier_declaration,
                    scope,
                })
            };
            bindings.entry("this".into()).or_insert(dispatch_receiver);
            self.lexical_values.push(bindings);
            self.lexical_types.push(HashMap::new());
            self.local_classifier_stack.push(classifier);
            let result = match &method.body {
                crate::ast::FunBody::Expr(expression) | crate::ast::FunBody::Block(expression) => {
                    self.expression(file, *expression, scope, origin)
                }
                crate::ast::FunBody::None => Ok(self.known(Ty::Unit)),
            };
            self.local_classifier_stack.pop();
            self.lexical_types.pop();
            self.lexical_values.pop();
            self.extracting_local_effects.remove(&declaration);
            let mut result = result?;
            if let Some(receiver) = extension_receiver {
                result = self.graph.add_expr(SigExpr::ScopedReceiver {
                    receiver,
                    result,
                    scope,
                });
            }
            result = self.graph.add_expr(SigExpr::ScopedReceiver {
                receiver: dispatch_receiver,
                result,
                scope,
            });
            result = self.wrap_lexical_receivers(
                result,
                enclosing_receivers
                    .iter()
                    .copied()
                    .filter(|receiver| *receiver != dispatch_receiver),
                scope,
            );
            let effect = super::LocalSignatureEffect {
                result,
                determines_result: method.ret.is_none()
                    && matches!(method.body, crate::ast::FunBody::Expr(_)),
            };
            self.graph.add_local_effect(declaration, effect);
            if effect.determines_result && self.graph.constraint(declaration).is_none() {
                let stub = self
                    .source_stubs
                    .get(&method.span)
                    .copied()
                    .expect("a local inferred method must retain its compact stub");
                let constraint_origin = origin(
                    file.expr_span(match method.body {
                        crate::ast::FunBody::Expr(expression) => expression,
                        crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => {
                            unreachable!("a determining local result must use an expression body")
                        }
                    })
                    .expect("a local inferred method must retain its source span"),
                );
                self.graph
                    .add_inferred_constraint(&stub, result, constraint_origin);
            }
        }
        Ok(())
    }

    fn register_local_class_property_constraints(
        &mut self,
        file: &File,
        statement: crate::ast::StmtId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<(), ExpressionForm> {
        let Some(&classifier) = file.local_class_decls.get(&statement) else {
            return Err(ExpressionForm::Block);
        };
        self.register_local_classifier_constraints(file, classifier, scope, origin)
    }

    fn register_local_classifier_constraints(
        &mut self,
        file: &File,
        classifier: crate::ast::DeclId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<(), ExpressionForm> {
        let crate::ast::Decl::Class(class) = file.decl(classifier) else {
            return Err(ExpressionForm::Block);
        };
        let Some(classifier_declaration) = self.source_classifiers.get(&class.span).copied() else {
            return Err(ExpressionForm::Block);
        };
        let source = self
            .graph
            .scope(scope)
            .expect("a local classifier dependency must retain its enclosing scope")
            .source;
        let member_scope = self.graph.add_scope(SignatureScope {
            owner: classifier_declaration,
            source,
        });
        let receiver = self.graph.add_expr(SigExpr::ClassifierType {
            declaration: classifier_declaration,
            scope: member_scope,
        });
        let enclosing_receivers = self.lexical_receivers();
        let mut bindings = HashMap::new();
        if let Some(&constructor) = self.source_primary_constructors.get(&class.span) {
            for (index, parameter) in class.props.iter().enumerate() {
                bindings.insert(
                    parameter.name.clone().into_boxed_str(),
                    self.graph.add_expr(SigExpr::Parameter {
                        declaration: constructor,
                        index: u32::try_from(index)
                            .expect("too many local classifier constructor parameters"),
                    }),
                );
            }
        }
        self.lexical_values.push(bindings);
        self.lexical_types
            .push(if file.is_anonymous_object_class(classifier) {
                HashMap::new()
            } else {
                HashMap::from([(
                    class
                        .name
                        .rsplit('.')
                        .next()
                        .unwrap_or(&class.name)
                        .to_owned()
                        .into_boxed_str(),
                    CompactLexicalType::Classifier(classifier_declaration),
                )])
            });
        self.local_classifier_stack.push(classifier);
        let extracted = (|| {
            self.register_local_classifier_explicit_types(
                file,
                class,
                classifier_declaration,
                member_scope,
                origin,
            );

            // Super-constructor and interface-delegate expressions execute before the new
            // classifier's dispatch receiver exists. They can nevertheless declare anonymous
            // classifiers whose inferred members capture constructor parameters or enclosing
            // lexical values, so discover those compact dependencies in this pre-`this` rung.
            for expression in class.base_args.iter().copied().chain(
                class
                    .interface_delegations
                    .iter()
                    .map(|delegation| delegation.value),
            ) {
                let _ = self.expression(file, expression, member_scope, origin);
            }

            let member_bindings = self
                .lexical_values
                .last_mut()
                .expect("a local classifier must retain its constructor scope");
            member_bindings.insert("this".into(), receiver);
            if let Some(label) = class.name.rsplit('.').next() {
                member_bindings.insert(format!("this@{label}").into_boxed_str(), receiver);
            }

            // Parser-hoisted classifiers inside a local/anonymous class include both statement
            // locals and ordinary nested/inner member classes. Walk only DIRECT children. Scanning
            // every descendant at every level revisits a depth-N anonymous chain through every
            // ancestor subset (exponential work); direct containment was derived once per file.
            let mut nested = file
                .decls
                .iter()
                .copied()
                .filter(|declaration| {
                    self.direct_classifier_parents.get(declaration) == Some(&classifier)
                })
                .collect::<Vec<_>>();
            nested.sort_unstable_by_key(|declaration| match file.decl(*declaration) {
                crate::ast::Decl::Class(candidate) => candidate.span.lo,
                crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_) => u32::MAX,
            });
            nested.dedup();
            for declaration in nested {
                self.register_local_classifier_constraints(
                    file,
                    declaration,
                    member_scope,
                    origin,
                )?;
            }

            let method_names = class
                .methods
                .iter()
                .map(|method| method.name.as_str())
                .collect::<std::collections::HashSet<_>>();
            for method_name in method_names {
                self.register_local_member_effects(
                    file,
                    classifier,
                    method_name,
                    member_scope,
                    origin,
                )?;
            }
            for property in &class.body_props {
                let Some(stub) = self.source_stubs.get(&property.span).copied() else {
                    continue;
                };
                if stub.signature_inference.is_none() || self.graph.constraint(stub.id).is_some() {
                    continue;
                }
                let Some(expression) = source_signature_expression(file, &stub) else {
                    continue;
                };
                let constraint_origin = origin(
                    file.expr_span(expression)
                        .expect("a local inferred property expression has a source span"),
                );
                let mut result = self.expression(file, expression, member_scope, origin)?;
                result = self.graph.add_expr(SigExpr::ScopedReceiver {
                    receiver,
                    result,
                    scope: member_scope,
                });
                result = self.wrap_lexical_receivers(
                    result,
                    enclosing_receivers.iter().copied(),
                    member_scope,
                );
                if stub.signature_inference == Some(super::InferredSignatureKind::DelegatedProperty)
                {
                    result = self.graph.add_expr(SigExpr::Delegate {
                        declaration: stub.id,
                        delegate: result,
                        scope: member_scope,
                        origin: constraint_origin,
                        local: true,
                    });
                }
                self.graph
                    .add_inferred_constraint(&stub, result, constraint_origin);
            }
            Ok(())
        })();
        self.local_classifier_stack.pop();
        self.lexical_types.pop();
        self.lexical_values.pop();
        extracted
    }

    /// Capture explicit member/header types while statement-local type aliases are still in their
    /// lexical scope. The transitional symbol table cannot represent that body-local alias rung and
    /// may contain `Ty::Error`; these compact type expressions replace it before publication.
    fn register_local_classifier_explicit_types(
        &mut self,
        _file: &File,
        class: &crate::ast::ClassDecl,
        classifier: DeclarationId,
        enclosing_scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) {
        // One local classifier can be reached by several demanded signature roots (for example,
        // an anonymous object returned through an inline lambda and its inferred member result).
        // Its declaration header is a single Pass-1 fact and must be captured only on the first
        // lexical visit; later visits still extract any independently demanded member effects.
        if !self.captured_local_classifier_headers.insert(classifier) {
            return;
        }
        let source = self
            .graph
            .scope(enclosing_scope)
            .expect("a local classifier must retain its source scope")
            .source;
        let declaration_scope = |this: &mut Self, declaration| {
            this.graph.add_scope(SignatureScope {
                owner: declaration,
                source,
            })
        };

        // Parent types belong to the classifier header, but a body-local classifier can spell
        // them through a statement-local typealias. Capture their expanded compact expressions
        // while that lexical type rung still exists; only the resolved semantic edges survive
        // Pass 1.
        let superclass = class.base_class.as_ref().map(|base| TypeRef {
            name: base.clone(),
            flags: TrFlags::default(),
            arg: None,
            targs: class.base_type_args.clone(),
            span: class.base_class_span.unwrap_or(class.span),
            fun_params: Vec::new(),
            fun_context_count: 0,
        });
        let superclass = superclass
            .as_ref()
            .map(|parent| self.compact_type(parent, enclosing_scope, origin));
        let supertypes = class
            .supertypes
            .iter()
            .map(|parent| self.compact_type(parent, enclosing_scope, origin))
            .collect::<Vec<_>>();
        self.graph
            .add_explicit_classifier_parents(classifier, superclass, supertypes);

        if let Some(&constructor) = self.source_primary_constructors.get(&class.span) {
            if class.props.iter().all(|parameter| !parameter.is_vararg) {
                let scope = declaration_scope(self, constructor);
                let parameters = class
                    .props
                    .iter()
                    .map(|parameter| self.compact_type(&parameter.ty, scope, origin))
                    .collect::<Vec<_>>();
                self.graph
                    .add_explicit_signature_types(constructor, parameters, None, None, None);
            }
        }
        for parameter in class.props.iter().filter(|parameter| parameter.is_property) {
            let Some(stub) = self
                .source_stubs
                .get(&parameter.span)
                .copied()
                .filter(|stub| stub.kind == super::DeclarationKind::Property)
            else {
                continue;
            };
            let scope = declaration_scope(self, stub.id);
            let result = self.compact_type(&parameter.ty, scope, origin);
            self.graph
                .add_explicit_signature_types(stub.id, [], Some(result), None, None);
        }
        for constructor in &class.secondary_ctors {
            let Some(stub) = self
                .source_stubs
                .get(&constructor.span)
                .copied()
                .filter(|stub| stub.kind == super::DeclarationKind::Constructor)
            else {
                continue;
            };
            if constructor
                .params
                .iter()
                .any(|parameter| parameter.is_vararg)
            {
                continue;
            }
            let scope = declaration_scope(self, stub.id);
            let parameters = constructor
                .params
                .iter()
                .map(|parameter| self.compact_type(&parameter.ty, scope, origin))
                .collect::<Vec<_>>();
            self.graph
                .add_explicit_signature_types(stub.id, parameters, None, None, None);
        }
        for method in &class.methods {
            let Some(declaration) = self.source_functions.get(&method.span).copied() else {
                continue;
            };
            if method.params.iter().any(|parameter| parameter.is_vararg) {
                continue;
            }
            let scope = declaration_scope(self, declaration);
            let parameters = method
                .params
                .iter()
                .map(|parameter| self.compact_type(&parameter.ty, scope, origin))
                .collect::<Vec<_>>();
            let result = method
                .ret
                .as_ref()
                .map(|result| self.compact_type(result, scope, origin));
            let receiver = method
                .receiver
                .as_ref()
                .map(|receiver| self.compact_type(receiver, scope, origin));
            self.graph.add_explicit_signature_types(
                declaration,
                parameters,
                result,
                receiver,
                None,
            );
        }
        for property in &class.body_props {
            let Some(stub) = self
                .source_stubs
                .get(&property.span)
                .copied()
                .filter(|stub| stub.kind == super::DeclarationKind::Property)
            else {
                continue;
            };
            let scope = declaration_scope(self, stub.id);
            let parameters = property
                .context_params
                .iter()
                .map(|parameter| self.compact_type(&parameter.ty, scope, origin))
                .collect::<Vec<_>>();
            let result = property
                .declared_ty()
                .map(|result| self.compact_type(result, scope, origin));
            let receiver = property
                .receiver
                .as_ref()
                .map(|receiver| self.compact_type(receiver, scope, origin));
            let storage = property
                .explicit_backing_field
                .as_ref()
                .and_then(|field| field.ty.as_ref())
                .map(|storage| self.compact_type(storage, scope, origin));
            self.graph
                .add_explicit_signature_types(stub.id, parameters, result, receiver, storage);
        }
    }

    fn compact_type(
        &mut self,
        ty: &crate::ast::TypeRef,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> SigExprId {
        let visible = self
            .lexical_types
            .iter()
            .flat_map(HashMap::iter)
            .map(|(name, binding)| (name.clone(), binding.clone()))
            .collect::<HashMap<_, _>>();
        let expanded = (!visible.is_empty()).then(|| {
            let declarations = visible
                .iter()
                .filter_map(|(name, binding)| match binding {
                    CompactLexicalType::Alias(alias) => Some((
                        name.to_string(),
                        alias.formals.clone(),
                        alias.target.clone(),
                    )),
                    CompactLexicalType::Classifier(_) => None,
                })
                .collect::<Vec<_>>();
            crate::parser::expanded_type_alias_target(
                &declarations,
                ty,
                &mut std::collections::HashMap::new(),
            )
        });
        let expanded = expanded.as_ref().unwrap_or(ty);
        if expanded.arg.is_none()
            && expanded.targs.is_empty()
            && expanded.fun_params.is_empty()
            && !expanded.in_projection()
            && !expanded.out_projection()
            && !expanded.is_star_projection()
        {
            if let Some(CompactLexicalType::Classifier(declaration)) =
                visible.get(expanded.name.as_str())
            {
                let mut result = self.graph.add_expr(SigExpr::ClassifierType {
                    declaration: *declaration,
                    scope,
                });
                if expanded.nullable() {
                    result = self.graph.add_expr(SigExpr::Nullable(result));
                }
                if expanded.definitely_non_null() {
                    result = self.graph.add_expr(SigExpr::NonNullable(result));
                }
                return result;
            }
        }
        let syntax = self.graph.add_type_syntax(expanded);
        let origin = origin(ty.span);
        self.graph.add_expr(SigExpr::Type {
            syntax,
            scope,
            origin,
        })
    }

    fn class_literal_type_ref(
        &self,
        file: &File,
        receiver: ExprId,
        literal: ExprId,
    ) -> Option<(TypeRef, String)> {
        fn qualified_name(
            file: &File,
            expression: ExprId,
            names: &mut Vec<String>,
            arguments: &mut Vec<Vec<TypeRef>>,
        ) -> bool {
            match file.expr(expression) {
                Expr::Name(name) => {
                    names.push(name.clone());
                    arguments.push(
                        file.call_type_args
                            .get(&expression.0)
                            .cloned()
                            .unwrap_or_default(),
                    );
                    true
                }
                Expr::Member { receiver, name }
                | Expr::CallableRef {
                    receiver: Some(receiver),
                    name,
                } => {
                    if !qualified_name(file, *receiver, names, arguments) {
                        return false;
                    }
                    names.push(name.clone());
                    arguments.push(
                        file.call_type_args
                            .get(&expression.0)
                            .cloned()
                            .unwrap_or_default(),
                    );
                    true
                }
                _ => false,
            }
        }

        let mut names = Vec::new();
        let mut segment_arguments = Vec::new();
        if !qualified_name(file, receiver, &mut names, &mut segment_arguments) {
            return None;
        }
        let root = names.first()?.clone();
        let root_is_lexical_value = self
            .lexical_values
            .iter()
            .rev()
            .any(|values| values.contains_key(root.as_str()));
        if root_is_lexical_value {
            return None;
        }
        Some((
            TypeRef {
                name: names.join("."),
                flags: TrFlags::default(),
                arg: None,
                // A nested classifier's semantic application is ordered as its own arguments,
                // then captured arguments from the nearest enclosing classifier outward. The
                // ordinary type parser produces the same flattened order for
                // `Outer<A>.Inner<B>.Deep<C>`.
                targs: segment_arguments.into_iter().rev().flatten().collect(),
                span: file.expr_spans[literal.0 as usize],
                fun_params: Vec::new(),
                fun_context_count: 0,
            },
            root,
        ))
    }

    fn smartcast_branch(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
        name: Box<str>,
        ty: SigExprId,
    ) -> Result<SigExprId, ExpressionForm> {
        let refines_implicit_receiver = name.as_ref() == "this";
        self.lexical_values.push(HashMap::from([(name, ty)]));
        let result = self.expression(file, expression, scope, origin);
        self.lexical_values.pop();
        let result = result?;
        Ok(if refines_implicit_receiver {
            self.graph.add_expr(SigExpr::ScopedReceiver {
                receiver: ty,
                result,
                scope,
            })
        } else {
            result
        })
    }

    /// Evaluate a value that is consumed inside the inferred expression rather than exposed as the
    /// declaration's result. A public declaration approximates an anonymous object only when that
    /// object escapes through its signature; its local members remain visible while the expression
    /// continues (`(object : A() { fun bar() = ... }).bar()`).
    fn consumed_expression(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<SigExprId, ExpressionForm> {
        let approximate = self.approximate_anonymous_result;
        self.approximate_anonymous_result = false;
        let result = self.expression(file, expression, scope, origin);
        self.approximate_anonymous_result = approximate;
        result
    }

    fn smartcast_bindings_branch(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
        bindings: Vec<(Box<str>, SigExprId)>,
    ) -> Result<SigExprId, ExpressionForm> {
        if bindings.is_empty() {
            return self.expression(file, expression, scope, origin);
        }
        let receiver = bindings
            .iter()
            .rev()
            .find_map(|(name, ty)| (name.as_ref() == "this").then_some(*ty));
        self.lexical_values.push(bindings.into_iter().collect());
        let result = self.expression(file, expression, scope, origin);
        self.lexical_values.pop();
        let result = result?;
        Ok(receiver.map_or(result, |receiver| {
            self.graph.add_expr(SigExpr::ScopedReceiver {
                receiver,
                result,
                scope,
            })
        }))
    }

    /// Compact smart-cast facts established when `condition` is true. Semantic compatibility is
    /// still decided by the ordinary resolver/checker adapter when these expression types evaluate.
    fn positive_smartcasts(
        &mut self,
        file: &File,
        condition: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<Vec<(Box<str>, SigExprId)>, ExpressionForm> {
        match file.expr(condition) {
            Expr::Is {
                operand,
                ty,
                negated: false,
            } => match file.expr(*operand) {
                Expr::Name(name) => Ok(vec![(
                    name.clone().into_boxed_str(),
                    self.smartcast_type(file, *operand, ty, scope, origin)?,
                )]),
                _ => Ok(Vec::new()),
            },
            Expr::Binary {
                op: BinOp::And,
                lhs,
                rhs,
                ..
            } => {
                let mut bindings = self.positive_smartcasts(file, *lhs, scope, origin)?;
                bindings.extend(self.positive_smartcasts(file, *rhs, scope, origin)?);
                Ok(bindings)
            }
            Expr::Binary { op, lhs, rhs, .. } if matches!(op, BinOp::Ne | BinOp::RefNe) => {
                let name = match (file.expr(*lhs), file.expr(*rhs)) {
                    (Expr::Name(name), Expr::NullLit) | (Expr::NullLit, Expr::Name(name)) => {
                        Some(name)
                    }
                    _ => None,
                };
                Ok(name
                    .and_then(|name| {
                        let value = self
                            .lexical_values
                            .iter()
                            .rev()
                            .find_map(|values| values.get(name.as_str()).copied())?;
                        let non_null = self.graph.add_expr(SigExpr::NonNullable(value));
                        Some((name.clone().into_boxed_str(), non_null))
                    })
                    .into_iter()
                    .collect())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn smartcast_type(
        &mut self,
        file: &File,
        operand: ExprId,
        ty: &crate::ast::TypeRef,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<SigExprId, ExpressionForm> {
        if !file.context_sensitive_resolution_using_expected_type {
            return Ok(self.compact_type(ty, scope, origin));
        }
        let expected = self.expression(file, operand, scope, origin)?;
        let syntax = self.graph.add_type_syntax(ty);
        let origin = origin(ty.span);
        Ok(self.graph.add_expr(SigExpr::ContextualType {
            expected,
            syntax,
            scope,
            origin,
        }))
    }

    fn expression(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<SigExprId, ExpressionForm> {
        let node_origin = origin(
            file.expr_span(expression)
                .expect("a signature expression must retain its source span"),
        );
        if let Some(declaration) = file.anonymous_object_classes.get(&expression).copied() {
            let crate::ast::Decl::Class(classifier) = file.decl(declaration) else {
                unreachable!("an anonymous-object construction must name its synthetic class")
            };
            let constraint_count = self.graph.constraints().len();
            self.register_local_classifier_constraints(file, declaration, scope, origin)?;
            let demands = self.declaration_demands_since(constraint_count);
            // Kotlin exposes a non-private anonymous-object result through its single declared
            // supertype. Keeping the synthetic classifier here leaks an unpublishable local type
            // into the module signature and also hides the applied generic convention members of
            // that supertype (a common `provideDelegate` factory shape). Private declarations keep
            // their anonymous type because its extra members remain source-visible.
            let result = if self.approximate_anonymous_result {
                if let Some(base) = classifier.base_class.as_ref() {
                    let supertype = TypeRef {
                        name: base.clone(),
                        flags: TrFlags::default(),
                        arg: None,
                        targs: classifier.base_type_args.clone(),
                        span: classifier.base_class_span.unwrap_or(classifier.span),
                        fun_params: Vec::new(),
                        fun_context_count: 0,
                    };
                    self.compact_type(&supertype, scope, origin)
                } else if let [supertype] = classifier.supertypes.as_slice() {
                    self.compact_type(supertype, scope, origin)
                } else {
                    // With no single declared supertype to expose, Kotlin's denotable public result
                    // is `Any`. The synthetic anonymous classifier is a body-local implementation
                    // detail and must never enter the stable signature graph.
                    self.known(Ty::obj("kotlin/Any"))
                }
            } else {
                let declaration = self
                    .source_classifiers
                    .get(&classifier.span)
                    .copied()
                    .ok_or(ExpressionForm::Call)?;
                self.graph
                    .add_expr(SigExpr::ClassifierType { declaration, scope })
            };
            return Ok(if demands.is_empty() {
                result
            } else {
                let effects = self.graph.add_operands(demands);
                self.graph.add_expr(SigExpr::Sequence { effects, result })
            });
        }
        let node = match file.expr(expression) {
            Expr::IntLit(value) => self.integer_literal(*value),
            Expr::LongLit(_) => self.known(Ty::Long),
            Expr::UIntLit(_) => self.known(Ty::UInt),
            Expr::ULongLit(_) => self.known(Ty::ULong),
            Expr::DoubleLit(_) => self.known(Ty::Double),
            Expr::FloatLit(_) => self.known(Ty::Float),
            Expr::BoolLit(_) => self.known(Ty::Boolean),
            Expr::StringLit(_) | Expr::Template(_) => self.known(Ty::String),
            Expr::CharLit(_) => self.known(Ty::Char),
            Expr::NullLit => self.known(Ty::Null),
            Expr::Return { value, label } if self.lambda_return_matches(label.as_deref()) => {
                let value = match value {
                    Some(value) => self.expression(file, *value, scope, origin)?,
                    None => self.known(Ty::Unit),
                };
                self.lambda_returns
                    .last_mut()
                    .expect("a matched lambda return must have an active scope")
                    .values
                    .push(value);
                self.known(Ty::Nothing)
            }
            Expr::Throw { .. }
            | Expr::Return { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => self.known(Ty::Nothing),
            Expr::Is { .. } | Expr::InRange { .. } => self.known(Ty::Boolean),
            Expr::As { ty, nullable, .. } => {
                let ty = self.compact_type(ty, scope, origin);
                if *nullable {
                    self.graph.add_expr(SigExpr::Nullable(ty))
                } else {
                    ty
                }
            }
            Expr::Name(spelling) => {
                if let Some(value) = self
                    .lexical_values
                    .iter()
                    .rev()
                    .find_map(|values| values.get(spelling.as_str()).copied())
                {
                    return Ok(value);
                }
                let spelling = self.graph.intern_name(spelling);
                let selection = self.graph.add_value_selection(DeferredValueSelection {
                    scope,
                    spelling,
                    origin: node_origin,
                    expected: None,
                });
                self.graph.add_expr(SigExpr::Value(selection))
            }
            Expr::NotNull { operand } => {
                let operand = self.expression(file, *operand, scope, origin)?;
                self.graph.add_expr(SigExpr::NonNullable(operand))
            }
            Expr::Elvis { lhs, rhs } => {
                let lhs = self.expression(file, *lhs, scope, origin)?;
                let lhs = self.graph.add_expr(SigExpr::NonNullable(lhs));
                let rhs = self.expression(file, *rhs, scope, origin)?;
                let operands = self.graph.add_operands([lhs, rhs]);
                self.graph.add_expr(SigExpr::Join {
                    operands,
                    scope,
                    origin: node_origin,
                })
            }
            Expr::Member { receiver, name } => {
                let selector_origin =
                    Self::member_name_origin(file, expression, name, origin, node_origin);
                // Keep a classifier-qualified value as one deferred spelling when the root is not
                // lexical. This lets signature semantics evaluate classifier-only rungs such as a
                // `companion val C.name` before asking whether `C` denotes a runtime value. Value
                // roots still fall through to the ordinary receiver/member graph below.
                if let Some(value) = self.qualified_value(file, expression, scope, selector_origin)
                {
                    return Ok(value);
                }
                let receiver_origin = file
                    .expr_span(*receiver)
                    .map(&mut *origin)
                    .unwrap_or(node_origin);
                let receiver = match self.qualified_value(file, *receiver, scope, receiver_origin) {
                    Some(receiver) => receiver,
                    None => self.consumed_expression(file, *receiver, scope, origin)?,
                };
                self.member(receiver, name, scope, selector_origin)
            }
            Expr::SafeCall {
                receiver,
                name,
                args,
            } => {
                let selector_origin =
                    Self::member_name_origin(file, expression, name, origin, node_origin);
                let receiver_argument_origin = origin(
                    file.expr_span(*receiver)
                        .expect("a signature receiver must retain its source span"),
                );
                let receiver = self.expression(file, *receiver, scope, origin)?;
                let receiver = self.graph.add_expr(SigExpr::NonNullable(receiver));
                let selected = match args {
                    Some(args) => {
                        let lexical_callee =
                            self.lexical_callables.iter().rev().find_map(|values| {
                                values
                                    .get(name.as_str())
                                    .filter(|callable| callable.has_receiver)
                                    .map(|callable| callable.value)
                            });
                        if let Some(callee) = lexical_callee {
                            // A safe extension-function-value invocation still binds its guarded
                            // receiver as the function type's receiver parameter. Keep the local
                            // binding and explicit receiver in the compact invoke shape; member
                            // lookup cannot discover a parameter such as `b` in `a?.b(1)`.
                            let explicit =
                                self.call_arguments(file, expression, args, scope, origin)?;
                            let mut arguments =
                                Vec::with_capacity(self.graph.call_arguments(explicit).len() + 1);
                            arguments.push(SigCallArgument {
                                value: receiver,
                                origin: receiver_argument_origin,
                                name: None,
                                spread: false,
                                lambda: false,
                            });
                            arguments.extend(self.graph.call_arguments(explicit).iter().copied());
                            let arguments = self.graph.add_call_arguments(arguments);
                            self.graph.add_expr(SigExpr::Invoke {
                                callee,
                                arguments,
                                scope,
                                origin: node_origin,
                            })
                        } else {
                            let arguments =
                                self.call_arguments(file, expression, args, scope, origin)?;
                            let type_arguments =
                                self.call_type_arguments(file, expression, scope, origin);
                            self.source_member_call(
                                receiver,
                                name,
                                arguments,
                                type_arguments,
                                file.call_has_trailing_lambda.contains(&expression.0),
                                scope,
                                selector_origin,
                            )
                        }
                    }
                    None => self.member(receiver, name, scope, selector_origin),
                };
                self.graph.add_expr(SigExpr::Nullable(selected))
            }
            Expr::Index { array, indices } => {
                let receiver = self.expression(file, *array, scope, origin)?;
                let mut arguments = Vec::with_capacity(indices.len());
                for index in indices {
                    arguments.push(self.expression(file, *index, scope, origin)?);
                }
                self.member_call(receiver, "get", arguments, scope, node_origin)
            }
            Expr::RangeTo { lo, hi, kind } => {
                let receiver = self.expression(file, *lo, scope, origin)?;
                let argument = self.expression(file, *hi, scope, origin)?;
                let spelling = match kind {
                    RangeKind::Through => "rangeTo",
                    RangeKind::OpenEnd => "rangeUntil",
                    RangeKind::Until => "until",
                    RangeKind::DownTo => "downTo",
                };
                self.member_call(receiver, spelling, [argument], scope, node_origin)
            }
            Expr::IncDec { target, dec, .. } => {
                let receiver = self.expression(file, *target, scope, origin)?;
                self.member_call(
                    receiver,
                    if *dec { "dec" } else { "inc" },
                    [],
                    scope,
                    node_origin,
                )
            }
            Expr::Unary { op, operand } => {
                let receiver = self.expression(file, *operand, scope, origin)?;
                let spelling = match op {
                    UnOp::Neg => "unaryMinus",
                    UnOp::Not => "not",
                    UnOp::Plus => "unaryPlus",
                };
                self.member_call(receiver, spelling, [], scope, node_origin)
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let lhs_expression = *lhs;
                let lhs = self.expression(file, lhs_expression, scope, origin)?;
                let rhs = if *op == BinOp::And {
                    let bindings = self.positive_smartcasts(file, lhs_expression, scope, origin)?;
                    self.smartcast_bindings_branch(file, *rhs, scope, origin, bindings)?
                } else {
                    self.expression(file, *rhs, scope, origin)?
                };
                let operator = match op {
                    BinOp::Add => super::SigBinaryOperator::Add,
                    BinOp::Sub => super::SigBinaryOperator::Subtract,
                    BinOp::Mul => super::SigBinaryOperator::Multiply,
                    BinOp::Div => super::SigBinaryOperator::Divide,
                    BinOp::Rem => super::SigBinaryOperator::Remainder,
                    BinOp::Eq => super::SigBinaryOperator::Equal,
                    BinOp::Ne => super::SigBinaryOperator::NotEqual,
                    BinOp::Lt => super::SigBinaryOperator::Less,
                    BinOp::Le => super::SigBinaryOperator::LessOrEqual,
                    BinOp::Gt => super::SigBinaryOperator::Greater,
                    BinOp::Ge => super::SigBinaryOperator::GreaterOrEqual,
                    BinOp::And => super::SigBinaryOperator::BooleanAnd,
                    BinOp::Or => super::SigBinaryOperator::BooleanOr,
                    BinOp::RefEq => super::SigBinaryOperator::ReferentialEqual,
                    BinOp::RefNe => super::SigBinaryOperator::ReferentialNotEqual,
                };
                self.graph.add_expr(SigExpr::Binary {
                    operator,
                    lhs,
                    rhs,
                    scope,
                    origin: node_origin,
                })
            }
            Expr::Call { callee, args } => {
                let callee_origin = file
                    .expr_span(*callee)
                    .map(&mut *origin)
                    .unwrap_or(node_origin);
                match file.expr(*callee) {
                    Expr::Name(spelling) => {
                        if let Some(classifier) = self.local_classifier_stack.last().copied() {
                            self.register_local_member_effects(
                                file, classifier, spelling, scope, origin,
                            )?;
                        }
                        let arguments =
                            self.call_arguments(file, expression, args, scope, origin)?;
                        if let Some(callee) = self
                            .lexical_values
                            .iter()
                            .rev()
                            .find_map(|values| values.get(spelling.as_str()).copied())
                        {
                            if spelling == "this" && !self.local_classifier_stack.is_empty() {
                                let type_arguments =
                                    self.call_type_arguments(file, expression, scope, origin);
                                self.source_member_call(
                                    callee,
                                    "invoke",
                                    arguments,
                                    type_arguments,
                                    file.call_has_trailing_lambda.contains(&expression.0),
                                    scope,
                                    callee_origin,
                                )
                            } else {
                                self.graph.add_expr(SigExpr::Invoke {
                                    callee,
                                    arguments,
                                    scope,
                                    origin: node_origin,
                                })
                            }
                        } else if let Some(declaration) = self
                            .local_classifier_stack
                            .last()
                            .copied()
                            .and_then(|classifier| {
                                if !file.is_local_declaration(classifier)
                                    && !file.is_anonymous_object_class(classifier)
                                {
                                    return None;
                                }
                                let crate::ast::Decl::Class(classifier_decl) =
                                    file.decl(classifier)
                                else {
                                    return None;
                                };
                                classifier_decl
                                    .methods
                                    .iter()
                                    .any(|method| method.name == *spelling)
                                    .then(|| {
                                        self.source_classifiers.get(&classifier_decl.span).copied()
                                    })
                                    .flatten()
                            })
                        {
                            let receiver = self
                                .graph
                                .add_expr(SigExpr::ClassifierType { declaration, scope });
                            let type_arguments =
                                self.call_type_arguments(file, expression, scope, origin);
                            self.source_member_call(
                                receiver,
                                spelling,
                                arguments,
                                type_arguments,
                                file.call_has_trailing_lambda.contains(&expression.0),
                                scope,
                                callee_origin,
                            )
                        } else {
                            let type_arguments =
                                self.call_type_arguments(file, expression, scope, origin);
                            let spelling = self.graph.intern_name(spelling);
                            let target =
                                self.graph
                                    .add_callable_selection(DeferredCallableSelection {
                                        scope,
                                        spelling,
                                        origin: callee_origin,
                                        expected: None,
                                        type_arguments,
                                        trailing_lambda: file
                                            .call_has_trailing_lambda
                                            .contains(&expression.0),
                                    });
                            self.graph.add_expr(SigExpr::Call { target, arguments })
                        }
                    }
                    Expr::Member { receiver, name } => {
                        let selector_origin =
                            Self::member_name_origin(file, *callee, name, origin, node_origin);
                        if let Some(classifier) =
                            file.anonymous_object_classes.get(receiver).copied()
                        {
                            if let crate::ast::Decl::Class(class) = file.decl(classifier) {
                                if let Some(declaration) =
                                    self.source_classifiers.get(&class.span).copied()
                                {
                                    // Member-effect extraction can fail before the receiver expression
                                    // is visited. The anonymous classifier header is independent of
                                    // that member result and must still be finalized for Pass 2.
                                    self.register_local_classifier_explicit_types(
                                        file,
                                        class,
                                        declaration,
                                        scope,
                                        origin,
                                    );
                                }
                            }
                            self.register_local_member_effects(
                                file, classifier, name, scope, origin,
                            )?;
                        }
                        let lexical_callee =
                            self.lexical_callables.iter().rev().find_map(|values| {
                                values
                                    .get(name.as_str())
                                    .filter(|callable| callable.has_receiver)
                                    .map(|callable| callable.value)
                            });
                        if let Some(callee) = lexical_callee {
                            // An extension-function value is invoked with its written receiver as the
                            // function type's receiver parameter. Preserve that lexical binding in the
                            // compact graph instead of turning `value.block()` into a deferred member
                            // lookup that can never find the local parameter declaration.
                            let receiver_argument_origin = origin(
                                file.expr_span(*receiver)
                                    .expect("a signature receiver must retain its source span"),
                            );
                            let receiver = self.expression(file, *receiver, scope, origin)?;
                            let explicit =
                                self.call_arguments(file, expression, args, scope, origin)?;
                            let mut arguments =
                                Vec::with_capacity(self.graph.call_arguments(explicit).len() + 1);
                            arguments.push(SigCallArgument {
                                value: receiver,
                                origin: receiver_argument_origin,
                                name: None,
                                spread: false,
                                lambda: false,
                            });
                            arguments.extend(self.graph.call_arguments(explicit).iter().copied());
                            let arguments = self.graph.add_call_arguments(arguments);
                            self.graph.add_expr(SigExpr::Invoke {
                                callee,
                                arguments,
                                scope,
                                origin: node_origin,
                            })
                        } else if let Some(spelling) =
                            self.qualified_callable_spelling(file, *callee)
                        {
                            let arguments =
                                self.call_arguments(file, expression, args, scope, origin)?;
                            let type_arguments =
                                self.call_type_arguments(file, expression, scope, origin);
                            let spelling = self.graph.intern_name(&spelling);
                            let target =
                                self.graph
                                    .add_callable_selection(DeferredCallableSelection {
                                        scope,
                                        spelling,
                                        origin: selector_origin,
                                        expected: None,
                                        type_arguments,
                                        trailing_lambda: file
                                            .call_has_trailing_lambda
                                            .contains(&expression.0),
                                    });
                            self.graph.add_expr(SigExpr::Call { target, arguments })
                        } else {
                            let receiver =
                                self.consumed_expression(file, *receiver, scope, origin)?;
                            let arguments =
                                self.call_arguments(file, expression, args, scope, origin)?;
                            let type_arguments =
                                self.call_type_arguments(file, expression, scope, origin);
                            self.source_member_call(
                                receiver,
                                name,
                                arguments,
                                type_arguments,
                                file.call_has_trailing_lambda.contains(&expression.0),
                                scope,
                                selector_origin,
                            )
                        }
                    }
                    Expr::IntLit(_)
                    | Expr::LongLit(_)
                    | Expr::UIntLit(_)
                    | Expr::ULongLit(_)
                    | Expr::DoubleLit(_)
                    | Expr::FloatLit(_)
                    | Expr::BoolLit(_)
                    | Expr::StringLit(_)
                    | Expr::CharLit(_)
                    | Expr::NullLit
                    | Expr::AnnotationArrayLiteral(_)
                    | Expr::UnsupportedAnnotationArgument(_)
                    | Expr::NotNull { .. }
                    | Expr::Elvis { .. }
                    | Expr::Template(_)
                    | Expr::SafeCall { .. }
                    | Expr::Throw { .. }
                    | Expr::Return { .. }
                    | Expr::Break { .. }
                    | Expr::Continue { .. }
                    | Expr::Lambda { .. }
                    | Expr::Try { .. }
                    | Expr::Is { .. }
                    | Expr::As { .. }
                    | Expr::InRange { .. }
                    | Expr::RangeTo { .. }
                    | Expr::IncDec { .. }
                    | Expr::Unary { .. }
                    | Expr::Binary { .. }
                    | Expr::ExtensionAccess { .. }
                    | Expr::Index { .. }
                    | Expr::Call { .. }
                    | Expr::If { .. }
                    | Expr::Block { .. }
                    | Expr::When { .. }
                    | Expr::CallableRef { .. } => {
                        let callee = self.expression(file, *callee, scope, origin)?;
                        let arguments =
                            self.call_arguments(file, expression, args, scope, origin)?;
                        self.graph.add_expr(SigExpr::Invoke {
                            callee,
                            arguments,
                            scope,
                            origin: node_origin,
                        })
                    }
                }
            }
            Expr::Lambda { .. } => self.lambda(file, expression, scope, origin, false, None)?,
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                // The condition does not contribute to the `if` result type, but it can contain a
                // local/anonymous classifier whose stable header must be finalized before Pass 2.
                // Walk it for those structural declarations without making unrelated condition
                // calls dependencies of the enclosing declaration's inferred result.
                let _ = self.expression(file, *cond, scope, origin);
                let positive = self.positive_smartcasts(file, *cond, scope, origin)?;
                let smartcast = match file.expr(*cond) {
                    Expr::Is {
                        operand,
                        ty,
                        negated,
                    } => match file.expr(*operand) {
                        Expr::Name(name) => Some((
                            name.clone().into_boxed_str(),
                            self.smartcast_type(file, *operand, ty, scope, origin)?,
                            !*negated,
                        )),
                        _ => None,
                    },
                    Expr::Binary { op, lhs, rhs, .. }
                        if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::RefEq | BinOp::RefNe) =>
                    {
                        let name = match (file.expr(*lhs), file.expr(*rhs)) {
                            (Expr::Name(name), Expr::NullLit)
                            | (Expr::NullLit, Expr::Name(name)) => Some(name),
                            _ => None,
                        };
                        name.and_then(|name| {
                            let value = self
                                .lexical_values
                                .iter()
                                .rev()
                                .find_map(|values| values.get(name.as_str()).copied())?;
                            let non_null = self.graph.add_expr(SigExpr::NonNullable(value));
                            Some((
                                name.clone().into_boxed_str(),
                                non_null,
                                matches!(op, BinOp::Ne | BinOp::RefNe),
                            ))
                        })
                    }
                    _ => None,
                };
                let then_branch = if !positive.is_empty() {
                    self.smartcast_bindings_branch(file, *then_branch, scope, origin, positive)?
                } else if let Some((name, ty, true)) = smartcast.as_ref() {
                    self.smartcast_branch(file, *then_branch, scope, origin, name.clone(), *ty)?
                } else {
                    self.expression(file, *then_branch, scope, origin)?
                };
                let else_branch = match else_branch {
                    Some(else_branch) => {
                        if let Some((name, ty, false)) = smartcast.as_ref() {
                            self.smartcast_branch(
                                file,
                                *else_branch,
                                scope,
                                origin,
                                name.clone(),
                                *ty,
                            )?
                        } else {
                            self.expression(file, *else_branch, scope, origin)?
                        }
                    }
                    None => self.known(Ty::Unit),
                };
                let operands = self.graph.add_operands([then_branch, else_branch]);
                self.graph.add_expr(SigExpr::Join {
                    operands,
                    scope,
                    origin: node_origin,
                })
            }
            Expr::Try { body, catches, .. } => {
                let mut results = Vec::with_capacity(catches.len() + 1);
                results.push(self.expression(file, *body, scope, origin)?);
                for catch in catches {
                    let ty = self.compact_type(&catch.ty, scope, origin);
                    self.lexical_values
                        .push(HashMap::from([(catch.name.clone().into_boxed_str(), ty)]));
                    let result = self.expression(file, catch.body, scope, origin);
                    self.lexical_values.pop();
                    results.push(result?);
                }
                let operands = self.graph.add_operands(results);
                self.graph.add_expr(SigExpr::Join {
                    operands,
                    scope,
                    origin: node_origin,
                })
            }
            Expr::When { arms, .. } => {
                let mut results = Vec::with_capacity(arms.len());
                for arm in arms {
                    let smartcast = match arm.conditions.as_slice() {
                        [condition] => match file.expr(condition.expression()) {
                            Expr::Is {
                                operand,
                                ty,
                                negated: false,
                            } => match file.expr(*operand) {
                                Expr::Name(name) => Some((
                                    name.clone().into_boxed_str(),
                                    self.smartcast_type(file, *operand, ty, scope, origin)?,
                                )),
                                _ => None,
                            },
                            _ => None,
                        },
                        [] | [_, _, ..] => None,
                    };
                    if let Some((name, ty)) = smartcast {
                        results
                            .push(self.smartcast_branch(file, arm.body, scope, origin, name, ty)?);
                    } else {
                        results.push(self.expression(file, arm.body, scope, origin)?);
                    }
                }
                let operands = self.graph.add_operands(results);
                self.graph.add_expr(SigExpr::Join {
                    operands,
                    scope,
                    origin: node_origin,
                })
            }
            Expr::Block { stmts, trailing } => {
                self.lexical_values.push(HashMap::new());
                self.lexical_callables.push(HashMap::new());
                self.lexical_types.push(HashMap::new());
                let mut effects = Vec::new();
                let mut terminal_lambda_return = false;
                for (statement_index, statement) in stmts.iter().enumerate() {
                    match file.stmt(*statement) {
                        Stmt::Local {
                            name,
                            ty: None,
                            init,
                            ..
                        } => {
                            // A local binding is an intermediate value, not the enclosing public
                            // declaration's exposed result. Keep an anonymous initializer exact so
                            // later expressions in this compact body can resolve its members; the
                            // final expression is still approximated if the object itself escapes.
                            let value = self.consumed_expression(file, *init, scope, origin)?;
                            self.lexical_values
                                .last_mut()
                                .expect("block scope must exist")
                                .insert(name.clone().into_boxed_str(), value);
                        }
                        Stmt::Local {
                            name, ty: Some(ty), ..
                        }
                        | Stmt::LocalLateinit { name, ty } => {
                            let value = self.compact_type(ty, scope, origin);
                            if ty.fun_has_receiver() {
                                self.lexical_callables
                                    .last_mut()
                                    .expect("block callable scope must exist")
                                    .insert(
                                        name.clone().into_boxed_str(),
                                        CompactLexicalCallable {
                                            value,
                                            has_receiver: true,
                                        },
                                    );
                            }
                            self.lexical_values
                                .last_mut()
                                .expect("block scope must exist")
                                .insert(name.clone().into_boxed_str(), value);
                        }
                        Stmt::Destructure { entries, init } => {
                            let receiver = self.consumed_expression(file, *init, scope, origin)?;
                            let statement_origin = origin(file.stmt_spans[statement.0 as usize]);
                            for (index, entry) in entries.iter().enumerate() {
                                if entry.ignored {
                                    continue;
                                }
                                let value = self.member_call(
                                    receiver,
                                    &format!("component{}", index + 1),
                                    [],
                                    scope,
                                    statement_origin,
                                );
                                self.lexical_values
                                    .last_mut()
                                    .expect("block scope must exist")
                                    .insert(entry.name.clone().into_boxed_str(), value);
                            }
                        }
                        Stmt::Assign { value, .. } => {
                            effects.push(self.expression(file, *value, scope, origin)?);
                        }
                        Stmt::AssignMember {
                            receiver, value, ..
                        } => {
                            effects.push(self.expression(file, *receiver, scope, origin)?);
                            effects.push(self.expression(file, *value, scope, origin)?);
                        }
                        Stmt::AssignIndex {
                            array,
                            indices,
                            value,
                        } => {
                            effects.push(self.expression(file, *array, scope, origin)?);
                            for index in indices {
                                effects.push(self.expression(file, *index, scope, origin)?);
                            }
                            effects.push(self.expression(file, *value, scope, origin)?);
                        }
                        Stmt::CompoundAssign { target, value, .. } => {
                            effects.push(self.expression(file, *target, scope, origin)?);
                            effects.push(self.expression(file, *value, scope, origin)?);
                        }
                        Stmt::While { cond, body, .. } => {
                            effects.push(self.expression(file, *cond, scope, origin)?);
                            effects.push(self.expression(file, *body, scope, origin)?);
                        }
                        Stmt::DoWhile { body, cond, .. } => {
                            effects.push(self.expression(file, *body, scope, origin)?);
                            effects.push(self.expression(file, *cond, scope, origin)?);
                        }
                        Stmt::For {
                            name, range, body, ..
                        } => {
                            let start = self.expression(file, range.start, scope, origin)?;
                            let end = self.expression(file, range.end, scope, origin)?;
                            let operands = self.graph.add_operands([start, end]);
                            let element = self.graph.add_expr(SigExpr::Join {
                                operands,
                                scope,
                                origin: origin(file.stmt_spans[statement.0 as usize]),
                            });
                            self.lexical_values
                                .push(HashMap::from([(name.clone().into_boxed_str(), element)]));
                            let body = self.expression(file, *body, scope, origin);
                            self.lexical_values.pop();
                            effects.push(body?);
                        }
                        Stmt::ForEach {
                            name,
                            iterable,
                            body,
                            ..
                        } => {
                            let iterable =
                                self.consumed_expression(file, *iterable, scope, origin)?;
                            let statement_origin = origin(file.stmt_spans[statement.0 as usize]);
                            let iterator =
                                self.member_call(iterable, "iterator", [], scope, statement_origin);
                            let element =
                                self.member_call(iterator, "next", [], scope, statement_origin);
                            self.lexical_values
                                .push(HashMap::from([(name.clone().into_boxed_str(), element)]));
                            let body = self.expression(file, *body, scope, origin);
                            self.lexical_values.pop();
                            effects.push(body?);
                        }
                        Stmt::IncDec { .. } | Stmt::Break(_) | Stmt::Continue(_) => {}
                        Stmt::Return(value, label)
                            if self.lambda_return_matches(label.as_deref()) =>
                        {
                            let value = match value {
                                Some(value) => self.expression(file, *value, scope, origin)?,
                                None => self.known(Ty::Unit),
                            };
                            self.lambda_returns
                                .last_mut()
                                .expect("a matched lambda return must have an active scope")
                                .values
                                .push(value);
                            terminal_lambda_return =
                                statement_index + 1 == stmts.len() && trailing.is_none();
                        }
                        Stmt::Return(value, _) => {
                            if let Some(value) = value {
                                effects.push(self.expression(file, *value, scope, origin)?);
                            }
                        }
                        Stmt::Expr(expression) => {
                            effects.push(self.expression(file, *expression, scope, origin)?);
                        }
                        Stmt::LocalDelegate {
                            name, ty, delegate, ..
                        } => {
                            let value = match ty {
                                Some(ty) => self.compact_type(ty, scope, origin),
                                None => {
                                    let delegate =
                                        self.consumed_expression(file, *delegate, scope, origin)?;
                                    self.graph.add_expr(SigExpr::Delegate {
                                        declaration: self
                                            .graph
                                            .scope(scope)
                                            .expect("a local delegate must retain its owner scope")
                                            .owner,
                                        delegate,
                                        scope,
                                        origin: origin(file.stmt_spans[statement.0 as usize]),
                                        local: true,
                                    })
                                }
                            };
                            self.lexical_values
                                .last_mut()
                                .expect("block scope must exist")
                                .insert(name.clone().into_boxed_str(), value);
                        }
                        Stmt::LocalFun(function) => {
                            if !function.type_params.is_empty() {
                                self.register_generic_local_function_dependencies(
                                    file, function, scope, origin,
                                );
                                continue;
                            }
                            let referenced_later =
                                stmts[statement_index + 1..].iter().any(|statement| {
                                    let mut found = false;
                                    file.any_child_stmt(*statement, &mut |expression| {
                                        found =
                                            file.expr_uses_name_deep(expression, &function.name);
                                        found
                                    });
                                    found
                                }) || trailing.is_some_and(|expression| {
                                    file.expr_uses_name_deep(expression, &function.name)
                                });
                            if function.type_params.is_empty() || referenced_later {
                                let value = self.local_function(file, function, scope, origin)?;
                                self.lexical_values
                                    .last_mut()
                                    .expect("block scope must exist")
                                    .insert(function.name.clone().into_boxed_str(), value);
                                self.lexical_callables
                                    .last_mut()
                                    .expect("block callable scope must exist")
                                    .insert(
                                        function.name.clone().into_boxed_str(),
                                        CompactLexicalCallable {
                                            value,
                                            has_receiver: function.receiver.is_some(),
                                        },
                                    );
                            }
                        }
                        Stmt::LocalTypeAlias(alias) => {
                            self.lexical_types
                                .last_mut()
                                .expect("block type scope must exist")
                                .insert(
                                    alias.name.clone().into_boxed_str(),
                                    CompactLexicalType::Alias(CompactLocalTypeAlias {
                                        formals: alias.type_params.clone(),
                                        target: alias.target.clone(),
                                    }),
                                );
                        }
                        Stmt::LocalClass(_) => {
                            let constraint_count = self.graph.constraints().len();
                            // A local-class declaration is a Unit-valued statement. Failure to
                            // extract one of its lazy member effects must not stop the structural
                            // walk before later local/anonymous classifier headers are captured;
                            // an actual use of that member remains a deferred graph dependency and
                            // will fail precisely if its signature is required.
                            let _ = self.register_local_class_property_constraints(
                                file, *statement, scope, origin,
                            );
                            effects.extend(self.declaration_demands_since(constraint_count));
                            if let Some(classifier) =
                                file.local_class_decls
                                    .get(statement)
                                    .and_then(|classifier| match file.decl(*classifier) {
                                        crate::ast::Decl::Class(class) => {
                                            self.source_classifiers.get(&class.span).copied().map(
                                                |declaration| {
                                                    (
                                                        class
                                                            .name
                                                            .rsplit('.')
                                                            .next()
                                                            .unwrap_or(&class.name),
                                                        declaration,
                                                    )
                                                },
                                            )
                                        }
                                        crate::ast::Decl::Fun(_)
                                        | crate::ast::Decl::Property(_) => None,
                                    })
                            {
                                self.lexical_types
                                    .last_mut()
                                    .expect("block type scope must exist")
                                    .insert(
                                        classifier.0.to_owned().into_boxed_str(),
                                        CompactLexicalType::Classifier(classifier.1),
                                    );
                            }
                        }
                    }
                }
                let result = if terminal_lambda_return {
                    self.known(Ty::Nothing)
                } else {
                    match trailing {
                        Some(trailing) => self.expression(file, *trailing, scope, origin)?,
                        None => self.known(Ty::Unit),
                    }
                };
                self.lexical_values.pop();
                self.lexical_callables.pop();
                self.lexical_types.pop();
                if effects.is_empty() {
                    result
                } else {
                    let effects = self.graph.add_operands(effects);
                    self.graph.add_expr(SigExpr::Sequence { effects, result })
                }
            }
            Expr::CallableRef {
                receiver: None,
                name,
            } => {
                if let Some(callable) = self.lexical_callables.iter().rev().find_map(|callables| {
                    callables.get(name.as_str()).map(|callable| callable.value)
                }) {
                    return Ok(callable);
                }
                let spelling = self.graph.intern_name(name);
                let target_origin =
                    Self::member_name_origin(file, expression, name, origin, node_origin);
                let target = self
                    .graph
                    .add_callable_selection(DeferredCallableSelection {
                        scope,
                        spelling,
                        origin: target_origin,
                        expected: None,
                        type_arguments: super::OperandRange::default(),
                        trailing_lambda: false,
                    });
                self.graph.add_expr(SigExpr::CallableReference(target))
            }
            Expr::CallableRef {
                receiver: Some(receiver),
                name,
            } if name == "class" => {
                let classifier = self.class_literal_type_ref(file, *receiver, expression);
                let receiver = self.expression(file, *receiver, scope, origin)?;
                let (classifier, root) = classifier
                    .map(|(ty, root)| {
                        let classifier = self.compact_type(&ty, scope, origin);
                        let root = self.graph.intern_name(&root);
                        (Some(classifier), Some(root))
                    })
                    .unwrap_or((None, None));
                self.graph.add_expr(SigExpr::ClassLiteral {
                    receiver,
                    classifier,
                    scope,
                    root,
                })
            }
            Expr::CallableRef {
                receiver: Some(receiver),
                name,
            } => {
                let classifier = self.class_literal_type_ref(file, *receiver, expression);
                let receiver = self.expression(file, *receiver, scope, origin)?;
                let (classifier, root) = classifier
                    .map(|(ty, root)| {
                        let classifier = self.compact_type(&ty, scope, origin);
                        let root = self.graph.intern_name(&root);
                        (Some(classifier), Some(root))
                    })
                    .unwrap_or((None, None));
                let spelling = self.graph.intern_name(name);
                let target_origin =
                    Self::member_name_origin(file, expression, name, origin, node_origin);
                let target = self
                    .graph
                    .add_callable_selection(DeferredCallableSelection {
                        scope,
                        spelling,
                        origin: target_origin,
                        expected: None,
                        type_arguments: super::OperandRange::default(),
                        trailing_lambda: false,
                    });
                self.graph.add_expr(SigExpr::BoundCallableReference {
                    receiver,
                    classifier,
                    scope,
                    root,
                    target,
                })
            }
            Expr::AnnotationArrayLiteral(_)
            | Expr::UnsupportedAnnotationArgument(_)
            | Expr::ExtensionAccess { .. } => {
                return Err(super::coverage::expression_form(file.expr(expression)));
            }
        };
        Ok(node)
    }

    fn local_function(
        &mut self,
        file: &File,
        function: &crate::ast::FunDecl,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<SigExprId, ExpressionForm> {
        let mut bindings = HashMap::new();
        let mut parameters = function
            .params
            .iter()
            .map(|parameter| {
                let ty = self.compact_type(&parameter.ty, scope, origin);
                bindings.insert(parameter.name.clone().into_boxed_str(), ty);
                ty
            })
            .collect::<Vec<_>>();
        let context_count =
            u32::try_from(function.context_count).map_err(|_| ExpressionForm::Block)?;
        let receiver = function.receiver.as_ref().map(|receiver| {
            let receiver = self.compact_type(receiver, scope, origin);
            parameters.insert(function.context_count, receiver);
            bindings.insert("this".into(), receiver);
            receiver
        });
        let result = match function.ret.as_ref() {
            Some(result) => self.compact_type(result, scope, origin),
            None => {
                self.lexical_values.push(bindings);
                let result = match &function.body {
                    crate::ast::FunBody::Expr(expression) => {
                        self.expression(file, *expression, scope, origin)
                    }
                    crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => {
                        Ok(self.known(Ty::Unit))
                    }
                };
                self.lexical_values.pop();
                result?
            }
        };
        let parameters = self.graph.add_operands(parameters);
        Ok(self.graph.add_expr(SigExpr::Function {
            parameters,
            result,
            context_count,
            has_receiver: receiver.is_some(),
            suspend: function.is_suspend(),
        }))
    }

    /// A named generic local function is not a first-class generic function value, so it cannot be
    /// represented by `SigExpr::Function`. Its body may nevertheless contain a local or anonymous
    /// classifier whose inferred member signature must be finalized in Pass 1. Walk that body with
    /// its value/callable/type rungs active and retain only any compact classifier constraints it
    /// registers; unrelated expression failures are intentionally not promoted to a constraint.
    fn register_generic_local_function_dependencies(
        &mut self,
        file: &File,
        function: &crate::ast::FunDecl,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) {
        let mut bindings = HashMap::new();
        let mut callables = HashMap::new();
        for parameter in &function.params {
            let ty = self.compact_type(&parameter.ty, scope, origin);
            if parameter.ty.fun_has_receiver() {
                callables.insert(
                    parameter.name.clone().into_boxed_str(),
                    CompactLexicalCallable {
                        value: ty,
                        has_receiver: true,
                    },
                );
            }
            bindings.insert(parameter.name.clone().into_boxed_str(), ty);
        }
        if let Some(receiver) = &function.receiver {
            let receiver = self.compact_type(receiver, scope, origin);
            bindings.insert("this".into(), receiver);
            bindings.insert(format!("this@{}", function.name).into_boxed_str(), receiver);
        }
        self.lexical_values.push(bindings);
        self.lexical_callables.push(callables);
        self.lexical_types.push(HashMap::new());
        if let crate::ast::FunBody::Expr(body) | crate::ast::FunBody::Block(body) = function.body {
            let _ = self.expression(file, body, scope, origin);
        }
        self.lexical_types.pop();
        self.lexical_callables.pop();
        self.lexical_values.pop();
    }

    fn lambda(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
        contextual: bool,
        implicit_label: Option<&str>,
    ) -> Result<SigExprId, ExpressionForm> {
        let Expr::Lambda { params, body } = file.expr(expression) else {
            unreachable!("lambda extraction requires a lambda expression")
        };
        let declared = file.lambda_param_types.get(&expression.0);
        let anonymous_function = file.anon_fun_lambdas.contains(&expression.0);
        let fully_typed = params.is_empty()
            || declared.is_some_and(|declared| {
                declared.len() == params.len() && declared.iter().all(Option::is_some)
            });
        // Anonymous functions share the parser's lambda payload but own a function declaration
        // shape. `fun(): R { ... }` never receives an implicit `it`; in particular, contextualizing
        // it as a lambda would bypass `anon_fun_ret` below and turn a block ending in `return value`
        // into `() -> Unit` during compact signature inference.
        let implicit_parameter_allowed = !anonymous_function
            && params.is_empty()
            && !file.lambda_explicit_arrows.contains(&expression.0);
        // Written value-parameter types do not make a lambda context-free. A call-site expectation
        // may still contribute leading context parameters or an extension receiver (notably for a
        // fun-interface constructor), and its result type still owns contextual Unit coercion.
        // Anonymous functions retain their declaration-shaped handling below.
        let contextual = !fully_typed || (contextual && !anonymous_function);
        let return_label = file
            .lambda_labels
            .get(&expression.0)
            .map(String::as_str)
            .or(implicit_label)
            .map(Box::<str>::from);
        let lambda_origin = origin(file.expr_spans[expression.0 as usize]);
        let declaration = self
            .graph
            .scope(scope)
            .expect("a lambda must retain its declaration scope")
            .owner;
        let mut bindings = HashMap::new();
        let mut parameter_types = Vec::with_capacity(params.len().max(1));
        if contextual && implicit_parameter_allowed {
            let parameter = self
                .graph
                .add_expr(SigExpr::ContextualParameter(declaration));
            bindings.insert("it".into(), parameter);
            parameter_types.push(parameter);
        } else {
            for (index, name) in params.iter().enumerate() {
                let ty = match declared
                    .and_then(|declared| declared.get(index))
                    .and_then(Option::as_ref)
                {
                    Some(ty) => self.compact_type(ty, scope, origin),
                    None if contextual => self
                        .graph
                        .add_expr(SigExpr::ContextualParameter(declaration)),
                    None => return Err(ExpressionForm::Lambda),
                };
                bindings.insert(name.clone().into_boxed_str(), ty);
                parameter_types.push(ty);
            }
        }
        if contextual {
            // A contextual function literal does not know whether its expected function type has
            // an extension receiver until signature evaluation. Defer bare `this` to that lexical
            // moment: the evaluator first enters the expected receiver rung, while a non-receiver
            // expectation naturally falls through to the enclosing declaration's `this`.
            let spelling = self.graph.intern_name("this");
            let selection = self.graph.add_value_selection(DeferredValueSelection {
                scope,
                spelling,
                origin: lambda_origin,
                expected: None,
            });
            let this = self.graph.add_expr(SigExpr::Value(selection));
            bindings.insert("this".into(), this);
            self.lexical_values.push(bindings);
            self.lambda_returns.push(CompactLambdaReturnScope {
                label: return_label.clone(),
                values: Vec::new(),
            });
            let result = self.expression(file, *body, scope, origin);
            let returns = self
                .lambda_returns
                .pop()
                .expect("a contextual lambda must close its return scope");
            self.lexical_values.pop();
            let result = self.finish_lambda_result(result?, returns, scope, lambda_origin);
            let parameters = self.graph.add_operands(parameter_types);
            return Ok(self.graph.add_expr(SigExpr::ContextualFunction {
                parameters,
                result,
                scope,
                implicit_it: params.is_empty(),
                suspend: file.suspend_lambdas.contains(&expression.0),
            }));
        }

        let context_count = file
            .anon_fun_context_count
            .get(&expression.0)
            .copied()
            .unwrap_or(0);
        if context_count as usize > parameter_types.len() {
            return Err(ExpressionForm::Lambda);
        }
        let receiver = file
            .anon_fun_receivers
            .get(&expression.0)
            .map(|receiver| self.compact_type(receiver, scope, origin));
        if let Some(receiver) = receiver {
            parameter_types.insert(context_count as usize, receiver);
            bindings.insert("this".into(), receiver);
        }
        self.lexical_values.push(bindings);
        self.lambda_returns.push(CompactLambdaReturnScope {
            label: return_label,
            values: Vec::new(),
        });
        let result = self.expression(file, *body, scope, origin);
        let returns = self
            .lambda_returns
            .pop()
            .expect("a lambda must close its return scope");
        self.lexical_values.pop();
        let result = match file.anon_fun_ret.get(&expression.0) {
            Some(declared) => self.compact_type(declared, scope, origin),
            None => self.finish_lambda_result(result?, returns, scope, lambda_origin),
        };
        let parameters = self.graph.add_operands(parameter_types);
        Ok(self.graph.add_expr(SigExpr::Function {
            parameters,
            result,
            context_count,
            has_receiver: receiver.is_some(),
            suspend: file.suspend_lambdas.contains(&expression.0),
        }))
    }

    fn member(
        &mut self,
        receiver: SigExprId,
        spelling: &str,
        scope: SignatureScopeId,
        origin: OriginId,
    ) -> SigExprId {
        let spelling = self.graph.intern_name(spelling);
        let lookup = self.graph.add_member_selection(DeferredMemberSelection {
            scope,
            spelling,
            origin,
            expected: None,
            type_arguments: super::OperandRange::default(),
            trailing_lambda: false,
        });
        self.graph.add_expr(SigExpr::Member {
            receiver,
            lookup,
            origin,
        })
    }

    fn member_name_origin(
        file: &File,
        expression: ExprId,
        name: &str,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
        fallback: OriginId,
    ) -> OriginId {
        file.exact_member_name_spans
            .get(&expression.0)
            .copied()
            .or_else(|| {
                file.expr_span(expression).map(|span| {
                    crate::diag::Span::new(span.hi.saturating_sub(name.len() as u32), span.hi)
                })
            })
            .map(origin)
            .unwrap_or(fallback)
    }

    fn member_call(
        &mut self,
        receiver: SigExprId,
        spelling: &str,
        arguments: impl IntoIterator<Item = SigExprId>,
        scope: SignatureScopeId,
        origin: OriginId,
    ) -> SigExprId {
        let spelling = self.graph.intern_name(spelling);
        let target = self.graph.add_member_selection(DeferredMemberSelection {
            scope,
            spelling,
            origin,
            expected: None,
            type_arguments: super::OperandRange::default(),
            trailing_lambda: false,
        });
        let arguments = self
            .graph
            .add_call_arguments(arguments.into_iter().map(|value| SigCallArgument {
                value,
                origin,
                name: None,
                spread: false,
                lambda: false,
            }));
        self.graph.add_expr(SigExpr::MemberCall {
            receiver,
            target,
            arguments,
            origin,
        })
    }

    fn source_member_call(
        &mut self,
        receiver: SigExprId,
        spelling: &str,
        arguments: super::CallArgumentRange,
        type_arguments: super::OperandRange,
        trailing_lambda: bool,
        scope: SignatureScopeId,
        origin: OriginId,
    ) -> SigExprId {
        let spelling = self.graph.intern_name(spelling);
        let target = self.graph.add_member_selection(DeferredMemberSelection {
            scope,
            spelling,
            origin,
            expected: None,
            type_arguments,
            trailing_lambda,
        });
        self.graph.add_expr(SigExpr::MemberCall {
            receiver,
            target,
            arguments,
            origin,
        })
    }

    fn call_arguments(
        &mut self,
        file: &File,
        call: ExprId,
        arguments: &[ExprId],
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<super::CallArgumentRange, ExpressionForm> {
        let names = file.call_arg_names.get(&call.0);
        let implicit_lambda_label = match file.expr(call) {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Name(name) | Expr::Member { name, .. } => Some(name.as_str()),
                _ => None,
            },
            _ => None,
        };
        let mut compact = Vec::with_capacity(arguments.len());
        for (index, argument) in arguments.iter().copied().enumerate() {
            let value = if matches!(file.expr(argument), Expr::Lambda { .. }) {
                self.lambda(file, argument, scope, origin, true, implicit_lambda_label)?
            } else {
                self.expression(file, argument, scope, origin)?
            };
            let name = names
                .and_then(|names| names.get(index))
                .and_then(Option::as_deref)
                .map(|name| self.graph.intern_name(name));
            compact.push(SigCallArgument {
                value,
                origin: origin(
                    file.expr_span(argument)
                        .expect("a signature argument must retain its source span"),
                ),
                name,
                spread: file.is_spread_arg(argument),
                lambda: matches!(file.expr(argument), Expr::Lambda { .. }),
            });
        }
        Ok(self.graph.add_call_arguments(compact))
    }

    fn call_type_arguments(
        &mut self,
        file: &File,
        call: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> super::OperandRange {
        let arguments = file
            .call_type_args
            .get(&call.0)
            .into_iter()
            .flatten()
            .map(|argument| self.compact_type(argument, scope, origin))
            .collect::<Vec<_>>();
        self.graph.add_operands(arguments)
    }
}

fn source_function(file: &File, range: crate::diag::Span) -> Option<&crate::ast::FunDecl> {
    for declaration in &file.decl_arena {
        match declaration {
            crate::ast::Decl::Fun(function) if function.span == range => return Some(function),
            crate::ast::Decl::Class(class) => {
                if let Some(function) = class
                    .methods
                    .iter()
                    .chain(
                        class
                            .enum_entries
                            .iter()
                            .flat_map(|entry| entry.methods.iter()),
                    )
                    .find(|function| function.span == range)
                {
                    return Some(function);
                }
            }
            crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_) => {}
        }
    }
    None
}

fn source_property(file: &File, range: crate::diag::Span) -> Option<&crate::ast::PropDecl> {
    for declaration in &file.decl_arena {
        match declaration {
            crate::ast::Decl::Property(property) if property.span == range => {
                return Some(property);
            }
            crate::ast::Decl::Class(class) => {
                if let Some(property) = class
                    .body_props
                    .iter()
                    .chain(
                        class
                            .enum_entries
                            .iter()
                            .flat_map(|entry| entry.props.iter()),
                    )
                    .find(|property| property.span == range)
                {
                    return Some(property);
                }
            }
            crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_) => {}
        }
    }
    None
}

fn source_signature_expression(file: &File, stub: &DeclarationStub) -> Option<ExprId> {
    match stub.kind {
        super::DeclarationKind::Function => match source_function(file, stub.range)?.body {
            crate::ast::FunBody::Expr(expression) => Some(expression),
            crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => None,
        },
        super::DeclarationKind::Property => {
            let property = source_property(file, stub.range)?;
            property
                .delegate
                .or(property.init)
                .or_else(|| match property.getter.as_ref() {
                    Some(crate::ast::FunBody::Expr(expression)) => Some(*expression),
                    Some(crate::ast::FunBody::Block(_) | crate::ast::FunBody::None) | None => None,
                })
        }
        super::DeclarationKind::Classifier
        | super::DeclarationKind::EnumEntry
        | super::DeclarationKind::TypeAlias
        | super::DeclarationKind::Constructor
        | super::DeclarationKind::Accessor
        | super::DeclarationKind::Initializer
        | super::DeclarationKind::Script => None,
    }
}
