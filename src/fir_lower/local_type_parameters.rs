//! The type parameters of a lifted local function, as kotlinc's `LocalDeclarationsLowering` gives
//! them.
//!
//! A local function lifted to a static method no longer sees the type parameters of the
//! declarations around it, so the lifted method declares a copy of each one its closure mentions,
//! ahead of its own. kotlinc's `ClosureAnnotator` collects that closure: it sees the function's
//! receiver, parameter and return types, then its own type parameters' bounds, then every type its
//! body mentions, and it includes the closure of each local function the body declares or calls
//! and of each lambda it contains. Capturing a type parameter also sees its bound.
//!
//! A lambda literal's method is signed without generics, so only a local function and an
//! anonymous function are given the copies.

use super::BodyLowering;
use crate::fir::{
    BodyLocalCallableDeclarationId, FirBody, FirExprId, FirExprKind, FirStatementId,
    FirStatementKind,
};
use crate::ir::{IrGenericSig, IrTypeParameter};
use crate::types::Ty;
use std::collections::HashMap;

/// The type parameters `body` captures, each as the type that names it, in the order its closure
/// first sees them. `published` gives the closure of a local function declared outside `body`.
pub(super) fn captured_type_parameters(
    body: &FirBody,
    published: &dyn Fn(BodyLocalCallableDeclarationId) -> Option<Box<[Ty]>>,
) -> Box<[Ty]> {
    collect_closure(body, published, &mut HashMap::new()).into_boxed_slice()
}

fn collect_closure(
    body: &FirBody,
    published: &dyn Fn(BodyLocalCallableDeclarationId) -> Option<Box<[Ty]>>,
    declared: &mut HashMap<BodyLocalCallableDeclarationId, Vec<Ty>>,
) -> Vec<Ty> {
    let mut closure = Closure {
        own: body
            .type_parameters()
            .iter()
            .map(|parameter| &*parameter.semantic_name)
            .collect(),
        captured: Vec::new(),
    };
    for ty in body
        .receiver_type()
        .into_iter()
        .chain(body.parameters().iter().map(|parameter| parameter.ty))
        .chain(body.result_type())
        .chain(
            body.type_parameters()
                .iter()
                .flat_map(|parameter| parameter.bounds.iter().map(|bound| bound.ty)),
        )
    {
        closure.see(ty.get());
    }
    let statements = (0..body.statement_count())
        .filter_map(|raw| body.statement(FirStatementId::from_raw(raw as u32)))
        .collect::<Vec<_>>();
    // A local function is declared before the calls that include its closure.
    for statement in &statements {
        if let FirStatementKind::LocalFunction {
            declaration, body, ..
        } = &statement.kind
        {
            let nested = collect_closure(body, published, declared);
            declared.insert(*declaration, nested);
        }
    }
    for raw in 0..body.expression_count() {
        let Some(expression) = body.expr(FirExprId::from_raw(raw as u32)) else {
            continue;
        };
        closure.see(expression.ty.get());
        match &expression.kind {
            FirExprKind::LocalCall { target, .. }
            | FirExprKind::LocalCallableReference { target, .. } => {
                let callee = target.declaration.and_then(|callee| {
                    declared
                        .get(&callee)
                        .map(|captured| captured.clone().into_boxed_slice())
                        .or_else(|| published(callee))
                });
                for ty in callee.iter().flat_map(|captured| captured.iter()) {
                    closure.see(*ty);
                }
            }
            FirExprKind::Lambda { body, .. } => {
                for ty in collect_closure(body, published, declared) {
                    closure.see(ty);
                }
            }
            _ => {}
        }
    }
    for statement in &statements {
        match &statement.kind {
            FirStatementKind::Local { ty, .. } => closure.see(ty.get()),
            FirStatementKind::LocalFunction { declaration, .. } => {
                for &ty in &declared[declaration] {
                    closure.see(ty);
                }
            }
            _ => {}
        }
    }
    closure.captured
}

struct Closure<'a> {
    own: Vec<&'a str>,
    captured: Vec<Ty>,
}

impl Closure<'_> {
    fn see(&mut self, ty: Ty) {
        match ty {
            Ty::TyParam(name, bound) => {
                if self.own.contains(&name)
                    || self
                        .captured
                        .iter()
                        .any(|captured| matches!(captured, Ty::TyParam(seen, _) if *seen == name))
                {
                    return;
                }
                self.captured.push(ty);
                self.see(*bound);
            }
            Ty::Obj(_, arguments) => arguments.iter().for_each(|&argument| self.see(argument)),
            Ty::Fun(signature) => {
                signature
                    .params
                    .iter()
                    .for_each(|&parameter| self.see(parameter));
                self.see(signature.ret);
            }
            Ty::DefinitelyNotNull(inner)
            | Ty::Nullable(inner)
            | Ty::PlatformNullable(inner)
            | Ty::InProjection(inner)
            | Ty::OutProjection(inner) => self.see(*inner),
            Ty::Intersection(parts) => parts.iter().for_each(|&part| self.see(part)),
            Ty::StarProjection(_) | Ty::Unit | Ty::Null | Ty::Nothing | Ty::Error | Ty::Pending => {
            }
        }
    }
}

impl BodyLowering<'_> {
    /// The closure of a local function or lambda `body` this body declares.
    pub(super) fn local_captured_type_parameters(&self, body: &FirBody) -> Box<[Ty]> {
        captured_type_parameters(body, &|callee| {
            self.published_local_callables
                .get(&callee)
                .map(|realization| realization.captured_type_parameters.clone())
        })
    }

    /// Sign the lifted `function` with the type parameters it captures, then its own.
    pub(super) fn attach_local_type_parameters(
        &mut self,
        function: crate::ir::FunId,
        body: &FirBody,
        captured: &[Ty],
    ) {
        let mut type_params = captured
            .iter()
            .map(|&ty| self.captured_type_parameter(ty))
            .collect::<Vec<_>>();
        type_params.extend(body.type_parameters().iter().map(|parameter| {
            IrTypeParameter {
                name: parameter.name.to_string(),
                semantic_name: parameter.semantic_name.to_string(),
                bounds: parameter
                    .bounds
                    .iter()
                    .map(|bound| (bound.ty.get(), bound.is_interface))
                    .collect(),
                variance: crate::types::TypeVariance::Invariant,
                reified: parameter.reified,
            }
        }));
        if type_params.is_empty() {
            return;
        }
        let signature = &self.ir.functions[function as usize];
        let generic = IrGenericSig {
            type_params,
            params: signature.params.clone(),
            ret: Some(signature.ret),
            supers: Vec::new(),
        };
        self.ir.signatures.insert(function, generic);
    }

    /// The declaration of a captured type parameter: a module declaration's, or the one an
    /// enclosing local function signs.
    fn captured_type_parameter(&self, ty: Ty) -> IrTypeParameter {
        let Ty::TyParam(semantic_name, _) = ty else {
            unreachable!("a closure captures type parameters only");
        };
        if let Some(parameter) = super::generics::module_type_parameter(self.index, semantic_name) {
            return parameter;
        }
        self.published_local_callables
            .values()
            .filter_map(|realization| self.ir.signatures.get(&realization.function))
            .flat_map(|signature| &signature.type_params)
            .find(|parameter| parameter.semantic_name == semantic_name)
            .cloned()
            .unwrap_or_else(|| {
                panic!("captured type parameter {semantic_name} has a declaration in scope")
            })
    }
}
