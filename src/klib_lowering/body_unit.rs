//! The common-IR unit that holds the lowered bodies of selected dependency declarations.
//!
//! Lowering one selected declaration also lowers every dependency declaration its body calls,
//! transitively, so the unit is closed over calls: each call names a function of the same unit.
//! A declaration is lowered once per signature, and a call cycle links back to the function it
//! already declared. If any reached body declines, the unit is left exactly as it was.

use std::collections::{HashMap, VecDeque};

use super::body_lowering::{BodyLowering, LinkedCallee};
use super::callee_facts::{KlibCalleeFact, KlibCalleeFacts};
use super::decline::{KlibBodyDecline, KlibBodyDeclineReason};
use super::function_lowering::{declare_function, function_header, FunctionHeader};
use super::ir_builtins::IrBuiltinOperators;
use crate::ir::{ExprId, FunId, IrFile};
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{KlibBodyCallable, KlibDeclarationSignature};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrBody, KlibIrStatement};
use crate::metadata::klib_ir::KlibIrSymbol;

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

/// Where a unit reads the declarations it lowers: their serialized bodies, and the frozen
/// declarations of the callees those bodies may call.
#[derive(Clone, Copy)]
struct Sources<'a> {
    bodies: &'a KlibDeclarationBodies,
    callees: &'a KlibCalleeFacts<'a>,
}

/// A declared function whose body is still to be lowered.
struct PendingBody<'a> {
    signature: &'a KlibDeclarationSignature,
    arena: &'a KlibIrArena,
    symbol: &'a KlibIrSymbol,
    statements: &'a [KlibIrStatement],
    header: FunctionHeader,
    function: FunId,
}

impl DependencyBodyUnit {
    /// The function holding `callable`'s body, lowering it, and every body it reaches through
    /// calls, the first time its signature is seen. A declined body leaves the unit as it was.
    ///
    /// `callees` holds the frozen declarations the body may call; a call of any other dependency
    /// declaration declines.
    pub fn lower_function<'a>(
        &mut self,
        callable: KlibBodyCallable<'a>,
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
    ) -> Result<FunId, KlibBodyDecline> {
        if let Some(function) = self.functions.get(callable.signature()) {
            return Ok(*function);
        }
        let checkpoint = UnitCheckpoint::of(self);
        let lowered = self.lower_reachable(callable, Sources { bodies, callees });
        lowered.map_err(|decline| {
            checkpoint.restore(self);
            if decline.declaration() == callable.signature() {
                decline
            } else {
                KlibBodyDecline::new(
                    callable.signature().clone(),
                    KlibBodyDeclineReason::CalleeDeclined(Box::new(decline)),
                )
            }
        })
    }

    pub fn ir(&self) -> &IrFile {
        &self.ir
    }

    fn lower_reachable<'a>(
        &mut self,
        callable: KlibBodyCallable<'a>,
        sources: Sources<'a>,
    ) -> Result<FunId, KlibBodyDecline> {
        let mut pending = VecDeque::new();
        let mut linker = Linker {
            functions: &mut self.functions,
            pending: &mut pending,
            sources,
        };
        let root = linker
            .declare(&mut self.ir, callable)
            .map_err(|reason| KlibBodyDecline::new(callable.signature().clone(), reason))?;
        while let Some(body) = linker.pending.pop_front() {
            let lowered = BodyLowering::new(
                body.arena,
                body.symbol,
                &body.header,
                &self.builtins,
                &mut self.ir,
                &mut linker,
            )
            .body(body.statements)
            .map_err(|reason| match reason {
                KlibBodyDeclineReason::CalleeDeclined(decline) => *decline,
                reason => KlibBodyDecline::new(body.signature.clone(), reason),
            })?;
            self.ir.functions[body.function as usize].body = Some(lowered);
        }
        Ok(root)
    }
}

/// Joins the calls of the bodies being lowered to the unit's functions, declaring each callee
/// reached for the first time and queueing its body.
pub(super) struct Linker<'u, 's> {
    functions: &'u mut HashMap<KlibDeclarationSignature, FunId>,
    pending: &'u mut VecDeque<PendingBody<'s>>,
    sources: Sources<'s>,
}

impl<'s> Linker<'_, 's> {
    /// The function a call of the public declaration `callee` invokes.
    pub(super) fn link(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Public(callee.clone());
        let function = match self.functions.get(&signature) {
            Some(function) => *function,
            None => {
                let callable = match self.sources.callees.get(&signature) {
                    None => {
                        return Err(KlibBodyDeclineReason::UnselectedCallee(Box::new(
                            callee.clone(),
                        )))
                    }
                    Some(KlibCalleeFact::Ambiguous) => {
                        return Err(KlibBodyDeclineReason::AmbiguousCallee(Box::new(
                            callee.clone(),
                        )))
                    }
                    Some(KlibCalleeFact::Selected(callable)) => callable,
                };
                self.declare(ir, callable).map_err(|reason| {
                    KlibBodyDeclineReason::CalleeDeclined(Box::new(KlibBodyDecline::new(
                        signature.clone(),
                        reason,
                    )))
                })?
            }
        };
        let declaration = &ir.functions[function as usize];
        Ok(LinkedCallee {
            function,
            params: declaration.params.clone(),
            ret: declaration.ret,
            context_count: ir.fn_context_counts.get(&function).copied().unwrap_or(0),
        })
    }

    /// Declare `callable`'s function and queue its body.
    fn declare(
        &mut self,
        ir: &mut IrFile,
        callable: KlibBodyCallable<'s>,
    ) -> Result<FunId, KlibBodyDeclineReason> {
        let (arena, function) = self
            .sources
            .bodies
            .function(callable.signature())
            .ok_or(KlibBodyDeclineReason::Unjoined)?;
        let statements = match &function.body {
            None => return Err(KlibBodyDeclineReason::NoBody),
            Some(KlibIrBody::Synthetic(_)) => return Err(KlibBodyDeclineReason::SyntheticBody),
            Some(KlibIrBody::Block(statements)) => statements,
        };
        let header = function_header(callable, arena, function)?;
        let declared = declare_function(ir, callable, &header);
        self.functions
            .insert(callable.signature().clone(), declared);
        self.pending.push_back(PendingBody {
            signature: callable.signature(),
            arena,
            symbol: &function.base.symbol,
            statements,
            header,
            function: declared,
        });
        Ok(declared)
    }
}

/// What a unit held before a lowering began. Lowering appends functions and expressions and
/// records facts keyed by them; restoring removes exactly those.
struct UnitCheckpoint {
    expressions: usize,
    functions: usize,
}

impl UnitCheckpoint {
    fn of(unit: &DependencyBodyUnit) -> Self {
        Self {
            expressions: unit.ir.exprs.len(),
            functions: unit.ir.functions.len(),
        }
    }

    fn restore(self, unit: &mut DependencyBodyUnit) {
        let first = ExprId::try_from(self.expressions).expect("an expression id fits u32");
        let ir = &mut unit.ir;
        ir.exprs.truncate(self.expressions);
        ir.logical_types.retain(|expression, _| *expression < first);
        ir.checked_return_depths
            .retain(|expression, _| *expression < first);
        ir.binding_read_stability
            .retain(|expression, _| *expression < first);
        ir.callable_scopes.retain(|expression| *expression < first);
        ir.negations.retain(|expression| *expression < first);
        ir.whens
            .exhaustive
            .retain(|expression, _| *expression < first);
        let first = FunId::try_from(self.functions).expect("a function id fits u32");
        ir.functions.truncate(self.functions);
        ir.fn_source_names.retain(|function, _| *function < first);
        ir.fn_params.retain(|function, _| *function < first);
        ir.extension_receiver_fns
            .retain(|function| *function < first);
        ir.fn_context_counts.retain(|function, _| *function < first);
        unit.functions.retain(|_, function| *function < first);
    }
}
