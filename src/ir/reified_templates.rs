//! Which code is a reified declaration's own template.
//!
//! Only an `inline` function may declare a reified type parameter, and every expansion that
//! executes substitutes the call's type argument for it. The declaration still keeps its own body
//! in the file, together with the lambdas and anonymous objects that body declares. That template
//! runs only if the declaration were called directly instead of inlined. A target that cannot
//! realize a runtime operation on the unsubstituted parameter may therefore stop the template
//! there. Anywhere else, an occurrence that survived is a lowering defect the target must refuse.
//!
//! Ownership is read from recorded provenance only: a function's own generic signature, the
//! enclosure of the lambda node or class that declares the code, the specialized copies an
//! expansion made, and the inline-copy marks on code an expansion copied into its caller.

use super::{ClassId, ExprId, FunId, IrEnclosure, IrExpr, IrFile};
use crate::types::Ty;

/// Where the provenance says a function is declared.
enum DeclaringScope {
    /// A member of this class.
    Member(ClassId),
    /// A lambda or anonymous function created by its one lambda node, written in this scope.
    LambdaNode(IrEnclosure),
    /// A function no class or lambda node owns.
    Declaration,
    /// Created by copied code or by more than one node, or without a recorded scope.
    Unproven,
}

impl IrFile {
    /// Whether the type operation `operation` against `tested`, emitted in `scope`, belongs to the
    /// template of the declaration that declares the reified parameter `tested` names.
    pub(crate) fn is_reified_template_operation(
        &self,
        scope: IrEnclosure,
        operation: ExprId,
        tested: Ty,
    ) -> bool {
        if self.is_inline_copy(operation) {
            return false;
        }
        let mut current = scope;
        // Each step leaves a function or a class for the scope that declares it, so a walk longer
        // than every function and class is a cycle in the recorded provenance.
        for _ in 0..=self.functions.len() + self.classes.len() {
            let next = match current {
                IrEnclosure::Function(function) | IrEnclosure::Lambda(function) => {
                    if self.specialized_functions.contains_key(&function) {
                        return false;
                    }
                    match self.declaring_scope(function) {
                        // A lambda or anonymous function declares no type parameter of its own;
                        // one its signature captures continues the walk to its declaration.
                        DeclaringScope::LambdaNode(enclosure) => Some(enclosure),
                        DeclaringScope::Unproven => return false,
                        DeclaringScope::Member(class) => {
                            if self.declares_reified_occurrence(function, tested) {
                                return true;
                            }
                            Some(IrEnclosure::Classifier(class))
                        }
                        DeclaringScope::Declaration => {
                            return self.declares_reified_occurrence(function, tested);
                        }
                    }
                }
                IrEnclosure::Constructor { class, .. }
                | IrEnclosure::ClassInitializer(class)
                | IrEnclosure::Classifier(class) => {
                    if !self.is_template_class(class) {
                        return false;
                    }
                    self.classes[class as usize].enclosure
                }
                IrEnclosure::PropertyAccessor { .. } | IrEnclosure::File => None,
            };
            let Some(next) = next else {
                return false;
            };
            current = next;
        }
        false
    }

    /// Whether `function` itself declares a reified parameter that `tested` names. The leading
    /// parameters a lifted local or anonymous function's signature captures from an enclosing
    /// declaration are that declaration's, not this function's.
    fn declares_reified_occurrence(&self, function: FunId, tested: Ty) -> bool {
        self.declared_type_parameters(function).any(|parameter| {
            parameter.reified
                && tested
                    .type_parameter_occurrence_bound(&parameter.semantic_name)
                    .is_some()
        })
    }

    /// The type parameters `function` itself declares: its generic signature without the leading
    /// ones a lifted local or anonymous function captures from an enclosing declaration.
    pub(crate) fn declared_type_parameters(
        &self,
        function: FunId,
    ) -> impl Iterator<Item = &super::IrTypeParameter> {
        let captured = self
            .captured_signature_type_parameters
            .get(&function)
            .copied()
            .unwrap_or(0);
        self.signatures
            .get(&function)
            .into_iter()
            .flat_map(move |signature| signature.type_params.iter().skip(captured))
    }

    /// Where `function` is declared: as a member of its class, or by the one lambda node that
    /// creates it, written in a recorded scope. A function no class or lambda node owns is a
    /// declaration of its own. A lambda implementation that an expansion's copied code creates,
    /// or that more than one node creates, has no proven declaring scope.
    fn declaring_scope(&self, function: FunId) -> DeclaringScope {
        if let Some(class) = self
            .classes
            .iter()
            .position(|class| class.methods.contains(&function))
        {
            return DeclaringScope::Member(class as ClassId);
        }
        let mut nodes = self
            .exprs
            .iter()
            .enumerate()
            .filter_map(|(node, expression)| {
                matches!(expression, IrExpr::Lambda { impl_fn, .. } if *impl_fn == function)
                    .then_some(node as ExprId)
            });
        let Some(node) = nodes.next() else {
            return DeclaringScope::Declaration;
        };
        if nodes.next().is_some() || self.is_inline_copy(node) {
            return DeclaringScope::Unproven;
        }
        self.callable_reference_enclosures
            .get(&node)
            .copied()
            .map_or(DeclaringScope::Unproven, DeclaringScope::LambdaNode)
    }

    /// Whether `class` is a declaration's own class rather than an expansion's copy of one: no
    /// expansion specialized it, and no code an expansion copied into its caller constructs it.
    fn is_template_class(&self, class: ClassId) -> bool {
        if self.specialized_anonymous_classes.contains_key(&class) {
            return false;
        }
        let name = self.classes[class as usize].fq_name;
        !self.exprs.iter().enumerate().any(|(node, expression)| {
            matches!(expression, IrExpr::New { internal, .. } if *internal == name)
                && self.is_inline_copy(node as ExprId)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrFunction, IrGenericSig, IrTypeParameter};

    fn parameter() -> IrTypeParameter {
        IrTypeParameter {
            name: "T".to_owned(),
            semantic_name: "\0tp:reified-template:T".to_owned(),
            bounds: Vec::new(),
            variance: crate::types::TypeVariance::Invariant,
            reified: true,
        }
    }

    fn function(ir: &mut IrFile, name: &str) -> FunId {
        ir.add_fun(IrFunction {
            name: name.to_owned(),
            params: Vec::new(),
            ret: Ty::Boolean,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        })
    }

    /// A reified declaration and the lambda its body declares, with the lambda's type operation.
    fn template() -> (IrFile, FunId, ExprId, ExprId) {
        let mut ir = IrFile::default();
        let declaration = function(&mut ir, "checks");
        ir.signatures.insert(
            declaration,
            IrGenericSig {
                type_params: vec![parameter()],
                params: Vec::new(),
                ret: Some(Ty::Boolean),
                supers: Vec::new(),
            },
        );
        let lambda = function(&mut ir, "checks$lambda");
        let node = ir.add_expr(IrExpr::Lambda {
            impl_fn: lambda,
            arity: 1,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        });
        ir.callable_reference_enclosures
            .insert(node, IrEnclosure::Function(declaration));
        let operand = ir.add_expr(IrExpr::GetValue(0));
        let operation = ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::InstanceOf,
            arg: operand,
            type_operand: parameter().ty(),
        });
        (ir, lambda, node, operation)
    }

    #[test]
    fn a_declared_lambda_is_its_reified_declarations_template() {
        let (ir, lambda, _, operation) = template();
        let scope = IrEnclosure::Function(lambda);
        assert!(ir.is_reified_template_operation(scope, operation, parameter().ty()));
        let other = Ty::ty_param("\0tp:other:T", Ty::nullable(Ty::obj("kotlin/Any")));
        assert!(!ir.is_reified_template_operation(scope, operation, other));
    }

    #[test]
    fn a_captured_signature_parameter_is_its_enclosing_declarations() {
        let (mut ir, lambda, node, operation) = template();
        ir.signatures.insert(
            lambda,
            IrGenericSig {
                type_params: vec![parameter()],
                params: Vec::new(),
                ret: Some(Ty::Boolean),
                supers: Vec::new(),
            },
        );
        ir.captured_signature_type_parameters.insert(lambda, 1);
        let scope = IrEnclosure::Function(lambda);
        assert!(ir.is_reified_template_operation(scope, operation, parameter().ty()));
        ir.mark_inline_copy(node);
        assert!(!ir.is_reified_template_operation(scope, operation, parameter().ty()));

        // A local function that captures `T` and no node creates is not the declaration either.
        let (mut ir, _, _, operation) = template();
        let local = function(&mut ir, "local");
        ir.signatures.insert(
            local,
            IrGenericSig {
                type_params: vec![parameter()],
                params: Vec::new(),
                ret: Some(Ty::Boolean),
                supers: Vec::new(),
            },
        );
        ir.captured_signature_type_parameters.insert(local, 1);
        assert!(!ir.is_reified_template_operation(
            IrEnclosure::Function(local),
            operation,
            parameter().ty()
        ));
    }

    #[test]
    fn code_an_expansion_copied_is_never_a_template() {
        let (mut ir, lambda, _, operation) = template();
        ir.mark_inline_copy(operation);
        assert!(!ir.is_reified_template_operation(
            IrEnclosure::Function(lambda),
            operation,
            parameter().ty()
        ));

        let (mut ir, lambda, node, operation) = template();
        ir.mark_inline_copy(node);
        assert!(!ir.is_reified_template_operation(
            IrEnclosure::Function(lambda),
            operation,
            parameter().ty()
        ));
    }
}
