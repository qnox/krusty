use super::*;
use crate::ir::{Callee, IrFunction};

/// A machine whose label is value 0, whose resumed value is value 1 and whose continuation is
/// value 2. The marker stands as `null`, and a call takes the continuation as one more argument.
struct Slots;

impl MachineStorage for Slots {
    fn read_label(&self, ir: &mut IrFile) -> ExprId {
        ir.add_expr(IrExpr::GetValue(0))
    }

    fn write_label(&self, ir: &mut IrFile, label: ExprId) -> ExprId {
        ir.add_expr(IrExpr::SetValue {
            var: 0,
            value: label,
        })
    }

    fn resumed_value(&self, ir: &mut IrFile) -> ExprId {
        ir.add_expr(IrExpr::GetValue(1))
    }

    fn suspended_marker(&self, ir: &mut IrFile) -> ExprId {
        ir.add_expr(IrExpr::Const(IrConst::Null))
    }

    fn call_with_continuation(
        &self,
        ir: &mut IrFile,
        call: ExprId,
    ) -> Result<ExprId, ReentryDecline> {
        let IrExpr::Call {
            callee,
            dispatch_receiver,
            mut args,
        } = ir.exprs[call as usize].clone()
        else {
            panic!("a direct call");
        };
        args.push(ir.add_expr(IrExpr::GetValue(2)));
        Ok(ir.add_expr(IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        }))
    }
}

/// A file with one suspend function `leaf()`, returning its id.
fn file() -> (IrFile, u32) {
    let mut ir = IrFile::default();
    let leaf = ir.add_fun(IrFunction {
        name: "leaf".to_string(),
        params: Vec::new(),
        ret: Ty::Int,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.suspend_funs.push(leaf);
    (ir, leaf)
}

fn call(ir: &mut IrFile, function: u32) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(function)),
        dispatch_receiver: None,
        args: Vec::new(),
    })
}

fn local(ir: &mut IrFile, index: u32, init: ExprId) -> ExprId {
    ir.add_expr(IrExpr::Variable {
        index,
        ty: Ty::Int,
        init: Some(init),
        named: true,
    })
}

fn split(ir: &mut IrFile, stmts: Vec<ExprId>) -> (ExprId, Result<u32, ReentryDecline>) {
    let body = ir.add_expr(IrExpr::Block { stmts, value: None });
    let suspend_set = ir.suspend_funs.iter().copied().collect::<HashSet<_>>();
    let result = split_at_suspensions(ir, body, &suspend_set, &Slots, 10);
    (body, result)
}

fn stmts(ir: &IrFile, block: ExprId) -> Vec<ExprId> {
    match &ir.exprs[block as usize] {
        IrExpr::Block { stmts, value: None } => stmts.clone(),
        other => panic!("a statement block, not {other:?}"),
    }
}

/// `label == value`, read from the machine's label slot.
fn is_label(ir: &IrFile, expression: ExprId, value: i32) -> bool {
    matches!(ir.exprs[expression as usize],
        IrExpr::PrimitiveBinOp { op: IrBinOp::Eq, lhs, rhs }
            if matches!(ir.exprs[lhs as usize], IrExpr::GetValue(0))
                && matches!(ir.exprs[rhs as usize], IrExpr::Const(IrConst::Int(v)) if v == value))
}

/// The operands of `lhs && rhs` as a `when` that evaluates `rhs` only when `lhs` holds.
fn and_then(ir: &IrFile, expression: ExprId) -> Option<(ExprId, ExprId)> {
    let IrExpr::When { branches } = &ir.exprs[expression as usize] else {
        return None;
    };
    match branches[..] {
        [(Some(lhs), rhs), (None, otherwise)]
            if matches!(
                ir.exprs[otherwise as usize],
                IrExpr::Const(IrConst::Boolean(false))
            ) =>
        {
            Some((lhs, rhs))
        }
        _ => None,
    }
}

/// The operands of `lhs || rhs` as a `when` that evaluates `rhs` only when `lhs` does not hold.
fn or_else(ir: &IrFile, expression: ExprId) -> Option<(ExprId, ExprId)> {
    let IrExpr::When { branches } = &ir.exprs[expression as usize] else {
        return None;
    };
    match branches[..] {
        [(Some(lhs), held), (None, rhs)]
            if matches!(
                ir.exprs[held as usize],
                IrExpr::Const(IrConst::Boolean(true))
            ) =>
        {
            Some((lhs, rhs))
        }
        _ => None,
    }
}

/// The one guard and body of a single-branch `when`.
fn guarded(ir: &IrFile, expression: ExprId) -> (ExprId, ExprId) {
    match &ir.exprs[expression as usize] {
        IrExpr::When { branches } => match branches.as_slice() {
            [(Some(guard), body)] => (*guard, *body),
            other => panic!("one guarded branch, not {other:?}"),
        },
        other => panic!("a guard, not {other:?}"),
    }
}

/// `val a = 1; val b = leaf(); val c = leaf()`: the plain run is guarded by label 0, and each
/// suspension takes the next label.
#[test]
fn each_suspension_takes_the_next_label_and_plain_runs_go_forward_only() {
    let (mut ir, leaf) = file();
    let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let a = local(&mut ir, 3, one);
    let first = call(&mut ir, leaf);
    let b = local(&mut ir, 4, first);
    let second = call(&mut ir, leaf);
    let c = local(&mut ir, 5, second);

    let (body, labels) = split(&mut ir, vec![a, b, c]);

    assert_eq!(labels, Ok(2));
    let top = stmts(&ir, body);
    assert_eq!(top.len(), 3);
    let (guard, run) = guarded(&ir, top[0]);
    assert!(is_label(&ir, guard, 0));
    assert_eq!(stmts(&ir, run), [a]);
    for (index, (statement, label)) in [(top[1], 1), (top[2], 2)].into_iter().enumerate() {
        let [declared, step, complete] = stmts(&ir, statement)[..] else {
            panic!("suspension {label}: declare, step, complete");
        };
        let expected = 10 + index as u32;
        assert!(matches!(ir.exprs[declared as usize],
            IrExpr::Variable { index, init: None, named: false, .. } if index == expected));
        let IrExpr::When { branches } = &ir.exprs[step as usize] else {
            panic!("suspension {label}: a two-way step");
        };
        let [(Some(forward), run), (Some(at), resume)] = branches[..] else {
            panic!("suspension {label}: forward and resume branches");
        };
        assert!(is_label(&ir, forward, 0));
        assert!(is_label(&ir, at, label));
        let [store_label, made, leave, completed_forward] = stmts(&ir, run)[..] else {
            panic!("suspension {label}: store the label, call, leave if suspended, go forward");
        };
        assert!(matches!(ir.exprs[store_label as usize],
            IrExpr::SetValue { var: 0, value } if matches!(ir.exprs[value as usize], IrExpr::Const(IrConst::Int(v)) if v == label)));
        let IrExpr::SetValue { value: made, .. } = ir.exprs[made as usize] else {
            panic!("the call's result is stored");
        };
        assert!(matches!(&ir.exprs[made as usize], IrExpr::Call { args, .. } if args.len() == 1));
        let (_, left) = guarded(&ir, leave);
        assert!(matches!(ir.exprs[left as usize], IrExpr::Return(Some(_))));
        // A call that completed without suspending clears the label, so the statement completes.
        assert!(matches!(ir.exprs[completed_forward as usize],
            IrExpr::SetValue { var: 0, value } if matches!(ir.exprs[value as usize], IrExpr::Const(IrConst::Int(0)))));
        let [cleared, taken] = stmts(&ir, resume)[..] else {
            panic!("suspension {label}: clear the label, take the resumed value");
        };
        assert!(matches!(
            ir.exprs[cleared as usize],
            IrExpr::SetValue { var: 0, .. }
        ));
        assert!(matches!(ir.exprs[taken as usize],
            IrExpr::SetValue { value, .. } if matches!(ir.exprs[value as usize], IrExpr::GetValue(1))));
        let (guard, completed) = guarded(&ir, complete);
        assert!(is_label(&ir, guard, 0));
        assert_eq!(completed, [b, c][index]);
    }
    // Each local now takes the value its suspension completed with.
    assert!(matches!(ir.exprs[first as usize],
        IrExpr::TypeOp { op: IrTypeOp::Cast, arg, .. } if matches!(ir.exprs[arg as usize], IrExpr::GetValue(10))));
    assert!(matches!(ir.exprs[second as usize],
        IrExpr::TypeOp { op: IrTypeOp::Cast, arg, .. } if matches!(ir.exprs[arg as usize], IrExpr::GetValue(11))));
}

/// A value-bearing block used as a statement still evaluates its terminal expression, after its
/// statements, and discards the block value only at the enclosing statement boundary.
#[test]
fn a_statement_block_splits_its_terminal_value_after_its_statements() {
    let (mut ir, leaf) = file();
    let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let before = local(&mut ir, 3, one);
    let terminal = call(&mut ir, leaf);
    let nested = ir.add_expr(IrExpr::Block {
        stmts: vec![before],
        value: Some(terminal),
    });

    let (body, labels) = split(&mut ir, vec![nested]);

    assert_eq!(labels, Ok(1));
    let [rewritten] = stmts(&ir, body)[..] else {
        panic!("one rewritten statement block");
    };
    let IrExpr::Block {
        stmts: nested,
        value: None,
    } = &ir.exprs[rewritten as usize]
    else {
        panic!("the nested block preserves statement position and has no value");
    };
    assert_eq!(nested.len(), 2);
    let (forward, run) = guarded(&ir, nested[0]);
    assert!(is_label(&ir, forward, 0));
    assert_eq!(stmts(&ir, run), [before]);
    let [declared, step, complete] = stmts(&ir, nested[1])[..] else {
        panic!("the terminal expression runs after the block statements");
    };
    assert!(matches!(
        ir.exprs[declared as usize],
        IrExpr::Variable { index: 10, .. }
    ));
    assert!(matches!(ir.exprs[step as usize], IrExpr::When { .. }));
    let (guard, completed_terminal) = guarded(&ir, complete);
    assert!(is_label(&ir, guard, 0));
    assert_eq!(completed_terminal, terminal);
    assert!(matches!(
        ir.exprs[terminal as usize],
        IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            ..
        }
    ));
}

#[test]
fn a_value_block_in_a_statement_keeps_its_value_and_value_position_suspension_declines_untouched() {
    let (mut ir, leaf) = file();
    let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let retained_value = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: Some(one),
    });
    let terminal = call(&mut ir, leaf);
    let after = local(&mut ir, 3, terminal);
    let (_body, labels) = split(&mut ir, vec![retained_value, after]);
    assert_eq!(labels, Ok(1));
    assert!(matches!(ir.exprs[retained_value as usize],
        IrExpr::Block { value: Some(value), .. } if value == one));

    let (mut ir, leaf) = file();
    let value_call = call(&mut ir, leaf);
    let value_block = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: Some(value_call),
    });
    let initializer = ir.add_expr(IrExpr::Variable {
        index: 3,
        ty: Ty::Int,
        init: Some(value_block),
        named: true,
    });
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![initializer],
        value: None,
    });
    let expressions_before = ir
        .exprs
        .iter()
        .map(|expression| format!("{expression:?}"))
        .collect::<Vec<_>>();
    let suspend_set = ir.suspend_funs.iter().copied().collect::<HashSet<_>>();

    assert_eq!(
        split_at_suspensions(&mut ir, body, &suspend_set, &Slots, 10),
        Err(ReentryDecline::UnsupportedSuspension)
    );
    assert_eq!(
        ir.exprs
            .iter()
            .map(|expression| format!("{expression:?}"))
            .collect::<Vec<_>>(),
        expressions_before
    );
}

/// `if (c) { leaf() } else { 2 }`: the suspending branch is taken by its condition on label 0 or
/// by holding label 1, without testing the condition again; the plain branch only forward.
#[test]
fn a_branch_holding_the_label_is_taken_without_its_condition() {
    let (mut ir, leaf) = file();
    let condition = ir.add_expr(IrExpr::GetValue(3));
    let suspending = call(&mut ir, leaf);
    let then = ir.add_expr(IrExpr::Block {
        stmts: vec![suspending],
        value: None,
    });
    let plain = ir.add_expr(IrExpr::Const(IrConst::Int(2)));
    let branch = ir.add_expr(IrExpr::When {
        branches: vec![(Some(condition), then), (None, plain)],
    });

    let (body, labels) = split(&mut ir, vec![branch]);

    assert_eq!(labels, Ok(1));
    let [rewritten] = stmts(&ir, body)[..] else {
        panic!("one statement");
    };
    let IrExpr::When { branches } = &ir.exprs[rewritten as usize] else {
        panic!("a when");
    };
    let [(Some(first), _), (Some(second), otherwise)] = branches[..] else {
        panic!("both branches are guarded");
    };
    let Some((forward, held)) = or_else(&ir, first) else {
        panic!("taken forward or by its label");
    };
    // The condition runs only forward: resuming does not evaluate it again.
    let Some((at_zero, tested)) = and_then(&ir, forward) else {
        panic!("taken forward by its condition");
    };
    assert!(is_label(&ir, at_zero, 0));
    assert_eq!(tested, condition);
    assert!(is_label(&ir, held, 1));
    assert!(is_label(&ir, second, 0));
    assert_eq!(otherwise, plain);
}

/// `while (c) { leaf() }`: a loop holding the label is entered once without its condition.
#[test]
fn a_loop_holding_the_label_is_entered_without_testing_its_condition() {
    let (mut ir, leaf) = file();
    let condition = ir.add_expr(IrExpr::GetValue(3));
    let suspending = call(&mut ir, leaf);
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![suspending],
        value: None,
    });
    let looped = ir.add_expr(IrExpr::While {
        cond: condition,
        body,
        update: None,
        post_test: false,
        label: None,
    });

    let (block, labels) = split(&mut ir, vec![looped]);

    assert_eq!(labels, Ok(1));
    let [rewritten] = stmts(&ir, block)[..] else {
        panic!("one statement");
    };
    let [resuming, looped] = stmts(&ir, rewritten)[..] else {
        panic!("the resuming flag, then the loop");
    };
    assert!(matches!(ir.exprs[resuming as usize],
        IrExpr::Variable { index: 10, ty: Ty::Boolean, init: Some(held), .. } if is_label(&ir, held, 1)));
    let IrExpr::While { cond, body, .. } = ir.exprs[looped as usize] else {
        panic!("the loop");
    };
    let Some((resuming, forward)) = or_else(&ir, cond) else {
        panic!("entered while resuming or forward");
    };
    assert!(matches!(ir.exprs[resuming as usize], IrExpr::GetValue(10)));
    let Some((at_zero, tested)) = and_then(&ir, forward) else {
        panic!("entered forward by its condition");
    };
    assert!(is_label(&ir, at_zero, 0));
    assert_eq!(tested, condition);
    let [cleared, _] = stmts(&ir, body)[..] else {
        panic!("clear the flag, then the body");
    };
    assert!(matches!(
        ir.exprs[cleared as usize],
        IrExpr::SetValue { var: 10, .. }
    ));
}

/// A suspension inside a `try` is not a statement this machine resumes at.
#[test]
fn a_suspension_inside_a_try_declines() {
    let (mut ir, leaf) = file();
    let suspending = call(&mut ir, leaf);
    let protected = ir.add_expr(IrExpr::Block {
        stmts: vec![suspending],
        value: None,
    });
    let attempt = ir.add_expr(IrExpr::Try {
        body: protected,
        catches: Vec::new(),
        finally: None,
        result: Ty::Unit,
    });

    let (_, labels) = split(&mut ir, vec![attempt]);

    assert_eq!(labels, Err(ReentryDecline::UnsupportedSuspension));
}

/// `a(leaf(), leaf())`: two suspensions in one statement are left to hoisting, which separates
/// them; this machine declines rather than resume in the middle of a call.
#[test]
fn two_suspensions_in_one_statement_decline() {
    let (mut ir, leaf) = file();
    let first = call(&mut ir, leaf);
    let second = call(&mut ir, leaf);
    let outer = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(leaf)),
        dispatch_receiver: None,
        args: vec![first, second],
    });

    let (_, labels) = split(&mut ir, vec![outer]);

    assert_eq!(labels, Err(ReentryDecline::UnsupportedSuspension));
}
