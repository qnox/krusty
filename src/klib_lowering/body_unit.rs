//! The common-IR unit that holds the lowered bodies of selected dependency declarations.
//!
//! Lowering one selected declaration also lowers every dependency declaration its body calls,
//! transitively, so the unit is closed over calls: each call names a function of the same unit.
//! A callee is joined to its serialized declaration by exact module-qualified identity, as a body
//! is, and declared from that declaration's header; a provider record of the same identity, when a
//! checked call selected one, is cross-checked against the header. A declaration is lowered once
//! per module-qualified signature, and a call cycle links back to the function it already declared.
//! If any reached body declines, the unit is left exactly as it was.

use crate::fx_hash::HashMap;
use std::collections::VecDeque;

use super::body_lowering::{BodyLowering, LinkedCallee, LoweredFunction};
use super::callee_facts::{KlibCalleeFact, KlibCalleeFacts};
use super::decline::{KlibBodyDecline, KlibBodyDeclineReason};
use super::function_lowering::{declare_function, function_header, FunctionHeader};
use super::ir_builtins::IrBuiltinOperators;
use crate::ir::{ExprId, FunId, IrFile};
use crate::klib_libraries::{KlibCalleeOrigin, KlibDeclarationBodies};
use crate::libraries::{KlibBodyCallable, KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrBody, KlibIrFunction, KlibIrStatement};
use crate::metadata::klib_ir::KlibIrSymbol;

/// Lowered dependency bodies in one common-IR file, one function per declaration signature.
pub struct DependencyBodyUnit {
    ir: IrFile,
    functions: HashMap<KlibDependencyCallable, FunId>,
    builtins: IrBuiltinOperators,
}

impl Default for DependencyBodyUnit {
    fn default() -> Self {
        Self {
            ir: IrFile::default(),
            functions: HashMap::default(),
            builtins: IrBuiltinOperators::new(),
        }
    }
}

/// Where a unit reads the declarations it lowers: their serialized declarations and bodies, and
/// the provider records of the callees a checked call selected.
#[derive(Clone, Copy)]
struct Sources<'a> {
    bodies: &'a KlibDeclarationBodies,
    callees: &'a KlibCalleeFacts<'a>,
}

/// A declared function whose body is still to be lowered.
struct PendingBody<'a> {
    identity: KlibDependencyCallable,
    arena: &'a KlibIrArena,
    symbol: &'a KlibIrSymbol,
    statements: &'a [KlibIrStatement],
    header: FunctionHeader,
    function: FunId,
}

impl DependencyBodyUnit {
    /// The function holding `callable`'s body, lowering it, and every body it reaches through
    /// calls, the first time its module-qualified identity is seen. A declined body leaves the
    /// unit as it was.
    ///
    /// `callees` holds the provider records of the callees a checked call selected; a callee it
    /// does not hold is described by its serialized header alone.
    pub fn lower_function<'a>(
        &mut self,
        callable: KlibBodyCallable<'a>,
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
    ) -> Result<FunId, KlibBodyDecline> {
        if let Some(function) = self.functions.get(callable.identity()).copied() {
            // Declared before, perhaps from its header alone as another body's callee: the
            // selection it is now asked for must describe it too.
            let (arena, declaration) = bodies.function(callable.identity()).ok_or_else(|| {
                KlibBodyDecline::new(
                    callable.signature().clone(),
                    KlibBodyDeclineReason::Unjoined,
                )
            })?;
            function_header(Some(callable), arena, declaration)
                .map_err(|reason| KlibBodyDecline::new(callable.signature().clone(), reason))?;
            return Ok(function);
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
            root: callable,
            active_module: callable.identity().module.clone(),
            sources,
        };
        let root = sources
            .bodies
            .function(callable.identity())
            .ok_or(KlibBodyDeclineReason::Unjoined)
            .and_then(|(arena, function)| {
                linker.declare(
                    &mut self.ir,
                    callable.identity(),
                    Some(callable),
                    arena,
                    function,
                )
            })
            .map_err(|reason| KlibBodyDecline::new(callable.signature().clone(), reason))?;
        while let Some(body) = linker.pending.pop_front() {
            linker.active_module.clone_from(&body.identity.module);
            let function = LoweredFunction {
                symbol: body.symbol,
                function: body.function,
            };
            let lowered = BodyLowering::new(
                body.arena,
                function,
                &body.header,
                &self.builtins,
                &mut self.ir,
                &mut linker,
            )
            .body(body.statements)
            .map_err(|reason| match reason {
                KlibBodyDeclineReason::CalleeDeclined(decline) => *decline,
                reason => KlibBodyDecline::new(body.identity.declaration, reason),
            })?;
            self.ir.functions[body.function as usize].body = Some(lowered);
        }
        Ok(root)
    }
}

/// Joins the calls of the bodies being lowered to the unit's functions, declaring each callee
/// reached for the first time and queueing its body.
pub(super) struct Linker<'u, 's> {
    functions: &'u mut HashMap<KlibDependencyCallable, FunId>,
    pending: &'u mut VecDeque<PendingBody<'s>>,
    /// The declaration this lowering was asked for, which its own recursive calls select.
    root: KlibBodyCallable<'s>,
    /// Declaring module of the body currently being lowered. Its producer dependency graph owns
    /// the origin of every public call symbol the body serialized.
    active_module: String,
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
        let identity = match self
            .sources
            .bodies
            .callee_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(Box::new(
                    callee.clone(),
                )))
            }
            KlibCalleeOrigin::Missing if callee.declaration().segments().len() > 1 => {
                // A member no library serializes is, in a complete library set, a fake override:
                // its real declaration is reached through serialized class declarations, which
                // lowering does not follow yet.
                return Err(KlibBodyDeclineReason::UnsupportedOperation(
                    "a call of a member no library serializes, such as a fake override",
                ));
            }
            KlibCalleeOrigin::Missing => {
                return Err(KlibBodyDeclineReason::UndeclaredCallee(Box::new(
                    callee.clone(),
                )))
            }
        };
        // The active selection decides every call, including one of a function an earlier
        // lowering already put in the unit.
        let selection = if &identity == self.root.identity() {
            Some(KlibCalleeFact::Selected(self.root))
        } else {
            self.sources.callees.get(&identity)
        };
        let selected = match selection {
            None => None,
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(KlibBodyDeclineReason::AmbiguousCallee(Box::new(
                    callee.clone(),
                )))
            }
            Some(KlibCalleeFact::Selected(callable)) => Some(callable),
        };
        let callee_declined = |reason| {
            KlibBodyDeclineReason::CalleeDeclined(Box::new(KlibBodyDecline::new(
                signature.clone(),
                reason,
            )))
        };
        let (arena, declaration) = self
            .sources
            .bodies
            .function(&identity)
            .ok_or_else(|| callee_declined(KlibBodyDeclineReason::Unjoined))?;
        if declaration.dispatch_receiver.is_some() {
            // A member call passes its dispatch receiver apart from its arguments and dispatches
            // virtually unless the member is final; lowering models neither yet.
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a call of a member function",
            ));
        }
        let function = match self.functions.get(&identity) {
            Some(function) => {
                // The function was declared under another lowering's selection, or from its
                // header alone; this lowering's selection must describe it too.
                if let Some(selected) = selected {
                    function_header(Some(selected), arena, declaration).map_err(callee_declined)?;
                }
                *function
            }
            None => self
                .declare(ir, &identity, selected, arena, declaration)
                .map_err(callee_declined)?,
        };
        let declaration = &ir.functions[function as usize];
        Ok(LinkedCallee {
            function,
            params: declaration.params.clone(),
            ret: declaration.ret,
            context_count: ir.fn_context_counts.get(&function).copied().unwrap_or(0),
        })
    }

    /// Declare serialized `function`, carrying `identity` and described also by the
    /// provider record `selected` when there is one, and queue its body.
    fn declare(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: Option<KlibBodyCallable<'s>>,
        arena: &'s KlibIrArena,
        function: &'s KlibIrFunction,
    ) -> Result<FunId, KlibBodyDeclineReason> {
        let statements = match &function.body {
            None => return Err(KlibBodyDeclineReason::NoBody),
            Some(KlibIrBody::Synthetic(_)) => return Err(KlibBodyDeclineReason::SyntheticBody),
            Some(KlibIrBody::Block(statements)) => statements,
        };
        let header = function_header(selected, arena, function)?;
        let declared = declare_function(ir, &header);
        self.functions.insert(identity.clone(), declared);
        self.pending.push_back(PendingBody {
            identity: identity.clone(),
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
        ir.short_circuits
            .retain(|expression, _| *expression < first);
        ir.written_casts.retain(|expression| *expression < first);
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
