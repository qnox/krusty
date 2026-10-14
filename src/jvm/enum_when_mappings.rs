//! kotlinc's `EnumWhenLowering`, as the JVM's `MappedEnumWhenLowering` specializes it.
//!
//! A subject `when` over an enum compares the subject's switch key, not the entry: the block that
//! holds the subject gains `val ordinal = <mapping>[subject.ordinal()]` (`-1` for a `null`
//! subject), and each `subject == Entry` becomes `ordinal == <key of Entry>`. The keys are not the
//! entries' ordinals but positions in a table the emitting class's synthetic `$WhenMappings` fills
//! at run time, so a dependency that adds entries does not change them. Which class emits the code,
//! and therefore owns the table, is a JVM placement fact, so the table and its keys are realized at
//! emission; this pass records which entries each `when` claims, in kotlinc's visiting order.

use crate::backend::coroutines::max_value_index;
use crate::fx_hash::HashMap;
use crate::ir::{EnumSwitchRegistration, ExprId, IrBinOp, IrConst, IrExpr, IrFile};
use crate::types::{Ty, TypeName};

pub(crate) fn lower(ir: &mut IrFile) {
    let mut lowering = EnumWhenLowering {
        claims: HashMap::default(),
        next_value: max_value_index(ir) + 1,
    };
    for expression in 0..ir.exprs.len() as ExprId {
        lowering.visit(ir, expression);
    }
}

struct EnumWhenLowering {
    /// The table claims each visited expression makes, in visiting order.
    claims: HashMap<ExprId, Vec<EnumSwitchRegistration>>,
    next_value: u32,
}

/// A subject `when` this pass rewrites: the block's subject temporary and the `when` reading it.
struct SubjectWhen {
    block: ExprId,
    subject: u32,
    classifier: TypeName,
    nullable: bool,
    when: ExprId,
}

/// The switch key of the subject's ordinal, created by the first comparison that needs it.
struct OrdinalProvider {
    value: Option<u32>,
    claims: Vec<EnumSwitchRegistration>,
}

impl EnumWhenLowering {
    /// kotlinc's `visitBlock` transforms the children first, so a `when` nested anywhere in a
    /// subject `when` claims its table entries before the `when` holding it.
    fn visit(&mut self, ir: &mut IrFile, expression: ExprId) -> Vec<EnumSwitchRegistration> {
        if let Some(claims) = self.claims.get(&expression) {
            return claims.clone();
        }
        // A shared subtree is visited once; a cycle cannot reach this entry twice.
        self.claims.insert(expression, Vec::new());
        let mut children = Vec::new();
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
        let mut claims = Vec::new();
        for child in children {
            claims.extend(self.visit(ir, child));
        }
        if let Some(subject_when) = subject_when(ir, expression) {
            claims.extend(self.lower_subject_when(ir, subject_when, &claims));
        }
        self.claims.insert(expression, claims.clone());
        claims
    }

    fn lower_subject_when(
        &mut self,
        ir: &mut IrFile,
        subject_when: SubjectWhen,
        nested: &[EnumSwitchRegistration],
    ) -> Vec<EnumSwitchRegistration> {
        if !possible_to_generate_jump_table(ir, &subject_when) {
            return Vec::new();
        }
        let mut provider = OrdinalProvider {
            value: None,
            claims: Vec::new(),
        };
        self.transform_branches(ir, &subject_when, subject_when.when, &mut provider);
        let Some(ordinal) = provider.value else {
            return Vec::new();
        };
        let mut registrations = nested.to_vec();
        registrations.extend(provider.claims.iter().cloned());
        let read = ir.add_expr(IrExpr::GetValue(subject_when.subject));
        let mapped = ir.add_expr(IrExpr::EnumSwitchOrdinal {
            classifier: subject_when.classifier,
            subject: read,
            registrations: registrations.into_boxed_slice(),
        });
        let init = if subject_when.nullable {
            // `irIfNull(subject, -1, <mapping>[subject.ordinal()])`.
            let read = ir.add_expr(IrExpr::GetValue(subject_when.subject));
            let null = ir.add_expr(IrExpr::Const(IrConst::Null));
            let is_null = ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: read,
                rhs: null,
            });
            let absent = ir.add_expr(IrExpr::Const(IrConst::Int(-1)));
            let when = ir.add_expr(IrExpr::When {
                branches: vec![(Some(is_null), absent), (None, mapped)],
            });
            ir.whens.exhaustive.insert(when, Ty::Int);
            when
        } else {
            mapped
        };
        let declaration = ir.add_expr(IrExpr::Variable {
            index: ordinal,
            ty: Ty::Int,
            init: Some(init),
            named: false,
        });
        let IrExpr::Block { stmts, .. } = &mut ir.exprs[subject_when.block as usize] else {
            unreachable!("a subject when is a block");
        };
        stmts.insert(1, declaration);
        provider.claims
    }

    fn transform_branches(
        &mut self,
        ir: &mut IrFile,
        subject_when: &SubjectWhen,
        when: ExprId,
        provider: &mut OrdinalProvider,
    ) {
        let IrExpr::When { branches } = ir.expr(when) else {
            return;
        };
        for (condition, result) in branches.clone() {
            if let Some(condition) = condition {
                self.transform_subexpression(ir, subject_when, condition, provider);
            }
            self.transform_subexpression(ir, subject_when, result, provider);
        }
    }

    /// A single entry clause is a comparison; several (`A, B ->`) are an `||` chain, which is a
    /// `when` whose branches are transformed alike.
    fn transform_subexpression(
        &mut self,
        ir: &mut IrFile,
        subject_when: &SubjectWhen,
        expression: ExprId,
        provider: &mut OrdinalProvider,
    ) {
        match ir.expr(expression) {
            IrExpr::When { .. } => self.transform_branches(ir, subject_when, expression, provider),
            &IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs,
                rhs,
            } => self.transform_enum_equals(ir, subject_when, lhs, rhs, provider),
            _ => {}
        }
    }

    fn transform_enum_equals(
        &mut self,
        ir: &mut IrFile,
        subject_when: &SubjectWhen,
        lhs: ExprId,
        rhs: ExprId,
        provider: &mut OrdinalProvider,
    ) {
        let Some(other) = other_operand(ir, subject_when.subject, lhs, rhs) else {
            return;
        };
        let key = match ir.expr(other) {
            IrExpr::EnumEntry { classifier, name } if *classifier == subject_when.classifier => {
                let name = name.clone();
                provider.claims.push(EnumSwitchRegistration {
                    classifier: subject_when.classifier,
                    entry: Some(name.clone()),
                });
                IrExpr::EnumSwitchKey {
                    classifier: subject_when.classifier,
                    name,
                }
            }
            IrExpr::Const(IrConst::Null) => IrExpr::Const(IrConst::Int(-1)),
            _ => return,
        };
        let ordinal = *provider.value.get_or_insert_with(|| {
            let value = self.next_value;
            self.next_value += 1;
            value
        });
        provider.claims.push(EnumSwitchRegistration {
            classifier: subject_when.classifier,
            entry: None,
        });
        // The comparison keeps its operands' positions: the ordinal is read where the left operand
        // was, and the key stands where the right one was.
        ir.exprs[lhs as usize] = IrExpr::GetValue(ordinal);
        ir.exprs[rhs as usize] = key;
        ir.logical_types.remove(&lhs);
        ir.logical_types.remove(&rhs);
    }
}

/// `Block { val subject = …; when { … } }` with an enum subject, as common lowering builds a
/// subject `when`. The subject is an enum when some branch compares it with one of its entries.
fn subject_when(ir: &IrFile, block: ExprId) -> Option<SubjectWhen> {
    let IrExpr::Block {
        stmts,
        value: Some(when),
    } = ir.expr(block)
    else {
        return None;
    };
    let [declaration] = stmts.as_slice() else {
        return None;
    };
    let IrExpr::Variable {
        index: subject, ty, ..
    } = ir.expr(*declaration)
    else {
        return None;
    };
    if !matches!(ir.expr(*when), IrExpr::When { .. }) {
        return None;
    }
    let (classifier, nullable) = match *ty {
        Ty::Obj(classifier, _) => (classifier, false),
        Ty::Nullable(Ty::Obj(classifier, _)) => (*classifier, true),
        _ => return None,
    };
    let candidate = SubjectWhen {
        block,
        subject: *subject,
        classifier,
        nullable,
        when: *when,
    };
    compares_an_entry(ir, &candidate, *when).then_some(candidate)
}

fn compares_an_entry(ir: &IrFile, subject_when: &SubjectWhen, expression: ExprId) -> bool {
    match ir.expr(expression) {
        IrExpr::When { branches } => branches.iter().any(|(condition, result)| {
            condition.is_some_and(|condition| compares_an_entry(ir, subject_when, condition))
                || matches!(ir.expr(*result), IrExpr::When { .. })
                    && compares_an_entry(ir, subject_when, *result)
        }),
        &IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs,
            rhs,
        } => other_operand(ir, subject_when.subject, lhs, rhs).is_some_and(|other| {
            matches!(ir.expr(other), IrExpr::EnumEntry { classifier, .. } if *classifier == subject_when.classifier)
        }),
        _ => false,
    }
}

/// kotlinc's `possibleToGenerateJumpTable`: no branch tests the subject with another operation,
/// or against a value that a call computes.
fn possible_to_generate_jump_table(ir: &IrFile, subject_when: &SubjectWhen) -> bool {
    let IrExpr::When { branches } = ir.expr(subject_when.when) else {
        return false;
    };
    branches.iter().all(|(condition, _)| {
        let Some(condition) = condition else {
            return true;
        };
        match ir.expr(*condition) {
            &IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs,
                rhs,
            } => other_operand(ir, subject_when.subject, lhs, rhs).is_none_or(|other| {
                matches!(
                    ir.expr(other),
                    IrExpr::EnumEntry { .. }
                        | IrExpr::Const(_)
                        | IrExpr::GetValue(_)
                        | IrExpr::GetStatic(_)
                        | IrExpr::GetField { .. }
                )
            }),
            // An `||` chain of clauses, a guard, and a type test are not calls.
            IrExpr::When { .. } | IrExpr::TypeOp { .. } | IrExpr::GetValue(_) => true,
            _ => false,
        }
    })
}

/// The operand compared with the subject, when one side reads the subject.
fn other_operand(ir: &IrFile, subject: u32, lhs: ExprId, rhs: ExprId) -> Option<ExprId> {
    let reads_subject =
        |operand: ExprId| matches!(ir.expr(operand), IrExpr::GetValue(value) if *value == subject);
    if reads_subject(lhs) {
        Some(rhs)
    } else if reads_subject(rhs) {
        Some(lhs)
    } else {
        None
    }
}
