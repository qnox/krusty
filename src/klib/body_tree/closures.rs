//! Lambdas, callable references, function-value calls, and the boxes of variables closures
//! write, as kotlinc's serialized IR records them.

use super::{
    assigned_locals, call, BodyExprId, BodyKind, BodyLambda, CallTarget, Frame, Translation,
    ValueRef,
};
use crate::fx_hash::HashMap;
use crate::ir::{ExprId, IrExpr};
use crate::metadata::id_signature::{
    accessor_signature, callable_signature, Accessor, CallableShape, ClassScope,
    DeclarationContainer, KlibPublicIdSignature, Placement, PropertyShape, TypeParameterShape,
};
use crate::metadata::semantic::KotlinType;
use crate::types::{Ty, TyKind};

impl Translation<'_, '_> {
    /// A lambda: its implementation function's body translated as the body of a local function,
    /// whose leading parameters read the values the lambda captures where they are declared.
    pub(super) fn lambda(
        &mut self,
        expression: ExprId,
        implementation: crate::ir::FunId,
        arity: u8,
        captures: &[ExprId],
    ) -> Option<BodyExprId> {
        let ir = self.scope.ir;
        // A lambda's value is a function of its implementation's own parameters.
        let function = ir.functions.get(implementation as usize)?;
        let ty = match ir.logical_type(expression) {
            Some(ty) => function_class(ty)?,
            None => Ty::obj_args_name(
                crate::types::TypeName::from(format!("kotlin/Function{arity}").as_str()),
                &function
                    .params
                    .get(captures.len()..)?
                    .iter()
                    .copied()
                    .chain(std::iter::once(function.ret))
                    .collect::<Vec<_>>(),
            ),
        };
        let captured = captures
            .iter()
            .map(|capture| match ir.expr(*capture) {
                IrExpr::GetValue(slot) => self.value(*slot),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        self.local_function(LocalFunction {
            implementation,
            arity: usize::from(arity),
            captured,
            ty,
            name: "<anonymous>".to_owned(),
            overridden_class: "kotlin/Function",
            reflection_target: None,
            origin: Some("LAMBDA"),
        })
    }

    /// `::function`: the reference's adapter translated as the local function a reflective
    /// function value invokes, naming the referenced function as its reflection target.
    pub(super) fn callable_reference(
        &mut self,
        reference: &crate::ir::IrCallableReference,
    ) -> Option<BodyExprId> {
        let ir = self.scope.ir;
        let crate::ir::IrCallableReferenceTarget::Module(callable) = reference.target else {
            return None;
        };
        if !reference.captures.is_empty()
            || reference.bound_contexts != 0
            || reference.bound_receiver.is_some()
            || reference.adaptation.is_some()
            || reference.declaration_suspend
        {
            return None;
        }
        let function = *ir.checked_callable_functions.get(&callable)?;
        let (signature, None) = self.scope.declarations.get(function)? else {
            return None;
        };
        let TyKind::Fun(shape) = reference.function_type.kind() else {
            return None;
        };
        // A reference's value is a `KFunctionN` of the function type's parameters and result.
        let arguments = shape
            .params
            .iter()
            .copied()
            .chain(std::iter::once(shape.ret))
            .collect::<Vec<_>>();
        let ty = Ty::obj_args_name(
            crate::types::TypeName::from(
                format!("kotlin/reflect/KFunction{}", shape.params.len()).as_str(),
            ),
            &arguments,
        );
        self.local_function(LocalFunction {
            implementation: reference.adapter,
            arity: shape.params.len(),
            captured: Vec::new(),
            ty,
            name: ir.fn_source_names.get(&function)?.clone(),
            overridden_class: "kotlin/reflect/KFunction",
            reflection_target: Some(CallTarget::Public(signature.clone())),
            origin: None,
        })
    }

    pub(super) fn local_function(&mut self, found: LocalFunction) -> Option<BodyExprId> {
        let ir = self.scope.ir;
        let function = ir.functions.get(found.implementation as usize)?;
        let captures = found.captured.len();
        if function.dispatch_receiver.is_some()
            || function.params.len() != captures + found.arity
            || !self.supported(found.ty)
            || !self.supported(function.ret)
            || ir.suspend_funs.contains(&found.implementation)
        {
            return None;
        }
        let TyKind::Obj(_, arguments) = found.ty.kind() else {
            return None;
        };
        // Function and function-value classes take each parameter, then the result.
        if arguments.len() != found.arity + 1 {
            return None;
        }
        let id = self.next_lambda;
        self.next_lambda += 1;
        let names = ir.function_parameter_identities(found.implementation);
        let parameters = function.params[captures..]
            .iter()
            .enumerate()
            .map(|(index, ty)| {
                let name = names
                    .and_then(|names| names.get(captures + index))
                    .and_then(|identity| identity.source_name.clone())
                    .unwrap_or_else(|| format!("p{index}"));
                self.supported(*ty).then_some((name, *ty))
            })
            .collect::<Option<Vec<_>>>()?;
        let body = function.body?;
        let frame =
            Frame {
                parameters: found
                    .captured
                    .into_iter()
                    .chain((0u32..).zip(&parameters).map(|(index, (_, ty))| {
                        (ValueRef::LambdaParameter { lambda: id, index }, *ty)
                    }))
                    .collect(),
                locals: HashMap::default(),
                assigned: assigned_locals(ir, body),
                loops: Vec::new(),
                lambda: Some(id),
            };
        let enclosing = std::mem::replace(&mut self.frame, frame);
        let statements = match ir.expr(body) {
            IrExpr::Block { stmts, value: None } => stmts
                .iter()
                .map(|statement| self.statement(*statement))
                .collect::<Option<Vec<_>>>(),
            _ => None,
        };
        self.frame = enclosing;
        let operands = (0..found.arity)
            .map(|index| format!("1:{index}"))
            .collect::<Vec<_>>()
            .join(";");
        let class = KlibPublicIdSignature::classifier(crate::types::TypeName::from(
            format!("{}{}", found.overridden_class, found.arity).as_str(),
        ));
        self.push(
            found.ty,
            BodyKind::Lambda(Box::new(BodyLambda {
                id,
                name: found.name,
                parameters,
                result: function.ret,
                statements: statements?,
                overridden: class.member_function("invoke", &format!("invoke({operands}){{}}")),
                reflection_target: found.reflection_target,
                origin: found.origin,
            })),
        )
    }

    /// A call of a function value, `FunctionN.invoke` over the value and its arguments.
    pub(super) fn invoke_function(
        &mut self,
        function: ExprId,
        args: &[ExprId],
        params: &[Ty],
        ret: Ty,
    ) -> Option<BodyExprId> {
        if params.len() != args.len() || !self.supported(ret) {
            return None;
        }
        let function = self.expr(function)?;
        let TyKind::Obj(class, _) = function_class(self.ty(function))?.kind() else {
            return None;
        };
        let operands = (0..args.len())
            .map(|index| format!("1:{index}"))
            .collect::<Vec<_>>()
            .join(";");
        let target = KlibPublicIdSignature::classifier(class)
            .member_function("invoke", &format!("invoke({operands}){{}}"));
        let mut arguments = vec![function];
        for argument in args {
            arguments.push(self.expr(*argument)?);
        }
        self.push(
            ret,
            call(CallTarget::Public(target), arguments, Some("INVOKE")),
        )
    }

    /// A variable a closure writes, boxed as kotlinc boxes it: in a `SharedVariableBox<T>`.
    pub(super) fn shared_box(&mut self, element: Ty, init: ExprId) -> Option<BodyExprId> {
        let init = self.expr(init)?;
        if !self.supported(element) {
            return None;
        }
        let ty = shared_box_type(element);
        self.push(
            ty,
            BodyKind::ConstructorCall {
                target: shared_box_member(SharedBoxMember::Constructor)?,
                constructor_type_arguments: 0,
                type_arguments: vec![element],
                arguments: vec![init],
                origin: None,
            },
        )
    }

    pub(super) fn shared_read(&mut self, holder: ExprId, element: Ty) -> Option<BodyExprId> {
        let holder = self.expr(holder)?;
        if self.ty(holder) != shared_box_type(element) {
            return None;
        }
        let target = shared_box_member(SharedBoxMember::Getter)?;
        self.push(element, call(target, vec![holder], None))
    }

    pub(super) fn shared_write(
        &mut self,
        holder: ExprId,
        element: Ty,
        value: ExprId,
    ) -> Option<BodyExprId> {
        let holder = self.expr(holder)?;
        if self.ty(holder) != shared_box_type(element) {
            return None;
        }
        let value = self.expr(value)?;
        let target = shared_box_member(SharedBoxMember::Setter)?;
        self.push(Ty::Unit, call(target, vec![holder, value], Some("EQ")))
    }
}

/// A common-IR function a lambda or callable reference invokes, with the values it captures.
pub(super) struct LocalFunction {
    implementation: crate::ir::FunId,
    arity: usize,
    captured: Vec<(ValueRef, Ty)>,
    /// The function value's Kotlin class type.
    ty: Ty,
    name: String,
    /// The function class family whose `invoke` the local function implements, without arity.
    overridden_class: &'static str,
    reflection_target: Option<CallTarget>,
    origin: Option<&'static str>,
}

/// The Kotlin class of a function value's type: `kotlin.FunctionN` for a function type, or the
/// class it already is (`KFunctionN`).
pub(super) fn function_class(ty: Ty) -> Option<Ty> {
    match ty.kind() {
        TyKind::Fun(_) => function_type_class(ty),
        TyKind::Obj(..) => Some(ty),
        _ => None,
    }
}

/// `(P1, …, Pn) -> R` as the class type `kotlin.FunctionN<P1, …, Pn, R>` Kotlin IR writes; `None`
/// for a function type with receivers, contexts or `suspend`, which carry more than the class.
pub(in crate::klib) fn function_type_class(ty: Ty) -> Option<Ty> {
    let TyKind::Fun(shape) = ty.kind() else {
        return None;
    };
    if shape.has_receiver || shape.context_count != 0 || shape.suspend {
        return None;
    }
    let arguments = shape
        .params
        .iter()
        .copied()
        .chain(std::iter::once(shape.ret))
        .collect::<Vec<_>>();
    Some(Ty::obj_args_name(
        crate::types::TypeName::from(format!("kotlin/Function{}", shape.params.len()).as_str()),
        &arguments,
    ))
}

/// `kotlin.internal.SharedVariableBox<T>`, the box kotlinc's serialized IR keeps a variable a
/// closure writes in.
pub(super) fn shared_box_type(element: Ty) -> Ty {
    Ty::obj_args_name(
        crate::types::TypeName::from("kotlin/internal/SharedVariableBox"),
        &[element],
    )
}

#[derive(Clone, Copy)]
pub(super) enum SharedBoxMember {
    Constructor,
    Getter,
    Setter,
}

/// The constructor or an accessor of `SharedVariableBox.element`, over the box's own type
/// parameter.
pub(super) fn shared_box_member(member: SharedBoxMember) -> Option<CallTarget> {
    let id = crate::metadata::semantic::KotlinTypeParameterId(0);
    let element = KotlinType::Param {
        name: "T".to_owned(),
        id,
        nullable: false,
    };
    let package = ["kotlin".to_owned(), "internal".to_owned()];
    let classes = [ClassScope {
        name: "SharedVariableBox",
        type_parameters: vec![TypeParameterShape {
            id,
            bounds: Vec::new(),
        }],
        expect: false,
    }];
    let container = DeclarationContainer::<KotlinType> {
        package: &package,
        classes: &classes,
        native_interop_library: false,
    };
    let property = PropertyShape::<KotlinType> {
        name: "element",
        contexts: Vec::new(),
        receiver: None,
        type_parameters: Vec::new(),
        expect: false,
        placement: Placement::Ordinary,
    };
    Some(match member {
        SharedBoxMember::Constructor => CallTarget::Public(
            callable_signature(
                container,
                &CallableShape {
                    name: "<init>",
                    contexts: Vec::new(),
                    receiver: None,
                    params: vec![&element],
                    vararg: None,
                    type_parameters: Vec::new(),
                    expect: false,
                    placement: Placement::Ordinary,
                },
            )
            .ok()?,
        ),
        SharedBoxMember::Getter => {
            CallTarget::Accessor(accessor_signature(container, &property, Accessor::Getter).ok()?)
        }
        SharedBoxMember::Setter => CallTarget::Accessor(
            accessor_signature(container, &property, Accessor::Setter { value: &element }).ok()?,
        ),
    })
}
