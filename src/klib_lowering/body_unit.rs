//! The common-IR unit that holds the lowered bodies of selected dependency declarations.

use std::collections::HashMap;

use super::body_lowering::BodyLowering;
use super::decline::{KlibBodyDecline, KlibBodyDeclineReason};
use super::function_lowering::{add_function, function_header};
use super::ir_builtins::IrBuiltinOperators;
use crate::ir::{ExprId, FunId, IrFile};
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{KlibBodyCallable, KlibDeclarationSignature};
use crate::metadata::klib_ir::tree::KlibIrBody;

/// Lowered dependency bodies in one common-IR file, one function per declaration signature.
pub struct DependencyBodyUnit {
    ir: IrFile,
    functions: HashMap<KlibDeclarationSignature, FunId>,
    builtins: IrBuiltinOperators,
}

impl Default for DependencyBodyUnit {
    fn default() -> Self {
        Self {
            ir: IrFile::default(),
            functions: HashMap::new(),
            builtins: IrBuiltinOperators::new(),
        }
    }
}

impl DependencyBodyUnit {
    /// The function holding `callable`'s body, lowering it the first time its signature is seen.
    /// A declined body leaves the unit as it was.
    pub fn lower_function(
        &mut self,
        callable: KlibBodyCallable<'_>,
        bodies: &KlibDeclarationBodies,
    ) -> Result<FunId, KlibBodyDecline> {
        if let Some(function) = self.functions.get(callable.signature()) {
            return Ok(*function);
        }
        let decline = |reason| KlibBodyDecline::new(callable.signature().clone(), reason);
        let (arena, function) = bodies
            .function(callable.signature())
            .ok_or_else(|| decline(KlibBodyDeclineReason::Unjoined))?;
        let statements = match &function.body {
            None => return Err(decline(KlibBodyDeclineReason::NoBody)),
            Some(KlibIrBody::Synthetic(_)) => {
                return Err(decline(KlibBodyDeclineReason::SyntheticBody))
            }
            Some(KlibIrBody::Block(statements)) => statements,
        };
        let header = function_header(callable, arena, function).map_err(decline)?;
        let checkpoint = ExpressionCheckpoint::of(&self.ir);
        let body = BodyLowering {
            arena,
            function: &function.base.symbol,
            header: &header,
            builtins: &self.builtins,
            ir: &mut self.ir,
        }
        .body(statements);
        let body = match body {
            Ok(body) => body,
            Err(reason) => {
                checkpoint.restore(&mut self.ir);
                return Err(decline(reason));
            }
        };
        let lowered = add_function(&mut self.ir, callable, header, body);
        self.functions.insert(callable.signature().clone(), lowered);
        Ok(lowered)
    }

    pub fn ir(&self) -> &IrFile {
        &self.ir
    }
}

/// The expressions a unit held before a body was lowered. Body lowering appends expressions and
/// records facts keyed by them; restoring removes exactly those.
struct ExpressionCheckpoint {
    expressions: usize,
}

impl ExpressionCheckpoint {
    fn of(ir: &IrFile) -> Self {
        Self {
            expressions: ir.exprs.len(),
        }
    }

    fn restore(self, ir: &mut IrFile) {
        let first = ExprId::try_from(self.expressions).expect("an expression id fits u32");
        ir.exprs.truncate(self.expressions);
        ir.logical_types.retain(|expression, _| *expression < first);
        ir.checked_return_depths
            .retain(|expression, _| *expression < first);
        ir.binding_read_stability
            .retain(|expression, _| *expression < first);
        ir.callable_scopes.retain(|expression| *expression < first);
        ir.whens
            .exhaustive
            .retain(|expression, _| *expression < first);
    }
}
