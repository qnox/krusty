//! Kotlin contract IR, shared by source decoding (the `contract { … }` DSL block), classfile
//! metadata decoding (`Function.contract`, proto field 32), the checker's call-site
//! application, and metadata emission. One representation for all four so a contract means the
//! same thing no matter where it came from.

use crate::ast::{Expr, ExprId, File, Stmt, TypeRef};

/// A function's declared contract: the effects from its `contract { … }` block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contract {
    pub effects: Vec<Effect>,
}

impl Contract {
    /// A copy with every `ConditionType::Source` mapped through `resolve` to a semantic type
    /// (metadata emission resolves source references once, against the declaring module).
    /// References `resolve` declines stay `Source` — consumers skip or degrade them.
    pub fn with_resolved_types(
        &self,
        resolve: &mut dyn FnMut(&TypeRef) -> Option<crate::types::Ty>,
    ) -> Contract {
        fn map(
            c: &Condition,
            resolve: &mut dyn FnMut(&TypeRef) -> Option<crate::types::Ty>,
        ) -> Condition {
            match c {
                Condition::IsType { param, ty, negated } => Condition::IsType {
                    param: *param,
                    ty: match ty {
                        ConditionType::Source(r) => resolve(r)
                            .map(ConditionType::Metadata)
                            .unwrap_or_else(|| ConditionType::Source(r.clone())),
                        m => m.clone(),
                    },
                    negated: *negated,
                },
                Condition::And(l, r) => {
                    Condition::And(Box::new(map(l, resolve)), Box::new(map(r, resolve)))
                }
                Condition::Or(l, r) => {
                    Condition::Or(Box::new(map(l, resolve)), Box::new(map(r, resolve)))
                }
                c => c.clone(),
            }
        }
        Contract {
            effects: self
                .effects
                .iter()
                .map(|e| match e {
                    Effect::ConditionalReturns {
                        returns,
                        conclusion,
                    } => Effect::ConditionalReturns {
                        returns: *returns,
                        conclusion: map(conclusion, resolve),
                    },
                    e => e.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// `returns()`, `returnsNotNull()`, `returns(true|false|null)` — an unconditional effect.
    Returns(ReturnsValue),
    /// `returns(X) implies <condition>` — when the call evaluates to `X`, the condition holds.
    ConditionalReturns {
        returns: ReturnsValue,
        conclusion: Condition,
    },
    /// `callsInPlace(lambda, KIND)` — invocation guarantee for a function parameter.
    CallsInPlace {
        param: ParamRef,
        kind: InvocationKind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnsValue {
    /// `returns()` — the call returns normally (any value).
    Any,
    /// `returnsNotNull()`.
    NotNull,
    /// `returns(null)`.
    Null,
    /// `returns(true)` / `returns(false)`.
    Bool(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationKind {
    ExactlyOnce,
    AtLeastOnce,
    AtMostOnce,
    Unknown,
}

impl InvocationKind {
    /// `Effect.kind` wire number (kotlinx-metadata's InvocationKind order). Shared by metadata
    /// decode and emit so the mapping lives in exactly one place.
    pub fn from_wire(kind: u64) -> InvocationKind {
        match kind {
            0 => InvocationKind::AtMostOnce,
            1 => InvocationKind::ExactlyOnce,
            2 => InvocationKind::AtLeastOnce,
            _ => InvocationKind::Unknown,
        }
    }

    /// The inverse of [`InvocationKind::from_wire`]; `None` for `Unknown`, which emit OMITS
    /// (a kindless CALLS effect reads back as `Unknown`).
    pub fn to_wire(self) -> Option<u64> {
        match self {
            InvocationKind::AtMostOnce => Some(0),
            InvocationKind::ExactlyOnce => Some(1),
            InvocationKind::AtLeastOnce => Some(2),
            InvocationKind::Unknown => None,
        }
    }
}

/// Which function input a condition talks about. Mirrors the proto
/// `Expression.value_parameter_reference` convention: the extension receiver vs a 0-based
/// value-parameter index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamRef {
    Receiver,
    Param(usize),
}

impl ParamRef {
    /// The `value_parameter_reference` wire convention (Expression field 2): 0 is the extension
    /// receiver, n is the 1-based value-parameter index. Shared by metadata decode and emit so
    /// the off-by-one lives in exactly one place.
    pub fn from_wire(vpr: u64) -> ParamRef {
        if vpr == 0 {
            ParamRef::Receiver
        } else {
            ParamRef::Param((vpr - 1) as usize)
        }
    }

    /// The inverse of [`ParamRef::from_wire`].
    pub fn to_wire(self) -> u64 {
        match self {
            ParamRef::Receiver => 0,
            ParamRef::Param(i) => (i + 1) as u64,
        }
    }
}

/// A boolean condition over the function's inputs (the right side of `implies`).
#[derive(Clone, Debug, PartialEq)]
pub enum Condition {
    /// `x == null` (`negated = false`) / `x != null` (`negated = true`).
    IsNull {
        param: ParamRef,
        negated: bool,
    },
    /// `x is T` / `x !is T`.
    IsType {
        param: ParamRef,
        ty: ConditionType,
        negated: bool,
    },
    /// The boolean argument itself — `returns() implies actual` in `require(actual)`.
    BoolParam(ParamRef),
    Const(bool),
    And(Box<Condition>, Box<Condition>),
    Or(Box<Condition>, Box<Condition>),
}

/// The type in an `is`-conclusion, at the stage decoding produced it. Source contracts carry
/// the unresolved AST reference (the checker resolves it at the call site, against the call's
/// type arguments); metadata contracts carry the decoded semantic type.
#[derive(Clone, Debug)]
pub enum ConditionType {
    Source(TypeRef),
    Metadata(crate::types::Ty),
}

impl PartialEq for ConditionType {
    /// `TypeRef` has no structural equality; two source references are "equal" for test purposes
    /// when they name the same type with the same flags.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ConditionType::Source(a), ConditionType::Source(b)) => {
                a.name == b.name && a.flags == b.flags
            }
            (ConditionType::Metadata(a), ConditionType::Metadata(b)) => a == b,
            _ => false,
        }
    }
}

/// Decode a source `contract { … }` call — ALREADY confirmed to be the `kotlin.contracts`
/// intrinsic by the caller — into a [`Contract`]. `params` are the declaring function's
/// value-parameter names, `fn_name` identifies the labeled receiver `this@<fn_name>`, and
/// `has_receiver` says whether the function is an extension. Anything outside the
/// `ContractBuilder` DSL yields `None` (treated as "no usable contract", never an error).
pub fn decode_source(
    file: &File,
    call: ExprId,
    params: &[String],
    fn_name: &str,
    has_receiver: bool,
) -> Option<Contract> {
    let ctx = SourceCtx {
        file,
        params,
        fn_name,
        has_receiver,
    };
    let Expr::Call { args, .. } = file.expr(call) else {
        return None;
    };
    let lambda = args.iter().next_back()?;
    let Expr::Lambda { body, .. } = file.expr(*lambda) else {
        return None;
    };
    let Expr::Block { stmts, trailing } = file.expr(*body) else {
        return None;
    };
    let mut effects = Vec::new();
    for s in stmts {
        let Stmt::Expr(e) = file.stmt(*s) else {
            return None;
        };
        effects.push(ctx.effect(*e)?);
    }
    if let Some(t) = trailing {
        effects.push(ctx.effect(*t)?);
    }
    Some(Contract { effects })
}

struct SourceCtx<'a> {
    file: &'a File,
    params: &'a [String],
    fn_name: &'a str,
    has_receiver: bool,
}

impl SourceCtx<'_> {
    fn effect(&self, e: ExprId) -> Option<Effect> {
        // `returns*(…) implies <cond>` parses as an infix call: `Call { Member { receiver:
        // <returns-call>, "implies" }, [cond] }`.
        if let Expr::Call { callee, args } = self.file.expr(e) {
            if let Expr::Member { receiver, name } = self.file.expr(*callee) {
                if name == "implies" && args.len() == 1 {
                    let returns = self.simple_returns(*receiver)?;
                    let conclusion = self.condition(args[0])?;
                    return Some(Effect::ConditionalReturns {
                        returns,
                        conclusion,
                    });
                }
            }
        }
        self.simple_effect(e)
    }

    fn simple_effect(&self, e: ExprId) -> Option<Effect> {
        match self.simple_returns(e) {
            Some(r) => Some(Effect::Returns(r)),
            None => self.calls_in_place(e),
        }
    }

    /// `returns()` / `returnsNotNull()` / `returns(true|false|null)`.
    fn simple_returns(&self, e: ExprId) -> Option<ReturnsValue> {
        let Expr::Call { callee, args } = self.file.expr(e) else {
            return None;
        };
        let Expr::Name(n) = self.file.expr(*callee) else {
            return None;
        };
        match (n.as_str(), args.as_slice()) {
            ("returns", []) => Some(ReturnsValue::Any),
            ("returnsNotNull", []) => Some(ReturnsValue::NotNull),
            ("returns", [a]) => match self.file.expr(*a) {
                Expr::BoolLit(b) => Some(ReturnsValue::Bool(*b)),
                Expr::NullLit => Some(ReturnsValue::Null),
                _ => None,
            },
            _ => None,
        }
    }

    /// `callsInPlace(x)` / `callsInPlace(x, InvocationKind.EXACTLY_ONCE)`.
    fn calls_in_place(&self, e: ExprId) -> Option<Effect> {
        let Expr::Call { callee, args } = self.file.expr(e) else {
            return None;
        };
        let Expr::Name(n) = self.file.expr(*callee) else {
            return None;
        };
        if n != "callsInPlace" || args.is_empty() || args.len() > 2 {
            return None;
        }
        let Expr::Name(p) = self.file.expr(args[0]) else {
            return None;
        };
        let param = self.param_ref(p)?;
        let kind = match args.get(1) {
            None => InvocationKind::Unknown,
            Some(k) => {
                // `InvocationKind.EXACTLY_ONCE` (qualified) or a star-imported bare name.
                let kn = match self.file.expr(*k) {
                    Expr::Member { name, .. } => name.as_str(),
                    Expr::Name(n) => n.as_str(),
                    _ => return None,
                };
                match kn {
                    "EXACTLY_ONCE" => InvocationKind::ExactlyOnce,
                    "AT_LEAST_ONCE" => InvocationKind::AtLeastOnce,
                    "AT_MOST_ONCE" => InvocationKind::AtMostOnce,
                    _ => InvocationKind::Unknown,
                }
            }
        };
        Some(Effect::CallsInPlace { param, kind })
    }

    fn condition(&self, e: ExprId) -> Option<Condition> {
        match self.file.expr(e) {
            Expr::Binary { op, lhs, rhs, .. } => {
                use crate::ast::BinOp;
                match op {
                    BinOp::And => Some(Condition::And(
                        Box::new(self.condition(*lhs)?),
                        Box::new(self.condition(*rhs)?),
                    )),
                    BinOp::Or => Some(Condition::Or(
                        Box::new(self.condition(*lhs)?),
                        Box::new(self.condition(*rhs)?),
                    )),
                    BinOp::Eq | BinOp::Ne => {
                        let negated = *op == BinOp::Ne;
                        let (param, null_side) = if matches!(self.file.expr(*rhs), Expr::NullLit) {
                            (lhs, rhs)
                        } else {
                            (rhs, lhs)
                        };
                        if !matches!(self.file.expr(*null_side), Expr::NullLit) {
                            return None;
                        }
                        let Expr::Name(p) = self.file.expr(*param) else {
                            return None;
                        };
                        Some(Condition::IsNull {
                            param: self.param_ref(p)?,
                            negated,
                        })
                    }
                    _ => None,
                }
            }
            Expr::Is {
                operand,
                ty,
                negated,
            } => {
                let Expr::Name(p) = self.file.expr(*operand) else {
                    return None;
                };
                Some(Condition::IsType {
                    param: self.param_ref(p)?,
                    ty: ConditionType::Source(ty.clone()),
                    negated: *negated,
                })
            }
            Expr::Name(p) => Some(Condition::BoolParam(self.param_ref(p)?)),
            Expr::BoolLit(b) => Some(Condition::Const(*b)),
            _ => None,
        }
    }

    fn param_ref(&self, name: &str) -> Option<ParamRef> {
        if self.has_receiver && (name == "this" || name == format!("this@{}", self.fn_name)) {
            return Some(ParamRef::Receiver);
        }
        self.params
            .iter()
            .position(|p| p == name)
            .map(ParamRef::Param)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::lexer::lex;
    use crate::parser::parse;

    /// Parse `src` (a single function) and decode the contract in its body.
    fn contract_of(src: &str, params: &[&str], fn_name: &str, has_receiver: bool) -> Contract {
        let mut diags = DiagSink::new();
        let toks = lex(src, &mut diags);
        let file = parse(src, &toks, &mut diags);
        let call = file
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(i, e)| {
                match e {
                    Expr::Call { callee, .. } => {
                        matches!(file.expr(*callee), Expr::Name(n) if n == "contract")
                    }
                    _ => false,
                }
                .then_some(crate::ast::ExprId(i as u32))
            })
            .expect("contract call in source");
        let params: Vec<String> = params.iter().map(|s| s.to_string()).collect();
        decode_source(&file, call, &params, fn_name, has_receiver).expect("decodable contract")
    }

    #[test]
    fn decodes_returns_implies_bool_param() {
        // The `require(actual)` shape.
        let c = contract_of(
            "fun check(actual: Boolean) {\n\
                 contract { returns() implies actual }\n\
             }",
            &["actual"],
            "check",
            false,
        );
        assert_eq!(
            c.effects,
            vec![Effect::ConditionalReturns {
                returns: ReturnsValue::Any,
                conclusion: Condition::BoolParam(ParamRef::Param(0)),
            }]
        );
    }

    #[test]
    fn decodes_returns_const_implies_labeled_receiver_is() {
        // The `isError` shape: `returns(true) implies (this@f is Foo)`.
        let c = contract_of(
            "class Foo\n\
             fun Bar.f(): Boolean {\n\
                 contract { returns(true) implies (this@f is Foo) }\n\
                 return true\n\
             }",
            &[],
            "f",
            true,
        );
        let [Effect::ConditionalReturns {
            returns,
            conclusion:
                Condition::IsType {
                    param,
                    ty: ConditionType::Source(ty),
                    negated,
                },
        }] = c.effects.as_slice()
        else {
            panic!(
                "expected single returns(true)-implies-is effect, got {:?}",
                c.effects
            );
        };
        assert_eq!(*returns, ReturnsValue::Bool(true));
        assert_eq!(*param, ParamRef::Receiver);
        assert!(!negated);
        assert_eq!(ty.name, "Foo");
    }

    #[test]
    fn decodes_returns_false_implies_not_null_and_compound() {
        // The `isNullOrBlank` shape plus an `&&` compound.
        let c = contract_of(
            "fun String?.f(x: String?): Boolean {\n\
                 contract { returns(false) implies (this != null && x != null) }\n\
                 return false\n\
             }",
            &["x"],
            "f",
            true,
        );
        assert_eq!(
            c.effects,
            vec![Effect::ConditionalReturns {
                returns: ReturnsValue::Bool(false),
                conclusion: Condition::And(
                    Box::new(Condition::IsNull {
                        param: ParamRef::Receiver,
                        negated: true,
                    }),
                    Box::new(Condition::IsNull {
                        param: ParamRef::Param(0),
                        negated: true,
                    }),
                ),
            }]
        );
    }

    #[test]
    fn decodes_calls_in_place_kinds() {
        let c = contract_of(
            "fun runOnce(x: () -> Unit, y: () -> Unit, z: () -> Unit) {\n\
                 contract {\n\
                     callsInPlace(x, InvocationKind.EXACTLY_ONCE)\n\
                     callsInPlace(y, InvocationKind.AT_MOST_ONCE)\n\
                     callsInPlace(z)\n\
                 }\n\
             }",
            &["x", "y", "z"],
            "runOnce",
            false,
        );
        assert_eq!(
            c.effects,
            vec![
                Effect::CallsInPlace {
                    param: ParamRef::Param(0),
                    kind: InvocationKind::ExactlyOnce,
                },
                Effect::CallsInPlace {
                    param: ParamRef::Param(1),
                    kind: InvocationKind::AtMostOnce,
                },
                Effect::CallsInPlace {
                    param: ParamRef::Param(2),
                    kind: InvocationKind::Unknown,
                },
            ]
        );
    }

    #[test]
    fn decodes_returns_not_null_and_null() {
        let c = contract_of(
            "fun f(x: String?): String? {\n\
                 contract {\n\
                     returnsNotNull() implies (x != null)\n\
                     returns(null) implies (x == null)\n\
                 }\n\
                 return x\n\
             }",
            &["x"],
            "f",
            false,
        );
        assert_eq!(
            c.effects,
            vec![
                Effect::ConditionalReturns {
                    returns: ReturnsValue::NotNull,
                    conclusion: Condition::IsNull {
                        param: ParamRef::Param(0),
                        negated: true,
                    },
                },
                Effect::ConditionalReturns {
                    returns: ReturnsValue::Null,
                    conclusion: Condition::IsNull {
                        param: ParamRef::Param(0),
                        negated: false,
                    },
                },
            ]
        );
    }
}
