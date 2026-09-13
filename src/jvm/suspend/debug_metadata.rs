//! Source and resume-line ownership for JVM coroutine `DebugMetadata`.

use super::{collect_suspension_points, is_suspension_point};
use crate::ir::{for_each_child, Callee, ExprId, IrExpr, IrFile};
use crate::libraries::InlineKind;
use std::collections::{HashMap, HashSet};

/// Capture each suspension point's source line and the line where execution resumes.
pub(super) fn capture_suspension_lines(
    ir: &IrFile,
    body: ExprId,
    suspend_functions: &HashSet<u32>,
    final_resume_line: Option<u32>,
) -> HashMap<ExprId, (u32, u32)> {
    let mut result = HashMap::new();
    collect_suspension_lines(ir, body, suspend_functions, final_resume_line, &mut result);
    // An expression-bodied suspension consumed immediately by an inline lambda call resumes in
    // the inlined SMAP region rather than on the suspension's own source line. A concrete successor
    // line (for example a selector on the next physical line) remains authoritative.
    for (call, (line, resume)) in &mut result {
        if *resume == *line && resumes_into_inline_call(ir, body, *call) {
            *resume = ir.source_line_count.saturating_add(1);
        }
    }
    result
}

/// The line control reaches first when a `when` arm runs. A block arm enters on its first statement,
/// not on the opening brace.
fn branch_entry_line(ir: &IrFile, expression: ExprId) -> Option<u32> {
    if let IrExpr::Block { stmts, value } = &ir.exprs[expression as usize] {
        if let Some(&first) = stmts.first().or(value.as_ref()) {
            return branch_entry_line(ir, first);
        }
    }
    execution_start_line(ir, expression)
}

fn direct_expression_source_line(ir: &IrFile, expression: ExprId) -> Option<u32> {
    ir.expr_source_lines
        .get(&expression)
        .copied()
        .or_else(|| ir.expr_lines.get(&expression).copied())
}

fn expression_source_line(ir: &IrFile, expression: ExprId) -> Option<u32> {
    direct_expression_source_line(ir, expression)
        .or_else(|| nearest_expression_source_line(ir, expression))
}

fn execution_start_line(ir: &IrFile, expression: ExprId) -> Option<u32> {
    if let IrExpr::Variable {
        init: Some(initializer),
        ..
    } = ir.exprs[expression as usize]
    {
        return expression_source_line(ir, initializer);
    }
    direct_expression_source_line(ir, expression)
}

fn nearest_expression_source_line(ir: &IrFile, root: ExprId) -> Option<u32> {
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    let mut best = None;
    while let Some(expression) = stack.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let Some(&line) = ir
            .expr_source_lines
            .get(&expression)
            .or_else(|| ir.expr_lines.get(&expression))
        {
            best = Some(best.map_or(line, |current: u32| current.min(line)));
        }
        for_each_child(&ir.exprs, expression, &mut |child| stack.push(child));
    }
    best
}

fn contains_lambda(ir: &IrFile, root: ExprId) -> bool {
    if matches!(ir.exprs[root as usize], IrExpr::Lambda { .. }) {
        return true;
    }
    let mut found = false;
    for_each_child(&ir.exprs, root, &mut |child| {
        found |= contains_lambda(ir, child);
    });
    found
}

fn is_inline_lambda_call(ir: &IrFile, expression: ExprId) -> bool {
    let IrExpr::Call { callee, args, .. } = &ir.exprs[expression as usize] else {
        return false;
    };
    let selected_inline = ir.inline_call_sites.contains(&expression)
        || matches!(callee, Callee::Static { inline, .. } if *inline != InlineKind::None);
    selected_inline && args.iter().any(|&argument| contains_lambda(ir, argument))
}

fn resumes_into_inline_call(ir: &IrFile, root: ExprId, suspension: ExprId) -> bool {
    fn walk(
        ir: &IrFile,
        expression: ExprId,
        suspension: ExprId,
        inline_consumer: bool,
    ) -> Option<bool> {
        if expression == suspension {
            return Some(inline_consumer);
        }
        let inline_consumer = inline_consumer
            || is_inline_lambda_call(ir, expression)
            || ir.inline_regions.contains(&expression);
        let mut result = None;
        for_each_child(&ir.exprs, expression, &mut |child| {
            if result.is_none() {
                result = walk(ir, child, suspension, inline_consumer);
            }
        });
        result
    }

    walk(ir, root, suspension, false).unwrap_or(false)
}

fn collect_suspension_lines(
    ir: &IrFile,
    expression: ExprId,
    suspend_functions: &HashSet<u32>,
    fall_through: Option<u32>,
    out: &mut HashMap<ExprId, (u32, u32)>,
) {
    if is_suspension_point(ir, expression, suspend_functions) {
        if let Some(line) = expression_source_line(ir, expression) {
            let resume = fall_through.unwrap_or(line);
            out.entry(expression).or_insert((line, resume));
        }
        return;
    }

    match &ir.exprs[expression as usize] {
        IrExpr::Block { stmts, value } => {
            let items: Vec<ExprId> = stmts.iter().copied().chain(value.iter().copied()).collect();
            for (index, &item) in items.iter().enumerate() {
                let next = items[index + 1..]
                    .iter()
                    .find_map(|&next| {
                        execution_start_line(ir, next).or_else(|| {
                            let current = expression_source_line(ir, item)?;
                            nearest_expression_source_line(ir, next).filter(|&line| line > current)
                        })
                    })
                    .or(fall_through);
                collect_suspension_lines(ir, item, suspend_functions, next, out);
            }
        }
        IrExpr::When { branches } => {
            for (index, (condition, body)) in branches.iter().enumerate() {
                let next_condition = branches[index + 1..].iter().find_map(|(condition, _)| {
                    condition.and_then(|condition| expression_source_line(ir, condition))
                });
                if let Some(condition) = condition {
                    collect_suspension_lines(
                        ir,
                        *condition,
                        suspend_functions,
                        expression_source_line(ir, *body)
                            .or(next_condition)
                            .or(fall_through),
                        out,
                    );
                }
                // An arm resumes at the next branch condition/body. The last arm converges on the
                // `when` merge, which kotlinc attributes to the expression itself.
                let next_branch = branches[index + 1..]
                    .iter()
                    .find_map(|(condition, body)| {
                        condition
                            .and_then(|condition| expression_source_line(ir, condition))
                            .or_else(|| branch_entry_line(ir, *body))
                    })
                    .or_else(|| direct_expression_source_line(ir, expression));
                collect_suspension_lines(
                    ir,
                    *body,
                    suspend_functions,
                    next_branch.or(fall_through),
                    out,
                );
            }
        }
        IrExpr::While {
            cond, body, update, ..
        } => {
            let condition_line = expression_source_line(ir, *cond);
            collect_suspension_lines(
                ir,
                *cond,
                suspend_functions,
                expression_source_line(ir, *body).or(fall_through),
                out,
            );
            collect_suspension_lines(
                ir,
                *body,
                suspend_functions,
                update
                    .and_then(|update| expression_source_line(ir, update))
                    .or(condition_line)
                    .or(fall_through),
                out,
            );
            if let Some(update) = update {
                collect_suspension_lines(
                    ir,
                    *update,
                    suspend_functions,
                    condition_line.or(fall_through),
                    out,
                );
            }
        }
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            let region_fall_through = finally
                .and_then(|finally| expression_source_line(ir, finally))
                .or(fall_through);
            let body_fall_through = ir.expr_end_lines.get(body).copied().or(region_fall_through);
            collect_suspension_lines(ir, *body, suspend_functions, body_fall_through, out);
            for catch in catches {
                collect_suspension_lines(
                    ir,
                    catch.body,
                    suspend_functions,
                    region_fall_through,
                    out,
                );
            }
            if let Some(finally) = finally {
                collect_suspension_lines(ir, *finally, suspend_functions, fall_through, out);
            }
        }
        IrExpr::Variable {
            init: Some(initializer),
            ..
        } if matches!(
            ir.exprs[*initializer as usize],
            IrExpr::Block { .. } | IrExpr::When { .. } | IrExpr::Try { .. }
        ) =>
        {
            collect_suspension_lines(ir, *initializer, suspend_functions, fall_through, out);
        }
        IrExpr::Return(Some(value)) if is_suspension_point(ir, *value, suspend_functions) => {
            collect_suspension_lines(ir, *value, suspend_functions, Some(u32::MAX), out);
        }
        IrExpr::Return(Some(value)) => {
            let mut calls = HashSet::new();
            collect_suspension_points(ir, *value, suspend_functions, &mut calls);
            for call in calls {
                if resumes_into_inline_call(ir, *value, call) {
                    if let Some(line) = expression_source_line(ir, call) {
                        out.entry(call)
                            .or_insert((line, ir.source_line_count.saturating_add(1)));
                    }
                }
            }
            collect_suspension_lines(
                ir,
                *value,
                suspend_functions,
                ir.expr_end_lines.get(value).copied().or(fall_through),
                out,
            );
        }
        _ => {
            let mut children = Vec::new();
            for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
            for (index, &child) in children.iter().enumerate() {
                let next = children[index + 1..]
                    .iter()
                    .find_map(|&next| expression_source_line(ir, next))
                    .or_else(|| {
                        expression_source_line(ir, expression)
                            .filter(|&line| Some(line) != expression_source_line(ir, child))
                    })
                    .or(fall_through);
                collect_suspension_lines(ir, child, suspend_functions, next, out);
            }
        }
    }
}
