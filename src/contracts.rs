//! Kotlin contract IR, shared by source decoding (the `contract { … }` DSL block), classfile
//! metadata decoding (`Function.contract`, proto field 32), the checker's call-site
//! application, and metadata emission. One representation for all four so a contract means the
//! same thing no matter where it came from.

use crate::ast::TypeRef;

mod description;

pub use description::{
    CallBinding, Decoded, Description, DescriptionBinder, DescriptionOwner, DslMember, KindBinding,
    SelectedDslCallable, Term, TermId, TermKind,
};

/// A contract whose source type references have all been bound to publishable semantic types.
///
/// Keeping this wrapper at the phase boundary prevents an unresolved [`TypeRef`] from reaching
/// checked FIR, common lowering, metadata, or a backend. Source decoding may temporarily produce a
/// [`Contract`], but only this form may be retained in the stable declaration index.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedContract(std::sync::Arc<Contract>);

impl Eq for ResolvedContract {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnpublishableContract {
    SourceType,
    PendingType,
    ErrorType,
}

impl ResolvedContract {
    pub fn new(contract: Contract) -> Result<Self, UnpublishableContract> {
        fn validate(condition: &Condition) -> Result<(), UnpublishableContract> {
            match condition {
                Condition::IsType { ty, .. } => match ty {
                    ConditionType::Source(_) => Err(UnpublishableContract::SourceType),
                    ConditionType::Metadata(ty) if ty.mentions_pending() => {
                        Err(UnpublishableContract::PendingType)
                    }
                    ConditionType::Metadata(ty) if ty.mentions_error() => {
                        Err(UnpublishableContract::ErrorType)
                    }
                    ConditionType::Metadata(_) => Ok(()),
                },
                Condition::And(left, right) | Condition::Or(left, right) => {
                    validate(left)?;
                    validate(right)
                }
                Condition::IsNull { .. } | Condition::BoolParam(_) | Condition::Const(_) => Ok(()),
            }
        }

        for effect in &contract.effects {
            if let Effect::ConditionalReturns { conclusion, .. } = effect {
                validate(conclusion)?;
            }
        }
        Ok(Self(std::sync::Arc::new(contract)))
    }

    pub fn as_contract(&self) -> &Contract {
        &self.0
    }

    pub fn to_arc(&self) -> std::sync::Arc<Contract> {
        self.0.clone()
    }

    pub fn storage_payload_bytes(&self) -> usize {
        fn condition_bytes(condition: &Condition) -> usize {
            match condition {
                Condition::And(left, right) | Condition::Or(left, right) => {
                    std::mem::size_of::<Condition>()
                        + condition_bytes(left)
                        + condition_bytes(right)
                }
                _ => std::mem::size_of::<Condition>(),
            }
        }

        std::mem::size_of::<Contract>()
            + self
                .0
                .effects
                .iter()
                .map(|effect| {
                    std::mem::size_of::<Effect>()
                        + match effect {
                            Effect::ConditionalReturns { conclusion, .. } => {
                                condition_bytes(conclusion)
                            }
                            Effect::Returns(_) | Effect::CallsInPlace { .. } => 0,
                        }
                })
                .sum::<usize>()
    }
}

/// A function's declared contract: the effects from its `contract { … }` block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contract {
    pub effects: Vec<Effect>,
}

impl Contract {
    /// Map the temporary source type references produced by DSL decoding to semantic types.
    ///
    /// Pass 1 invokes this while the declaration's lexical type scope is live and then requires
    /// [`ResolvedContract::new`] to succeed before publishing the contract. The legacy whole-file
    /// resolver also uses it while that migration path remains; checked FIR, lowering, metadata,
    /// and backends must never receive a surviving `Source` variant.
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
    /// The kind an entry of `kotlin.contracts.InvocationKind` stands for, by the entry's name.
    pub fn of_entry(entry: &str) -> Option<InvocationKind> {
        match entry {
            "AT_MOST_ONCE" => Some(InvocationKind::AtMostOnce),
            "EXACTLY_ONCE" => Some(InvocationKind::ExactlyOnce),
            "AT_LEAST_ONCE" => Some(InvocationKind::AtLeastOnce),
            "UNKNOWN" => Some(InvocationKind::Unknown),
            _ => None,
        }
    }

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
    /// (the reader defaults a missing kind to wire 0, so a kindless effect reads back as
    /// `AtMostOnce` — the kindless form does not round-trip).
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

/// The type in an `is`-conclusion at the decoding boundary. Source DSL decoding temporarily carries
/// an unresolved AST reference only until Pass 1 binds it in the declaring function's lexical type
/// scope. Stable source contracts and metadata contracts both carry the semantic form.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Expr;
    use crate::diag::DiagSink;
    use crate::lexer::lex;
    use crate::parser::parse;

    /// Binds each call by its spelling, standing in for the resolver's selection of the
    /// `kotlin.contracts` declarations a real description's calls select.
    struct SpellingBinder;

    impl DescriptionBinder for SpellingBinder {
        fn bind_call(&mut self, description: &Description, call: TermId) -> CallBinding {
            let TermKind::Call { name, args, .. } = &description.term(call).kind else {
                return CallBinding::Unresolved;
            };
            CallBinding::Dsl(match (name.as_str(), args.len()) {
                ("returns", 0) => DslMember::Returns,
                ("returns", 1) => DslMember::ReturnsValue,
                ("returnsNotNull", 0) => DslMember::ReturnsNotNull,
                ("callsInPlace", _) => DslMember::CallsInPlace,
                ("implies", 1) => DslMember::Implies,
                _ => return CallBinding::Foreign(format!("test/{name}")),
            })
        }

        fn bind_kind(
            &mut self,
            description: &Description,
            _call: TermId,
            kind: TermId,
        ) -> KindBinding {
            let TermKind::Name(segments) = &description.term(kind).kind else {
                return KindBinding::Unresolved;
            };
            segments
                .last()
                .and_then(|entry| InvocationKind::of_entry(entry))
                .map_or(KindBinding::Unresolved, KindBinding::Entry)
        }
    }

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
        let decoded = Description::read(&file, call)
            .expect("contract description")
            .decode(
                &DescriptionOwner {
                    params: &params,
                    name: fn_name,
                    has_receiver,
                },
                &mut SpellingBinder,
            );
        assert!(decoded.errors.is_empty(), "{:?}", decoded.errors);
        decoded.contract.expect("decodable contract")
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
