//! Which locals a suspension snapshots, and which of them are still live when it runs.
//!
//! kotlinc's positional-spill model (docs/POSITIONAL_SPILLS.md) is a LEXICAL rule for named
//! variables and a LIVENESS rule for compiler temporaries, and the two are decided by one walk of
//! the final body. Keeping that walk in a module of its own is what lets the rule be read in one
//! place rather than inferred from the transform that consumes it.

use super::*;

/// Per-suspension lexical scope snapshots (kotlinc's positional-spill model, see
/// docs/POSITIONAL_SPILLS.md): one walk of the FINAL (post-hoist) body, tracking the eligible
/// locals in scope; every suspend-call expression id maps to `params ++ in-scope entries` in
/// declaration order. A NAMED local is included whenever lexically in scope (kotlinc's scope
/// rule); an unnamed TEMP only while LIVE — some read of it can still run after the suspension
/// (the `pending` context: every not-yet-walked statement of the enclosing lists, plus the whole
/// enclosing loops, which re-run their subtrees on the back-edge). A catch variable remapped to a
/// spill local (`catch_spills`: cvar → ev) is in scope inside its catch body (named — it must
/// survive its body's own suspensions).
pub(super) struct ScopeWalk<'a> {
    pub(super) ir: &'a IrFile,
    pub(super) suspend_set: &'a HashSet<u32>,
    pub(super) params: &'a [(u32, Ty)],
    pub(super) scope: Vec<ScopeEntry>,
    /// Exprs that may still execute after the current walk point (rest-of-list statements, whole
    /// enclosing loops).
    pub(super) pending: Vec<ExprId>,
    /// Start index in `pending` of each nesting level, outermost first. `pending` grows one
    /// contiguous block per level, so the levels run INSIDE-OUT: the last block executes first.
    pub(super) levels: Vec<usize>,
    /// Snapshot ONLY the unnamed temps that are LIVE at each suspension, with no parameter prefix —
    /// the complementary pass run over the FINAL body (see [`live_temp_scopes`]). The default (false)
    /// snapshots the parameter prefix plus the NAMED locals in scope.
    pub(super) temps_only: bool,
    pub(super) out: SuspensionScopes,
}

/// Per-suspension lists of the unnamed TEMPS live across each suspension, walked over the FINAL
/// (post-splice, post-hoist) body. The named-by-scope lists are captured PRE-SPLICE — before
/// `hoist_suspensions` even creates the temps that a multi-suspension expression (`a() + b()`) needs —
/// so the temps can only be collected here, once the body has its final shape. Merged into the
/// captured lists by [`merge_live_temps`].
pub(super) fn live_temp_scopes(
    ir: &IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
) -> SuspensionScopes {
    let mut w = ScopeWalk {
        ir,
        suspend_set,
        params: &[],
        scope: Vec::new(),
        pending: Vec::new(),
        levels: Vec::new(),
        temps_only: true,
        out: Default::default(),
    };
    w.walk(body);
    w.out
}

/// Append each suspension's live temps to its captured scope list. Only suspensions the capture
/// already knows are extended: a list's PARAMETER prefix comes from the capture, and a state whose
/// restores would silently lose it must keep behaving as before.
pub(super) fn merge_live_temps(scopes: &mut SuspensionScopes, temps: SuspensionScopes) {
    for (call, temp_scope) in temps {
        let Some(scope) = scopes.get_mut(&call) else {
            continue;
        };
        for entry in temp_scope.values {
            if !scope.values.iter().any(|e| e.0 == entry.0) {
                scope.values.push(entry);
            }
        }
    }
}

/// Make the machine-local allocation set agree with the positional lists it will actually store and
/// restore. Scope capture deliberately includes every named local in scope, while the earlier liveness
/// analysis finds locals whose values are read across a suspension; either route makes a local a real
/// spill consumer. Parameters/captures have no declaration in `body` and retain their dedicated entry
/// handling, so only body-local declarations are added here. Centralizing the reconciliation keeps the
/// named-function and suspend-lambda machines consistent and avoids operand/block-specific exceptions.
pub(super) fn reconcile_positional_spill_locals(
    ir: &IrFile,
    body: ExprId,
    scopes: &SuspensionScopes,
    live_calls: &HashSet<ExprId>,
    spilled: &mut Vec<(u32, Ty)>,
) {
    for (call, scope) in scopes {
        if !live_calls.contains(call) {
            continue;
        }
        for &(local, _) in &scope.values {
            if spilled.iter().any(|(existing, _)| *existing == local) {
                continue;
            }
            if let Some(ty) = find_local_ty(ir, body, local) {
                spilled.push((local, spill_field_ty(ty)));
            }
        }
    }
    spilled.sort_by_key(|(local, _)| *local);
    spilled.dedup_by_key(|(local, _)| *local);
}

/// One lexically in-scope local of a [`ScopeWalk`]. `named` distinguishes a source variable (always
/// snapshotted while in scope, kotlinc's rule) from a compiler TEMP (snapshotted only while live).
pub(super) struct ScopeEntry {
    pub(super) slot: u32,
    pub(super) ty: Ty,
    pub(super) name: Option<String>,
    pub(super) named: bool,
}

impl ScopeWalk<'_> {
    fn push_decl(&mut self, st: ExprId) {
        if let IrExpr::Variable {
            index,
            ty,
            init,
            named,
        } = self.ir.exprs[st as usize]
        {
            if !self.scope.iter().any(|e| e.slot == index) {
                // Every NAMED source variable participates (kotlinc's scope rule; kind decides the
                // field family: references L$, ints I$, …). An unnamed TEMP is admitted too, but
                // `snapshot` filters it by liveness.
                self.scope.push(ScopeEntry {
                    slot: index,
                    ty: spill_field_ty(local_storage_ty(self.ir, ty, init)),
                    name: crate::jvm::debug_local_names::name(self.ir, st),
                    named,
                });
            }
        }
    }
    fn snapshot(&mut self, call: ExprId) {
        let mut snapshot = SuspensionScope {
            values: if self.temps_only {
                Vec::new()
            } else {
                self.params.to_vec()
            },
            names: std::collections::HashMap::new(),
        };
        // NAMED vars spill by SCOPE (kotlinc's rule) — every splice-materialization local kotlinc
        // names is emitted `named` at its lowering site, so scope and liveness agree for them.
        // An unnamed TEMP spills by LIVENESS instead: it is a materialized operand, and kotlinc
        // likewise gives a live operand a field (`a() + b()` stores its partial `StringBuilder` in
        // `L$0` and restores it in every later arm). Admitting a DEAD temp would only inflate the
        // per-kind field maxima past kotlinc's; admitting none at all leaves a temp that genuinely
        // crosses a suspension spilled but never restored (a null/`Bad local variable type` arm).
        // (kotlinc's `nullOutSpilledVariable` stores are FIELD HYGIENE — clearing a previous
        // state's spill of a now-dead var — and never allocate positions beyond some state's live
        // list.)
        // An unnamed TEMP that is still read after this point CONSUMES a spill position even though it
        // contributes no name: kotlinc spills a loop's iterator into its own `L$N` between the locals
        // declared before and after the loop opens, so the following named local's field number skips
        // over it. Dropping the temp here compacted those numbers and made every suspending loop's `s`
        // array disagree with kotlinc's.
        let live: Vec<(u32, Ty, Option<String>)> = self
            .scope
            .iter()
            .filter(|e| {
                if self.temps_only {
                    !e.named && self.pending_reads(e.slot)
                } else {
                    e.named || self.pending_reads(e.slot)
                }
            })
            .map(|e| (e.slot, e.ty, e.name.clone()))
            .collect();
        for (slot, ty, name) in live {
            snapshot.values.push((slot, ty));
            if let Some(name) = name {
                snapshot.names.insert(slot, name);
            }
        }
        self.out.insert(call, snapshot);
    }
    /// Whether any expression that may still execute after the current walk point reads `slot` — the
    /// liveness test for an unnamed temp (rest-of-list statements, plus whole enclosing loops, which
    /// re-run their subtrees on the back-edge).
    fn pending_reads(&self, slot: u32) -> bool {
        pending_reads_after(self.ir, &self.pending, &self.levels, slot, self.suspend_set)
    }
    pub(super) fn walk_stmts(&mut self, stmts: &[ExprId]) {
        let base = self.scope.len();
        for (i, &st) in stmts.iter().enumerate() {
            let pbase = self.pending.len();
            self.pending.extend_from_slice(&stmts[i + 1..]);
            self.levels.push(pbase);
            self.walk(st);
            self.levels.pop();
            self.pending.truncate(pbase);
            self.push_decl(st);
        }
        self.close_scope(base);
    }
    /// Close the lexical scopes above `base`.
    fn close_scope(&mut self, base: usize) {
        self.scope.truncate(base);
    }
    pub(super) fn walk(&mut self, e: ExprId) {
        if is_suspension_point(self.ir, e, self.suspend_set) {
            self.snapshot(e);
        }
        match self.ir.exprs[e as usize].clone() {
            IrExpr::Block { stmts, value } => {
                let base = self.scope.len();
                if let Some(v) = value {
                    let pbase = self.pending.len();
                    self.pending.push(v);
                    self.levels.push(pbase);
                    self.walk_stmts(&stmts);
                    self.levels.pop();
                    self.pending.truncate(pbase);
                    // The trailing value sees the block's declarations.
                    for &st in &stmts {
                        self.push_decl(st);
                    }
                    self.walk(v);
                } else {
                    self.walk_stmts(&stmts);
                }
                self.close_scope(base);
            }
            IrExpr::When { branches } => {
                for (c, body) in branches {
                    if let Some(c) = c {
                        self.walk(c);
                    }
                    let base = self.scope.len();
                    self.walk(body);
                    self.close_scope(base);
                }
            }
            IrExpr::While {
                cond, body, update, ..
            } => {
                // Inside the loop, its WHOLE subtree may re-run on the back-edge.
                let pbase = self.pending.len();
                self.pending.push(e);
                self.levels.push(pbase);
                self.walk(cond);
                let base = self.scope.len();
                self.walk(body);
                if let Some(u) = update {
                    self.walk(u);
                }
                self.close_scope(base);
                self.levels.pop();
                self.pending.truncate(pbase);
            }
            IrExpr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                let base = self.scope.len();
                self.walk(body);
                self.close_scope(base);
                for c in catches {
                    let base = self.scope.len();
                    // The catch variable is in scope (named) for the catch body. When the body
                    // suspends, the machine builder remaps it to a fresh exception-spill local and
                    // REWRITES the captured lists to that index (`catch_spills` remap below).
                    if !self.scope.iter().any(|e| e.slot == c.var) {
                        self.scope.push(ScopeEntry {
                            slot: c.var,
                            ty: Ty::obj(&c.exc_internal.render()),
                            // A spilled catch parameter is named through the same debug-local
                            // boundary as the local variable table's entry for it, so a spliced
                            // expansion's copy carries its inline depth in both.
                            name: c.binding.as_ref().and_then(|binding| {
                                crate::jvm::debug_local_names::render(
                                    self.ir,
                                    Some(binding.name.as_str()),
                                    binding.provenance,
                                )
                            }),
                            named: true,
                        });
                    }
                    self.walk(c.body);
                    self.close_scope(base);
                }
                if let Some(f) = finally {
                    self.walk(f);
                }
            }
            IrExpr::Variable {
                init: Some(init), ..
            } => {
                self.walk(init);
            }
            _ => {
                let mut children: Vec<ExprId> = Vec::new();
                for_each_child(&self.ir.exprs, e, &mut |c| children.push(c));
                for c in children {
                    self.walk(c);
                }
            }
        }
    }
}
