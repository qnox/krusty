//! The frame an inline expansion's returns leave: the copied body producing its sole tail
//! return's value directly, or a labelled loop each return breaks out of with the result.

use crate::ir::{ExprId, IrExpr, IrFile};
use crate::types::Ty;

/// A return an inline expansion leaves through: the `return` node, its value, and whether a nested
/// lambda's template holds it.
#[derive(Clone, Copy)]
pub(crate) struct InlineReturn {
    pub(crate) returned: ExprId,
    pub(crate) value: Option<ExprId>,
    pub(crate) nested: bool,
}

/// Rewrite `root` to PRODUCE the value of its sole rewritten return, or leave it exactly as it was.
///
/// The shape is PROVED before anything changes, and proving it cannot change anything: the chain of
/// statement blocks from `root` down to `tail` is collected from a shared reference. Only once that
/// answers does the `Unit` placeholder get allocated and the blocks rewritten. Ordering it the other
/// way left an orphan `UnitInstance` in the arena whenever the answer turned out to be no — the
/// block shapes were restored, but the allocation was not, so a refused optimization still shifted
/// every expression identity after it.
pub(crate) fn produce_sole_tail_return(
    ir: &mut crate::ir::IrFile,
    root: ExprId,
    tail: ExprId,
    value: Option<ExprId>,
    result_ty: Ty,
    close_line: Option<u32>,
) -> bool {
    let Some(chain) = tail_statement_block_chain(ir, root, tail) else {
        return false;
    };
    let mut produced = value.unwrap_or_else(|| ir.add_expr(IrExpr::UnitInstance));
    if result_ty == Ty::Unit && !matches!(ir.expr(produced), IrExpr::UnitInstance) {
        let effect = produced;
        let unit = ir.add_expr(IrExpr::UnitInstance);
        ir.copy_inline_copy_mark(tail, unit);
        if let Some(line) = ir
            .source_line(tail)
            .or_else(|| ir.source_line(effect))
            .or_else(|| ir.fallthrough_return_line(tail))
            .or_else(|| ir.end_line(tail))
            .or_else(|| ir.end_line(root))
            .or(close_line)
        {
            ir.set_source_line(unit, line);
        }
        ir.retain_inline_unit_line(unit);
        produced = ir.add_expr(IrExpr::Block {
            stmts: vec![effect],
            value: Some(unit),
        });
    }
    if matches!(ir.expr(produced), IrExpr::UnitInstance) {
        ir.copy_inline_copy_mark(tail, produced);
        if !ir.source_line(produced).is_some() {
            if let Some(line) = ir
                .source_line(tail)
                .or_else(|| ir.fallthrough_return_line(tail))
                .or_else(|| ir.end_line(tail))
                .or_else(|| ir.end_line(root))
                .or(close_line)
            {
                ir.set_source_line(produced, line);
            }
        }
        ir.retain_inline_unit_line(produced);
    }
    ir.exprs[tail as usize] = IrExpr::Block {
        stmts: Vec::new(),
        value: Some(produced),
    };
    for &block in &chain {
        let IrExpr::Block { stmts, value: None } = ir.expr(block).clone() else {
            unreachable!("the chain was proved to be statement blocks")
        };
        let mut stmts = stmts;
        let last = stmts.pop().expect("a chained block ends in a statement");
        ir.exprs[block as usize] = IrExpr::Block {
            stmts,
            value: Some(last),
        };
    }
    true
}

/// The statement blocks from `block` down to `tail`, outermost first — the ones that must become
/// value-producing for `tail`'s value to reach `block`.
///
/// `None` unless the path is a chain of statement blocks each ending in the next, which is every
/// case that has to keep the caller's result local and labelled exit loop: a `return` anywhere but
/// the tail must be able to carry its value out of the middle of the body.
fn tail_statement_block_chain(
    ir: &crate::ir::IrFile,
    block: ExprId,
    tail: ExprId,
) -> Option<Vec<ExprId>> {
    let mut chain = Vec::new();
    let mut current = block;
    loop {
        let IrExpr::Block { stmts, value: None } = ir.expr(current) else {
            return None;
        };
        let &last = stmts.last()?;
        chain.push(current);
        if last == tail {
            return Some(chain);
        }
        current = last;
    }
}

/// Whether the return `returned`, copied into an inline frame, leaves that frame. A return of
/// depth 0, or one with no checked depth, belongs to the inlined body and exits its frame; its
/// depth is consumed. A deeper return names a declaration enclosing the frame: it stays a return,
/// one lexical frame nearer its target.
pub(crate) fn leaves_inline_frame(ir: &mut IrFile, returned: ExprId) -> bool {
    match ir.checked_return_depths.get(&returned).copied() {
        Some(depth) if depth > 0 => {
            ir.checked_return_depths.insert(returned, depth - 1);
            false
        }
        _ => {
            ir.checked_return_depths.remove(&returned);
            true
        }
    }
}

/// The statements that leave inline-return frame `label` with `value`: store it as the frame's
/// result, then `break`. A return in the frame's own body stores by the result's value index; one
/// in a nested lambda's template names the frame instead. With no result, the value still runs.
pub(crate) fn frame_exit(
    ir: &mut IrFile,
    label: &str,
    result: Option<u32>,
    nested: bool,
    value: Option<ExprId>,
    source: ExprId,
) -> IrExpr {
    let mut stmts = Vec::new();
    let store = if let Some(value) = value {
        let store = match (result, nested) {
            (Some(var), false) => Some(ir.add_expr(IrExpr::SetValue { var, value })),
            (Some(_), true) => Some(ir.add_expr(IrExpr::SetFrameResult {
                frame: label.to_string(),
                value,
            })),
            (None, _) => {
                stmts.push(value);
                None
            }
        };
        if let Some(store) = store {
            stmts.push(store);
        }
        store
    } else {
        None
    };
    // The break replaces `source`. It keeps that return's line and inline-copy identity so the
    // `goto` after `finally` is the rewritten return, not an unlabeled jump the goto cleanup
    // threads into the finalizer's own exit. The store takes the same line, marked at the
    // `istore`/`astore` rather than at the value that precedes it.
    let exit = ir.add_expr(IrExpr::Break {
        label: Some(label.to_string()),
    });
    let line = ir.statement_line(source).or_else(|| ir.source_line(source));
    if let Some(line) = line {
        ir.set_statement_line(exit, line);
        ir.set_source_line(exit, line);
        if let Some(store) = store {
            ir.set_source_line(store, line);
            ir.copy_inline_copy_mark(source, store);
        }
    }
    ir.copy_inline_copy_mark(source, exit);
    stmts.push(exit);
    IrExpr::Block { stmts, value: None }
}

#[cfg(test)]
mod tail_promotion_tests {
    use super::{produce_sole_tail_return, tail_statement_block_chain};
    use crate::ir::{ExprId, IrConst, IrExpr, IrFile};
    use crate::types::Ty;

    fn statement(ir: &mut IrFile, value: i32) -> ExprId {
        ir.add_expr(IrExpr::Const(IrConst::Int(value)))
    }

    fn statement_block(ir: &mut IrFile, stmts: Vec<ExprId>) -> ExprId {
        ir.add_expr(IrExpr::Block { stmts, value: None })
    }

    /// Everything the arena holds, as text: both its LENGTH and every node, so a refusal that
    /// allocated or replaced anything at all shows up.
    fn arena(ir: &IrFile) -> String {
        format!("{} nodes: {:?}", ir.exprs.len(), ir.exprs)
    }

    fn assert_block(ir: &IrFile, block: ExprId, stmts: &[ExprId], value: Option<ExprId>) {
        let IrExpr::Block {
            stmts: actual,
            value: produced,
        } = ir.expr(block)
        else {
            panic!("expression {block} is not a block");
        };
        assert_eq!(actual.as_slice(), stmts, "block {block} statements");
        assert_eq!(*produced, value, "block {block} value");
    }

    /// The direct case: the expansion's body IS the block whose last statement is the tail.
    #[test]
    fn a_direct_tail_statement_becomes_the_blocks_value() {
        let mut ir = IrFile::default();
        let first = statement(&mut ir, 1);
        let returned = statement(&mut ir, 2);
        let tail = statement_block(&mut ir, vec![]);
        let block = statement_block(&mut ir, vec![first, tail]);

        assert!(produce_sole_tail_return(
            &mut ir,
            block,
            tail,
            Some(returned),
            Ty::Int,
            None,
        ));
        assert_block(&ir, block, &[first], Some(tail));
        assert_block(&ir, tail, &[], Some(returned));
    }

    /// A statement-bodied inline function wraps its body one level deeper, so the promotion has to
    /// descend — and every block on that path becomes value-producing, or the value is discarded by
    /// whichever block above it still ends in a statement.
    #[test]
    fn a_nested_tail_statement_promotes_every_block_on_its_path() {
        let mut ir = IrFile::default();
        let first = statement(&mut ir, 1);
        let returned = statement(&mut ir, 2);
        let tail = statement_block(&mut ir, vec![]);
        let inner = statement_block(&mut ir, vec![first, tail]);
        let outer = statement_block(&mut ir, vec![inner]);

        assert!(produce_sole_tail_return(
            &mut ir,
            outer,
            tail,
            Some(returned),
            Ty::Int,
            None,
        ));
        assert_block(&ir, outer, &[], Some(inner));
        assert_block(&ir, inner, &[first], Some(tail));
        assert_block(&ir, tail, &[], Some(returned));
    }

    /// The return is not the tail, which is the case that must keep the caller's result local and
    /// labelled exit loop — a non-local return has to be able to carry its value out of the middle
    /// of the body. Nothing may be left half-promoted when the answer is no.
    #[test]
    fn a_statement_after_the_return_changes_nothing() {
        let mut ir = IrFile::default();
        let returned = statement(&mut ir, 1);
        let tail = statement_block(&mut ir, vec![]);
        let after = statement(&mut ir, 2);
        let block = statement_block(&mut ir, vec![tail, after]);
        let before = arena(&ir);

        assert!(!produce_sole_tail_return(
            &mut ir,
            block,
            tail,
            Some(returned),
            Ty::Int,
            None,
        ));
        assert_eq!(arena(&ir), before, "a refused promotion mutates nothing");
    }

    /// The `Unit` case, which is the one that used to allocate before it knew the answer: there is
    /// no returned expression, so the promotion has to materialize `Unit` itself. A refusal must
    /// leave the arena at exactly its previous LENGTH — the block shapes being restored is not
    /// enough, because an orphan allocation shifts every later expression identity.
    #[test]
    fn a_refused_unit_return_allocates_nothing() {
        let mut ir = IrFile::default();
        let tail = statement_block(&mut ir, vec![]);
        let after = statement(&mut ir, 1);
        let block = statement_block(&mut ir, vec![tail, after]);
        let before = arena(&ir);

        assert!(!produce_sole_tail_return(
            &mut ir,
            block,
            tail,
            None,
            Ty::Unit,
            None,
        ));
        assert_eq!(
            arena(&ir),
            before,
            "a refused Unit promotion allocates no placeholder"
        );
    }

    /// Refusal deep in the path also leaves every block above it untouched: the chain is proved
    /// before anything is written, so a failure below happens before any of them is reached.
    #[test]
    fn a_refusal_below_leaves_the_blocks_above_untouched() {
        let mut ir = IrFile::default();
        let returned = statement(&mut ir, 1);
        let tail = statement_block(&mut ir, vec![]);
        let after = statement(&mut ir, 2);
        let inner = statement_block(&mut ir, vec![tail, after]);
        let outer = statement_block(&mut ir, vec![inner]);
        let before = arena(&ir);

        assert!(!produce_sole_tail_return(
            &mut ir,
            outer,
            tail,
            Some(returned),
            Ty::Int,
            None,
        ));
        assert_eq!(arena(&ir), before, "a refused promotion mutates nothing");
    }

    /// A block that already produces a value is not a statement block, so there is no tail
    /// statement to promote.
    #[test]
    fn a_value_producing_block_changes_nothing() {
        let mut ir = IrFile::default();
        let returned = statement(&mut ir, 1);
        let tail = statement_block(&mut ir, vec![]);
        let produced = statement(&mut ir, 2);
        let block = ir.add_expr(IrExpr::Block {
            stmts: vec![tail],
            value: Some(produced),
        });
        let before = arena(&ir);

        assert!(!produce_sole_tail_return(
            &mut ir,
            block,
            tail,
            Some(returned),
            Ty::Int,
            None,
        ));
        assert_eq!(arena(&ir), before, "a refused promotion mutates nothing");
    }

    /// The proof itself takes a SHARED reference, so it cannot write even in principle. Asserted
    /// here as a contract rather than left to the signature alone.
    #[test]
    fn the_chain_is_proved_without_touching_the_arena() {
        let mut ir = IrFile::default();
        let first = statement(&mut ir, 1);
        let tail = statement_block(&mut ir, vec![]);
        let inner = statement_block(&mut ir, vec![first, tail]);
        let outer = statement_block(&mut ir, vec![inner]);
        let before = arena(&ir);

        assert_eq!(
            tail_statement_block_chain(&ir, outer, tail),
            Some(vec![outer, inner])
        );
        assert_eq!(arena(&ir), before, "proving the chain reads only");
    }
}
