//! Inline functions the unit's bodies call.
//!
//! kotlinc expands each call of an inline function of a library it links, which a library compiled
//! before Kotlin 2.4.20 still serializes as a call. The unit lowers such a function from its
//! serialized body like any other, keeps it out of the emitted program, and expands every call of
//! it from that lowered body, as common lowering expands a call of an inline function it compiles.
//! A lambda the call passes for a parameter the function inlines is not created: each invocation
//! of the parameter in the copied body is expanded from the lambda's own body instead.

use crate::fir::ExternalCallableId;
use crate::fx_hash::{HashMap, HashSet};

use crate::inline_expansion::{InlineBodySplice, InlineOperand, SplicedResult};
use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile};
use crate::libraries::{KlibBodyCallable, KlibDependencyCallable};
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrFunction};
use crate::types::{stored_value_ty, InlineExpansionMode, Ty, TyKind, TypeName};

use super::super::decline::KlibBodyDeclineReason;
use super::super::function_lowering::{function_header, prepared_inline_header, FunctionHeader};
use super::super::inline_templates::{expansion_modes, modifiers_with_receiver};

/// Whether the serialized `function` is inline: every call of it is expanded, never made.
fn declares_inline(function: &KlibIrFunction) -> bool {
    function.base.flags & crate::metadata::function_flags::IS_INLINE != 0
}

/// Decline a call of a suspend inline `function` that has no prepared copy to expand from.
///
/// Its serialized body still holds the unlowered suspension intrinsics, which only the library's
/// prepared copy replaces with the continuation operations, so expanding it would emit a
/// suspension the unit cannot lower.
fn require_prepared_copy(function: &KlibIrFunction) -> Result<(), KlibBodyDeclineReason> {
    if declares_inline(function)
        && function.base.flags & crate::metadata::function_flags::IS_SUSPEND != 0
    {
        return Err(KlibBodyDeclineReason::UnsupportedOperation(
            "a call of a suspend inline function without a prepared copy",
        ));
    }
    Ok(())
}

impl<'s> super::Linker<'_, 's> {
    /// The function a call of the top-level `declaration` links to, declared when this unit has not
    /// yet. A call of a suspend inline function expands from the copy its library prepared, whose
    /// header is the selected declaration's; without one the call is declined, since the serialized
    /// body still holds the unlowered suspension intrinsics.
    pub(super) fn top_level_callee(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: Option<KlibBodyCallable<'s>>,
        arena: &'s KlibIrArena,
        declaration: &'s KlibIrFunction,
    ) -> Result<FunId, KlibBodyDeclineReason> {
        let copy = self.sources.callees.expansion_copy(identity, declaration);
        match (self.functions.get(identity).copied(), copy) {
            (Some(function), _) => {
                // The function was declared under another lowering's selection, or from its
                // header alone; this lowering's selection must describe it too.
                if let Some(selected) = selected {
                    match copy {
                        Some((arena, copy)) => {
                            prepared_inline_header(
                                selected,
                                selected.type_parameters(),
                                arena,
                                copy,
                            )?;
                        }
                        None => {
                            function_header(Some(selected), arena, declaration)?;
                        }
                    }
                }
                Ok(function)
            }
            (None, Some((arena, copy))) => {
                let selected = selected.ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                    "a call of a suspend inline function without a selected declaration",
                ))?;
                let header =
                    prepared_inline_header(selected, selected.type_parameters(), arena, copy)?;
                self.declare_with_header(ir, identity, Some(selected), arena, copy, header)
            }
            (None, None) => self.declare_callee(ir, identity, selected, arena, declaration),
        }
    }

    /// Declare a callee from its serialized body, which a call of a suspend inline function cannot
    /// expand without the copy its library prepared.
    pub(super) fn declare_callee(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: Option<KlibBodyCallable<'s>>,
        arena: &'s KlibIrArena,
        function: &'s KlibIrFunction,
    ) -> Result<FunId, KlibBodyDeclineReason> {
        require_prepared_copy(function)?;
        self.declare(ir, identity, selected, arena, function)
    }

    /// Whether a call this unit links to a function realized outside it (another unit's export)
    /// calls a `suspend` declaration, as the provider's record of it says.
    pub(super) fn external_is_suspend(&self, target: ExternalCallableId) -> bool {
        self.sources
            .declarations
            .external_callable(target)
            .is_some_and(|realization| realization.callable.suspend)
    }
}

/// A function the unit lowered from its serialized body, with the selected declaration it was
/// lowered for, that is declared as an inline callee when it is inline.
pub(super) struct Candidate<'a, 's> {
    declared: FunId,
    function: &'a KlibIrFunction,
    header: &'a FunctionHeader,
    selected: Option<KlibBodyCallable<'s>>,
}

impl<'a, 's> Candidate<'a, 's> {
    pub(super) fn new(
        declared: FunId,
        function: &'a KlibIrFunction,
        header: &'a FunctionHeader,
        selected: Option<KlibBodyCallable<'s>>,
    ) -> Self {
        Self {
            declared,
            function,
            header,
            selected,
        }
    }

    /// A suspend body is a suspend function of the unit, as a source one is of its file: the common
    /// coroutine preparation gives it its continuation and state machine. An inline-only prepared
    /// copy is registered too, because membership is what makes a call of it a suspension point of
    /// its caller; coroutine preparation skips the template itself (it is never emitted, so it gets
    /// no state machine) and its call sites are expanded from its body.
    pub(super) fn register_suspension(&self, ir: &mut IrFile) {
        if self.function.base.flags & crate::metadata::function_flags::IS_SUSPEND != 0 {
            ir.suspend_funs.push(self.declared);
        }
    }

    /// Declare it, which is `identity`, as one the unit expands at each call, unless it is the
    /// selected `root`: the root is lowered as the declaration it is, and a call of an inline
    /// callee is expanded.
    pub(super) fn declare(
        &self,
        ir: &mut IrFile,
        root: Option<KlibBodyCallable<'_>>,
        identity: &KlibDependencyCallable,
        is_interface: &mut dyn FnMut(TypeName) -> Result<bool, KlibBodyDeclineReason>,
    ) -> Result<(), KlibBodyDeclineReason> {
        let root = root.is_some_and(|root| root.identity() == identity);
        if root || !declares_inline(self.function) {
            return Ok(());
        }
        let Self {
            declared,
            function,
            header,
            selected,
        } = *self;
        declare(ir, declared, function, header, selected, is_interface)
    }
}

/// Declare `function`, lowered from the serialized inline `declaration` with `header`, as one the
/// unit expands at each call. The `selected` declaration records the `crossinline`/`noinline`
/// modifiers of its value parameters, which decide the parameters whose lambdas are expanded, and
/// the type parameters a call binds. A form whose expansion is not modelled declines the call.
pub(super) fn declare(
    ir: &mut IrFile,
    function: FunId,
    declaration: &KlibIrFunction,
    header: &FunctionHeader,
    selected: Option<KlibBodyCallable<'_>>,
    is_interface: &mut dyn FnMut(TypeName) -> Result<bool, KlibBodyDeclineReason>,
) -> Result<(), KlibBodyDeclineReason> {
    let receiver = header.extension_receiver_index();
    let modifiers = match selected {
        Some(selected) => selected.inline_modifiers().to_vec(),
        // A declaration no provider records, a private one, writes its modifiers in its own
        // serialized parameters.
        None => declaration
            .context_parameters
            .iter()
            .chain(&declaration.regular_parameters)
            .map(|parameter| parameter.inline_modifier())
            .collect(),
    };
    let modes = expansion_modes(header.params(), receiver, &modifiers)?;
    let info = ir
        .fn_params
        .get_mut(&function)
        .expect("a declared function publishes its parameters");
    info.inline_modifiers = modifiers_with_receiver(&modifiers, receiver);
    info.expansion_modes = modes;
    if !declaration.type_parameters.is_empty() {
        sign_type_parameters(ir, function, declaration, header, selected, is_interface)?;
    }
    ir.inline_fns.insert(function);
    ir.inline_only_fns.insert(function);
    Ok(())
}

/// Record the type parameters of the generic inline `function`, as checked FIR records a source
/// declaration's: the identities a call binds and the reified ones a target patches.
fn sign_type_parameters(
    ir: &mut IrFile,
    function: FunId,
    declaration: &KlibIrFunction,
    header: &FunctionHeader,
    selected: Option<KlibBodyCallable<'_>>,
    is_interface: &mut dyn FnMut(TypeName) -> Result<bool, KlibBodyDeclineReason>,
) -> Result<(), KlibBodyDeclineReason> {
    let Some(selected) = selected else {
        return Err(KlibBodyDeclineReason::UnsupportedOperation(
            "a call of a generic inline function whose type parameters are not selected",
        ));
    };
    let formals = selected.type_parameters();
    if formals.len() != declaration.type_parameters.len()
        || selected
            .reified_type_parameter_ordinals()
            .iter()
            .any(|&ordinal| ordinal as usize >= formals.len())
    {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "the inline function's type parameters are not parallel to its selected formals"
                .to_owned(),
        ));
    }
    let mut type_params = Vec::with_capacity(formals.len());
    for (index, (formal, serialized)) in
        formals.iter().zip(&declaration.type_parameters).enumerate()
    {
        let TyKind::TyParam(semantic, _) = formal.kind() else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(format!(
                "the type parameter `{}` is selected as {formal:?}",
                serialized.name
            )));
        };
        let mut bounds = Vec::new();
        for &bound in header
            .type_parameters()
            .declared_bounds(semantic.as_str())
            .unwrap_or_default()
        {
            // The implicit `Any?` bound is no declared constraint.
            if bound == Ty::nullable(Ty::obj("kotlin/Any")) {
                continue;
            }
            // A bound naming another type parameter names no classifier.
            let interface = match bound.non_null().kotlin_class_internal() {
                Some(classifier) => is_interface(classifier)?,
                None => false,
            };
            bounds.push((bound, interface));
        }
        type_params.push(crate::ir::IrTypeParameter {
            name: serialized.name.clone(),
            semantic_name: semantic.as_str().to_owned(),
            bounds,
            // Kotlin declares no variance on a function's type parameter.
            variance: crate::types::TypeVariance::Invariant,
            reified: selected
                .reified_type_parameter_ordinals()
                .contains(&(index as u32)),
            captured_from_enclosing: false,
        });
    }
    let declared = &ir.functions[function as usize];
    let signature = crate::ir::IrGenericSig {
        type_params,
        params: declared.params.clone(),
        ret: Some(declared.ret),
        supers: Vec::new(),
    };
    ir.signatures.insert(function, signature);
    Ok(())
}

/// Expand every call of an inline function of the unit in `ir`, including the calls a copied body
/// itself makes.
pub(super) fn expand_calls(ir: &mut IrFile) -> Result<(), KlibBodyDeclineReason> {
    let mut sites = Vec::new();
    for site in 0..ir.exprs.len() as ExprId {
        if inline_callee(ir, site)?.is_some() {
            sites.push(site);
        }
    }
    if sites.is_empty() {
        return Ok(());
    }
    let mut expansion = Expansion {
        next_temporary: crate::inline_expansion::fresh_value_base(ir),
        stack: HashSet::default(),
        lambdas: HashMap::default(),
    };
    for site in sites {
        expansion.call(ir, site)?;
    }
    // Every lambda passed for an inlined parameter now runs only as the copies of its body. Its
    // expression stays in the arena, orphaned, with the implementation consumed, as checked FIR
    // lowering leaves a lambda it spliced into an inline caller: no target creates it.
    for lambda in expansion.lambdas.into_values() {
        ir.functions[lambda.function as usize].body = None;
    }
    Ok(())
}

/// The inline function of the unit `site` calls, when it is such a call. A call that leaves
/// arguments to an inline function's defaults is not expanded, so it declines.
fn inline_callee(ir: &IrFile, site: ExprId) -> Result<Option<FunId>, KlibBodyDeclineReason> {
    let IrExpr::Call { callee, .. } = ir.expr(site) else {
        return Ok(None);
    };
    match **callee {
        Callee::Local(function) => Ok(ir.inline_only_fns.contains(&function).then_some(function)),
        Callee::LocalWithDefaults { function, .. } if ir.inline_only_fns.contains(&function) => {
            Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a call of an inline function that leaves arguments to its defaults",
            ))
        }
        _ => Ok(None),
    }
}

/// A lambda passed for an inlined parameter: its implementation and the caller's locals holding
/// the values it captured.
#[derive(Clone)]
struct InlinedLambda {
    function: FunId,
    captures: Vec<u32>,
}

/// The state of the expansions of one unit.
struct Expansion {
    next_temporary: u32,
    /// The inline functions and lambdas whose expansion is in progress.
    stack: HashSet<FunId>,
    /// The local each inlined parameter's lambda stands in, by the slot the copied bodies read.
    lambdas: HashMap<u32, InlinedLambda>,
}

impl Expansion {
    /// Expand the call `site` of an inline function, and then the calls and lambda invocations its
    /// copied body makes.
    fn call(&mut self, ir: &mut IrFile, site: ExprId) -> Result<(), KlibBodyDeclineReason> {
        let Some(function) = inline_callee(ir, site)? else {
            return Ok(());
        };
        let IrExpr::Call {
            dispatch_receiver,
            args,
            ..
        } = ir.expr(site).clone()
        else {
            unreachable!("the site was proved to be a call")
        };
        let modes = ir
            .inline_expansion_modes(function)
            .map(<[_]>::to_vec)
            .ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "an inline function publishes no expansion mode for each parameter".to_owned(),
                )
            })?;
        if modes.len() != args.len() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an inline call's arguments disagree with its function's parameters".to_owned(),
            ));
        }
        let mut statements = Vec::new();
        let mut lambdas = Vec::new();
        let mut operands = dispatch_receiver
            .into_iter()
            .map(InlineOperand::Value)
            .collect::<Vec<_>>();
        for (argument, mode) in args.into_iter().zip(modes) {
            let lambda = match mode {
                InlineExpansionMode::Splice => self.lambda(ir, argument, &mut statements)?,
                InlineExpansionMode::Receiver | InlineExpansionMode::Materialize => None,
            };
            operands.push(match lambda {
                Some(slot) => {
                    lambdas.push(slot);
                    InlineOperand::Slot(slot)
                }
                None => InlineOperand::Value(argument),
            });
        }
        let body = Self::body(ir, function)?;
        let expanded = self.splice(ir, site, function, body, operands, statements)?;
        // Only an invocation of an inlined parameter is expanded; any other use would need the
        // lambda as an object, which the call never creates.
        if lambdas.iter().any(|slot| reads(ir, expanded, *slot)) {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "an inline function's lambda parameter used other than by invoking it",
            ));
        }
        Ok(())
    }

    /// The local an inlined parameter's `argument` stands in, with the declarations of the values
    /// it captures appended to `statements`, when the argument is a lambda.
    fn lambda(
        &mut self,
        ir: &mut IrFile,
        argument: ExprId,
        statements: &mut Vec<ExprId>,
    ) -> Result<Option<u32>, KlibBodyDeclineReason> {
        let lambda = match ir.expr(argument).clone() {
            // A parameter of an enclosing expansion passed on keeps standing in its lambda.
            IrExpr::GetValue(slot) if self.lambdas.contains_key(&slot) => return Ok(Some(slot)),
            IrExpr::Lambda {
                impl_fn,
                captures,
                sam: None,
                ..
            } => {
                let types = ir.functions[impl_fn as usize].params.clone();
                let mut slots = Vec::with_capacity(captures.len());
                for (capture, ty) in captures.into_iter().zip(types) {
                    let slot = self.temporary()?;
                    statements.push(ir.add_expr(IrExpr::Variable {
                        index: slot,
                        ty: stored_value_ty(ty),
                        init: Some(capture),
                        named: false,
                    }));
                    slots.push(slot);
                }
                // The lambda is expanded at each invocation, never created.
                ir.inline_only_fns.insert(impl_fn);
                InlinedLambda {
                    function: impl_fn,
                    captures: slots,
                }
            }
            _ => return Ok(None),
        };
        let slot = self.temporary()?;
        self.lambdas.insert(slot, lambda);
        Ok(Some(slot))
    }

    /// Expand the invocation `site` of the lambda `lambda` from the lambda's body.
    fn invocation(
        &mut self,
        ir: &mut IrFile,
        site: ExprId,
        lambda: InlinedLambda,
        args: Vec<ExprId>,
    ) -> Result<(), KlibBodyDeclineReason> {
        let operands = lambda
            .captures
            .iter()
            .map(|slot| InlineOperand::Slot(*slot))
            .chain(args.into_iter().map(InlineOperand::Value))
            .collect();
        let body = Self::body(ir, lambda.function)?;
        self.splice(ir, site, lambda.function, body, operands, Vec::new())?;
        Ok(())
    }

    /// Replace `site` with the expansion of `function`'s `body` for `operands` after `statements`,
    /// then expand the calls and invocations the copy makes. Returns the expansion.
    fn splice(
        &mut self,
        ir: &mut IrFile,
        site: ExprId,
        function: FunId,
        body: ExprId,
        operands: Vec<InlineOperand>,
        mut statements: Vec<ExprId>,
    ) -> Result<ExprId, KlibBodyDeclineReason> {
        if !self.stack.insert(function) {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a recursive inline function",
            ));
        }
        let bindings = ir
            .inline_call_type_arguments
            .get(&site)
            .map(|arguments| arguments.iter().cloned().collect::<HashMap<_, _>>())
            .unwrap_or_default();
        let reified_bindings = ir
            .reified_call_subst
            .get(&site)
            .map(|arguments| arguments.iter().cloned().collect::<HashMap<_, _>>())
            .unwrap_or_default();
        let spliced = crate::inline_expansion::splice_inline_body(
            ir,
            InlineBodySplice {
                function,
                body,
                operands,
                bindings: &bindings,
                reified_bindings: &reified_bindings,
                statement: false,
                label_prefix: "$klib_inline$",
            },
            &mut self.next_temporary,
        )
        .map_err(|failure| {
            KlibBodyDeclineReason::SignatureMismatch(format!(
                "an inline function body cannot be expanded at its call: {failure:?}"
            ))
        })?;
        statements.extend(spliced.statements);
        let value = match spliced.produced {
            SplicedResult::Value(value) => value,
            SplicedResult::Statement(statement) => {
                statements.push(statement);
                ir.add_expr(IrExpr::UnitInstance)
            }
        };
        ir.exprs[site as usize] = IrExpr::Block {
            stmts: statements,
            value: Some(value),
        };
        ir.inline_regions.insert(site);
        let mut copies = spliced.copies.into_values().collect::<Vec<_>>();
        copies.sort_unstable();
        for copy in copies {
            if inline_callee(ir, copy)?.is_some() {
                self.call(ir, copy)?;
            } else if let Some((lambda, args)) = self.invoked(ir, copy) {
                self.invocation(ir, copy, lambda, args)?;
            }
        }
        self.stack.remove(&function);
        Ok(site)
    }

    /// The inlined lambda `site` invokes, with the invocation's arguments.
    fn invoked(&self, ir: &IrFile, site: ExprId) -> Option<(InlinedLambda, Vec<ExprId>)> {
        let IrExpr::InvokeFunction { func, args, .. } = ir.expr(site) else {
            return None;
        };
        let IrExpr::GetValue(slot) = ir.expr(*func) else {
            return None;
        };
        Some((self.lambdas.get(slot)?.clone(), args.clone()))
    }

    fn body(ir: &IrFile, function: FunId) -> Result<ExprId, KlibBodyDeclineReason> {
        ir.functions
            .get(function as usize)
            .and_then(|function| function.body)
            .ok_or(KlibBodyDeclineReason::NoBody)
    }

    fn temporary(&mut self) -> Result<u32, KlibBodyDeclineReason> {
        let slot = self.next_temporary;
        self.next_temporary = slot.checked_add(1).ok_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "an inline expansion overflows the caller's locals".to_owned(),
            )
        })?;
        Ok(slot)
    }
}

/// Whether the tree under `root` reads the local `slot`.
fn reads(ir: &IrFile, root: ExprId, slot: u32) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if matches!(ir.expr(expression), IrExpr::GetValue(read) if *read == slot) {
            return true;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
}

/// Whether the selected classifier `name` is an interface: the supertype of an object literal an
/// inline callee creates.
pub(super) fn is_interface(
    linker: &super::Linker<'_, '_>,
    name: TypeName,
) -> Result<bool, KlibBodyDeclineReason> {
    let record = linker.sources.declarations.classifier(name);
    record.map(|record| record.is_interface()).ok_or_else(|| {
        KlibBodyDeclineReason::SignatureMismatch(format!(
            "an object literal's supertype {name:?} is not a selected classifier"
        ))
    })
}
