//! Decoding a source `contract { … }` description from the declarations its calls select.
//!
//! A description is ordinary Kotlin: `returns(true) implies (x is String)` is a call of a
//! `ContractBuilder` member followed by an infix call of a `SimpleEffect` member. What an effect
//! means is decided by the declaration each call selects, never by its spelling, so a same-named
//! extension or a foreign `EXACTLY_ONCE` is not mistaken for the DSL. [`Description`] keeps the
//! description's syntax only as lookup input; a [`DescriptionBinder`] answers which declaration
//! each call and invocation kind selected, and [`Description::decode`] maps those identities to
//! effects and to kotlinc's "error in contract description" findings.

use super::{Condition, ConditionType, Contract, Effect, InvocationKind, ParamRef, ReturnsValue};
use crate::ast::{BinOp, Expr, ExprId, File, Stmt, TypeRef};

/// A member of the contract DSL, identified by the declaration a call selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DslMember {
    /// `ContractBuilder.returns()`.
    Returns,
    /// `ContractBuilder.returns(value)`.
    ReturnsValue,
    /// `ContractBuilder.returnsNotNull()`.
    ReturnsNotNull,
    /// `ContractBuilder.callsInPlace(lambda, kind)`.
    CallsInPlace,
    /// `SimpleEffect.implies(booleanExpression)`.
    Implies,
}

/// The declaration a description call selected, as the provider classifies a DSL member by it:
/// its exact declaring classifier, name, receiver and context shape, and complete parameter and
/// return types. A same-owner, same-name declaration of another signature is not the DSL.
#[derive(Clone, Copy, Debug)]
pub struct SelectedDslCallable<'a> {
    /// The classifier declaring the selected member.
    pub owner: crate::types::TypeName,
    pub name: &'a str,
    /// The call selected a member dispatched on its receiver, not an extension.
    pub dispatch_member: bool,
    /// The number of leading context parameters the declaration has.
    pub context_parameters: usize,
    /// The value parameter types, context parameters excluded.
    pub params: &'a [crate::types::Ty],
    pub ret: crate::types::Ty,
}

/// What one call of a description selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallBinding {
    Dsl(DslMember),
    /// A declaration outside the DSL, named by its callable id as kotlinc renders it
    /// (`kotlin/toString`, `p/Owner.member`).
    Foreign(String),
    /// Nothing was selected; ordinary call checking owns that diagnostic.
    Unresolved,
}

/// What the invocation-kind argument of a `callsInPlace` selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KindBinding {
    /// An entry of the enum the selected `callsInPlace` declares its kind parameter as.
    Entry(InvocationKind),
    /// Any other value, rendered as kotlinc renders the argument (`R|p/EXACTLY_ONCE|`).
    Foreign(String),
    Unresolved,
}

/// Binds the calls and invocation kinds of a description to the declarations they select.
pub trait DescriptionBinder {
    fn bind_call(&mut self, description: &Description, call: TermId) -> CallBinding;
    /// The invocation kind argument `kind` of the `callsInPlace` call `call` selected.
    fn bind_kind(&mut self, description: &Description, call: TermId, kind: TermId) -> KindBinding;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TermId(u32);

/// One expression of a description. `origin` is the parser expression it was read from; it is
/// meaningful only while that parser unit is active.
#[derive(Clone, Debug)]
pub struct Term {
    pub origin: ExprId,
    pub kind: TermKind,
}

#[derive(Clone, Debug)]
pub enum TermKind {
    /// `name(args)`, `receiver.name(args)`, or infix `receiver name arg`.
    Call {
        receiver: Option<TermId>,
        name: String,
        args: Vec<TermId>,
        infix: bool,
    },
    /// A simple or dot-qualified name: `x`, `this`, `this@f`, `InvocationKind.EXACTLY_ONCE`.
    Name(Vec<String>),
    Bool(bool),
    Null,
    Is {
        operand: TermId,
        ty: TypeRef,
        negated: bool,
    },
    Equality {
        lhs: TermId,
        rhs: TermId,
        negated: bool,
    },
    And(TermId, TermId),
    Or(TermId, TermId),
    Other,
}

/// The body of a `contract { … }` call, read while its parser unit is active.
#[derive(Clone, Debug)]
pub struct Description {
    /// The `contract` call itself.
    pub call: ExprId,
    terms: Vec<Term>,
    /// One entry per statement of the lambda; `None` for a statement that is not an expression.
    statements: Vec<Option<TermId>>,
}

/// The declaration a description belongs to, as the description's references see it.
pub struct DescriptionOwner<'a> {
    pub params: &'a [String],
    pub name: &'a str,
    pub has_receiver: bool,
}

/// The decoded description: the contract when every statement is a valid effect, and kotlinc's
/// findings, each with the expression it is reported at.
pub struct Decoded {
    pub contract: Option<Contract>,
    pub errors: Vec<(ExprId, String)>,
}

impl Description {
    /// Read the description of contract call `call`: the statements of its trailing lambda.
    pub fn read(file: &File, call: ExprId) -> Option<Self> {
        let Expr::Call { args, .. } = file.expr(call) else {
            return None;
        };
        let Expr::Lambda { body, .. } = file.expr(*args.last()?) else {
            return None;
        };
        let Expr::Block { stmts, trailing } = file.expr(*body) else {
            return None;
        };
        let mut description = Self {
            call,
            terms: Vec::new(),
            statements: Vec::new(),
        };
        for statement in stmts {
            let term = match file.stmt(*statement) {
                Stmt::Expr(expression) => Some(description.read_term(file, *expression)),
                _ => None,
            };
            description.statements.push(term);
        }
        if let Some(expression) = trailing {
            let term = description.read_term(file, *expression);
            description.statements.push(Some(term));
        }
        Some(description)
    }

    pub fn term(&self, id: TermId) -> &Term {
        &self.terms[id.0 as usize]
    }

    fn push(&mut self, origin: ExprId, kind: TermKind) -> TermId {
        self.terms.push(Term { origin, kind });
        TermId(self.terms.len() as u32 - 1)
    }

    fn read_term(&mut self, file: &File, e: ExprId) -> TermId {
        let kind = match file.expr(e) {
            Expr::Call { callee, args } => match file.expr(*callee) {
                Expr::Name(name) => TermKind::Call {
                    receiver: None,
                    name: name.clone(),
                    args: args.iter().map(|arg| self.read_term(file, *arg)).collect(),
                    infix: false,
                },
                Expr::Member { receiver, name } => TermKind::Call {
                    receiver: Some(self.read_term(file, *receiver)),
                    name: name.clone(),
                    args: args.iter().map(|arg| self.read_term(file, *arg)).collect(),
                    infix: file.infix_calls.contains(&e.0),
                },
                _ => TermKind::Other,
            },
            Expr::Name(_) | Expr::Member { .. } => {
                qualified_name(file, e).map_or(TermKind::Other, TermKind::Name)
            }
            Expr::BoolLit(value) => TermKind::Bool(*value),
            Expr::NullLit => TermKind::Null,
            Expr::Is {
                operand,
                ty,
                negated,
            } => TermKind::Is {
                operand: self.read_term(file, *operand),
                ty: ty.clone(),
                negated: *negated,
            },
            Expr::Binary { op, lhs, rhs, .. } => {
                let lhs = self.read_term(file, *lhs);
                let rhs = self.read_term(file, *rhs);
                match op {
                    BinOp::And => TermKind::And(lhs, rhs),
                    BinOp::Or => TermKind::Or(lhs, rhs),
                    BinOp::Eq => TermKind::Equality {
                        lhs,
                        rhs,
                        negated: false,
                    },
                    BinOp::Ne => TermKind::Equality {
                        lhs,
                        rhs,
                        negated: true,
                    },
                    _ => TermKind::Other,
                }
            }
            _ => TermKind::Other,
        };
        self.push(e, kind)
    }

    /// Decode the description against `owner`, asking `binder` what each call selected.
    pub fn decode(
        &self,
        owner: &DescriptionOwner<'_>,
        binder: &mut dyn DescriptionBinder,
    ) -> Decoded {
        let mut effects = Vec::new();
        let mut errors = Vec::new();
        let mut valid = true;
        let mut in_place = Vec::new();
        for statement in &self.statements {
            let Some(statement) = *statement else {
                valid = false;
                continue;
            };
            let mut decoder = Decoder {
                description: self,
                owner,
                binder: &mut *binder,
                statement: self.term(statement).origin,
            };
            match decoder.effect(statement) {
                Ok(effect) => {
                    if let Effect::CallsInPlace { param, .. } = effect {
                        if in_place.contains(&param) {
                            errors.push((
                                self.call,
                                "A value parameter may not be annotated with callsInPlace twice."
                                    .to_string(),
                            ));
                        }
                        in_place.push(param);
                    }
                    effects.push(effect);
                }
                Err(error) => {
                    valid = false;
                    errors.extend(error);
                }
            }
        }
        Decoded {
            contract: valid.then_some(Contract { effects }),
            errors,
        }
    }
}

/// `a`, `a.b.c`: the segments of a name whose every receiver is itself a name.
fn qualified_name(file: &File, e: ExprId) -> Option<Vec<String>> {
    match file.expr(e) {
        Expr::Name(name) => Some(vec![name.clone()]),
        Expr::Member { receiver, name } => {
            let mut segments = qualified_name(file, *receiver)?;
            segments.push(name.clone());
            Some(segments)
        }
        _ => None,
    }
}

/// A finding at an expression, or `None` for a shape kotlinc rejects through another check.
type Finding = Option<(ExprId, String)>;

struct Decoder<'a, 'b> {
    description: &'a Description,
    owner: &'a DescriptionOwner<'a>,
    binder: &'b mut dyn DescriptionBinder,
    /// The effect being decoded; a reference finding is reported at it.
    statement: ExprId,
}

impl<'a> Decoder<'a, '_> {
    fn term(&self, id: TermId) -> &'a Term {
        self.description.term(id)
    }

    fn effect(&mut self, id: TermId) -> Result<Effect, Finding> {
        let TermKind::Call { receiver, args, .. } = &self.term(id).kind else {
            return Err(None);
        };
        match self.dsl_member(id, id)? {
            DslMember::Implies => {
                let (Some(receiver), [condition]) = (receiver, args.as_slice()) else {
                    return Err(None);
                };
                let (receiver, condition) = (*receiver, *condition);
                let returns = self.simple_returns(receiver, id)?;
                let conclusion = self.condition(condition)?;
                Ok(Effect::ConditionalReturns {
                    returns,
                    conclusion,
                })
            }
            DslMember::CallsInPlace => self.calls_in_place(id),
            DslMember::Returns | DslMember::ReturnsValue | DslMember::ReturnsNotNull => {
                self.simple_returns(id, id).map(Effect::Returns)
            }
        }
    }

    /// The DSL member call `id` selected; a foreign selection is reported at `statement`.
    fn dsl_member(&mut self, id: TermId, statement: TermId) -> Result<DslMember, Finding> {
        match self.binder.bind_call(self.description, id) {
            CallBinding::Dsl(member) => Ok(member),
            CallBinding::Foreign(callable) => Err(Some((
                self.term(statement).origin,
                format!("'{callable}' is not part of the contracts DSL."),
            ))),
            CallBinding::Unresolved => Err(None),
        }
    }

    fn simple_returns(&mut self, id: TermId, statement: TermId) -> Result<ReturnsValue, Finding> {
        let TermKind::Call { args, .. } = &self.term(id).kind else {
            return Err(None);
        };
        match (self.dsl_member(id, statement)?, args.as_slice()) {
            (DslMember::Returns, []) => Ok(ReturnsValue::Any),
            (DslMember::ReturnsNotNull, []) => Ok(ReturnsValue::NotNull),
            (DslMember::ReturnsValue, [value]) => match self.term(*value).kind {
                TermKind::Bool(value) => Ok(ReturnsValue::Bool(value)),
                TermKind::Null => Ok(ReturnsValue::Null),
                _ => Err(None),
            },
            _ => Err(None),
        }
    }

    fn calls_in_place(&mut self, id: TermId) -> Result<Effect, Finding> {
        let TermKind::Call { args, .. } = &self.term(id).kind else {
            return Err(None);
        };
        let Some(&lambda) = args.first() else {
            return Err(None);
        };
        let param = match &self.term(lambda).kind {
            TermKind::Name(segments) if segments.len() == 1 => self
                .param_ref(&segments[0])
                .ok_or_else(|| Some((self.statement, reference_error(&segments[0]))))?,
            _ => {
                return Err(Some((
                    self.statement,
                    "element is not a parameter or receiver reference.".to_string(),
                )))
            }
        };
        let kind = match args.get(1) {
            None => InvocationKind::Unknown,
            Some(&kind) => match self.binder.bind_kind(self.description, id, kind) {
                KindBinding::Entry(kind) => kind,
                KindBinding::Foreign(rendered) => {
                    return Err(Some((
                        self.term(id).origin,
                        format!("'{rendered}' is not a valid invocation kind."),
                    )))
                }
                KindBinding::Unresolved => return Err(None),
            },
        };
        Ok(Effect::CallsInPlace { param, kind })
    }

    fn condition(&mut self, id: TermId) -> Result<Condition, Finding> {
        match &self.term(id).kind {
            &TermKind::And(lhs, rhs) => Ok(Condition::And(
                Box::new(self.condition(lhs)?),
                Box::new(self.condition(rhs)?),
            )),
            &TermKind::Or(lhs, rhs) => Ok(Condition::Or(
                Box::new(self.condition(lhs)?),
                Box::new(self.condition(rhs)?),
            )),
            &TermKind::Equality { lhs, rhs, negated } => {
                let (value, null) = if matches!(self.term(rhs).kind, TermKind::Null) {
                    (lhs, rhs)
                } else {
                    (rhs, lhs)
                };
                if !matches!(self.term(null).kind, TermKind::Null) {
                    return Err(None);
                }
                Ok(Condition::IsNull {
                    param: self.reference(value)?,
                    negated,
                })
            }
            TermKind::Is {
                operand,
                ty,
                negated,
            } => Ok(Condition::IsType {
                param: self.reference(*operand)?,
                ty: ConditionType::Source(ty.clone()),
                negated: *negated,
            }),
            TermKind::Name(_) => Ok(Condition::BoolParam {
                param: self.reference(id)?,
                negated: false,
            }),
            TermKind::Bool(value) => Ok(Condition::Const(*value)),
            _ => Err(None),
        }
    }

    /// The parameter or receiver a condition operand names.
    fn reference(&self, id: TermId) -> Result<ParamRef, Finding> {
        let TermKind::Name(segments) = &self.term(id).kind else {
            return Err(None);
        };
        let [name] = segments.as_slice() else {
            return Err(None);
        };
        self.param_ref(name)
            .ok_or_else(|| Some((self.statement, reference_error(name))))
    }

    fn param_ref(&self, name: &str) -> Option<ParamRef> {
        if self.owner.has_receiver
            && (name == "this" || name.strip_prefix("this@") == Some(self.owner.name))
        {
            return Some(ParamRef::Receiver);
        }
        self.owner
            .params
            .iter()
            .position(|param| param == name)
            .map(ParamRef::Param)
    }
}

fn reference_error(name: &str) -> String {
    if name == "this" || name.starts_with("this@") {
        "'this' can only be a qualified reference to the extension receiver of contract owner.."
            .to_string()
    } else {
        format!("'{name}' is not a value parameter.")
    }
}

/// Whether `callable`, declared in `package`, is the contract-declaration intrinsic itself:
/// `kotlin.contracts.contract(builder: ContractBuilder.() -> Unit): Unit`, by its declaring
/// package, name, and complete signature. An unrelated library callable never acquires intrinsic
/// behavior because one component happens to match. Each provider supplies the package, since only
/// it knows which physical owner realizes a top-level declaration.
pub fn is_contract_intrinsic(
    callable: &crate::libraries::LibraryCallable,
    package: crate::types::TypeName,
) -> bool {
    use crate::types::Ty;
    let builder = Ty::obj("kotlin/contracts/ContractBuilder");
    let takes_builder = match callable.params.as_slice() {
        [Ty::Fun(function)] => {
            function.has_receiver
                && function.context_count == 0
                && !function.suspend
                && function.params == [builder]
                && function.ret == Ty::Unit
        }
        _ => false,
    };
    callable.name == "contract"
        && callable.ret == Ty::Unit
        && package.matches("kotlin/contracts")
        && takes_builder
}

/// The contract-DSL member the selected declaration `callable` is, by its complete signature.
pub fn dsl_member(callable: &SelectedDslCallable<'_>) -> Option<DslMember> {
    use crate::types::Ty;
    if !callable.dispatch_member || callable.context_parameters != 0 {
        return None;
    }
    let contracts = |name: &str| Ty::obj(&format!("kotlin/contracts/{name}"));
    let member = if callable.owner.matches("kotlin/contracts/ContractBuilder") {
        match (callable.name, callable.params) {
            ("returns", []) => (DslMember::Returns, contracts("Returns")),
            ("returns", [value]) if *value == Ty::nullable(Ty::obj("kotlin/Any")) => {
                (DslMember::ReturnsValue, contracts("Returns"))
            }
            ("returnsNotNull", []) => (DslMember::ReturnsNotNull, contracts("ReturnsNotNull")),
            // `fun <R> callsInPlace(lambda: Function<R>, kind: InvocationKind)`: the lambda's
            // `R` is the member's own type parameter, which selection may have specialized.
            ("callsInPlace", [Ty::Obj(lambda, [_]), kind])
                if lambda.matches("kotlin/Function") && *kind == contracts("InvocationKind") =>
            {
                (DslMember::CallsInPlace, contracts("CallsInPlace"))
            }
            _ => return None,
        }
    } else if callable.owner.matches("kotlin/contracts/SimpleEffect") {
        match (callable.name, callable.params) {
            ("implies", [Ty::Boolean]) => (DslMember::Implies, contracts("ConditionalEffect")),
            _ => return None,
        }
    } else {
        return None;
    };
    (callable.ret == member.1).then_some(member.0)
}
