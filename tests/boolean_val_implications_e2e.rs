//! Smart casts through a stable local `Boolean` `val`.
//!
//! Kotlin binds what a condition proves to the stable Boolean variable that holds its value:
//! `val ok = x != null; if (ok) x.length` narrows `x` exactly as `if (x != null)` would, and a
//! contract applies through the variable too (`val empty = s.isNullOrEmpty()` proves `s` non-null
//! when `empty` is false). A write to the value a fact talks about drops that fact, and the fact
//! is still subject to the ordinary stability rules where the variable is read.
//!
//! Every accepted and rejected form is compared with kotlinc's complete error ledger.

use super::common;

/// Each form kotlinc accepts: the plain null check, a contract through a negated `||`, an `is`
/// test, a val copied into another val, an early return, a `when` branch, an `&&` operand, a local
/// `var` root that is not written again, reads inside an inline lambda, a non-inline lambda and a
/// local function, and a member call guarded by a contract through the val. `andOperand` has an
/// inferred result type, so the declaration's signature pass reads the val as well.
#[test]
fn a_stable_boolean_val_carries_its_conditions_facts_like_kotlinc() {
    common::assert_accepted_like_kotlinc(
        "fun nullCheck(x: String?) { val ok = x != null; if (ok) x.length }\n\
         fun contract(x: String?, y: String?) {\n\
         \x20   val use = !y.isNullOrEmpty() || x == null\n\
         \x20   if (use) {} else x.length\n\
         }\n\
         fun negatedContract(y: String?) { val empty = y.isNullOrEmpty(); if (!empty) y.length }\n\
         fun typeTest(x: Any) { val text = x is String; if (text) x.length }\n\
         fun negatedTypeTest(x: Any) { val other = x !is String; if (!other) x.length }\n\
         fun both(x: String?, y: String?) { val ok = x != null && y != null; if (ok) { x.length; y.length } }\n\
         fun copied(x: String?) { val ok = x != null; val again = ok; if (again) x.length }\n\
         fun earlyReturn(x: String?) { val missing = x == null; if (missing) return; x.length }\n\
         fun whenBranch(x: String?) { val ok = x != null; when { ok -> x.length } }\n\
         fun andOperand(x: String?) = run { val ok = x != null; ok && x.length > 0 }\n\
         fun copiedSignature(x: String?) = run { val ok = x != null; val again = ok; if (again) x.length else 0 }\n\
         fun shadowedAliasSignature(x: String?) = run {\n\
         \x20   val ok = x != null\n\
         \x20   run { val ok = ok; if (ok) x.length else 0 }\n\
         }\n\
         fun shadowedRootSignature(x: Any) = run {\n\
         \x20   val text = x is String\n\
         \x20   run { val x = 1; if (text) x else 2 }\n\
         }\n\
         fun consumeShadowedRootSignature() = shadowedRootSignature(\"\") + 1\n\
         fun unrelatedAssignment(x: String?) = run { val ok = x != null; var n = 0; n = 1; if (ok) x.length else n }\n\
         fun unrelatedConditionWrite(x: String?) = run {\n\
         \x20   var gate = true\n\
         \x20   val ok = x != null && gate\n\
         \x20   gate = false\n\
         \x20   if (ok) x.length else 0\n\
         }\n\
         fun oneChangedRootKeepsTheOther(x: String?, y: String?) = run {\n\
         \x20   var a = x\n\
         \x20   val both = a != null && y != null\n\
         \x20   a = null\n\
         \x20   if (both) y.length else 0\n\
         }\n\
         fun typedSignature(x: String?) = run { val ok: Boolean = x != null; if (ok) x.length else 0 }\n\
         fun falseSignature(x: String?) = run { val missing = x == null; if (missing) 0 else x.length }\n\
         fun negatedFalseSignature(x: String?) = run { val ok = x != null; if (!ok) 0 else x.length }\n\
         fun whenSignature(x: String?) = run { val ok = x != null; when { ok -> x.length; else -> 0 } }\n\
         fun projectedSignature(x: String?) = run { val missing = x == null; if (missing == false) x.length else 0 }\n\
         fun subjectWhenSignature(x: String?) = run { val ok = x != null; when (ok) { true -> x.length; false -> 0 } }\n\
         fun whenElseSignature(x: String?) = run { val missing = x == null; when { missing -> 0; else -> x.length } }\n\
         fun whenBranchMutationDoesNotLeak(x: String?) = run {\n\
         \x20   var y = x\n\
         \x20   val missing = y == null\n\
         \x20   when { missing -> { y = null; 0 }; else -> y.length }\n\
         }\n\
         fun returningIfBranchDoesNotEraseFacts(x: String?, stop: Boolean) = run {\n\
         \x20   var y = x\n\
         \x20   val ok = y != null\n\
         \x20   if (stop) { y = null; return@run 0 }\n\
         \x20   if (ok) y.length else 0\n\
         }\n\
         fun returningWhenBranchDoesNotEraseFacts(x: String?, stop: Boolean) = run {\n\
         \x20   var y = x\n\
         \x20   val ok = y != null\n\
         \x20   when { stop -> { y = null; return@run 0 }; else -> {} }\n\
         \x20   if (ok) y.length else 0\n\
         }\n\
         fun catchMutationDoesNotLeak(x: String?) = run {\n\
         \x20   var y = x\n\
         \x20   val ok = y != null\n\
         \x20   try { 0 }\n\
         \x20   catch (e: IllegalArgumentException) { y = null; 0 }\n\
         \x20   catch (e: Throwable) { if (ok) y.length else 0 }\n\
         }\n\
         fun returningCatchDoesNotEraseFacts(x: String?) = run {\n\
         \x20   var y = x\n\
         \x20   val ok = y != null\n\
         \x20   try { 0 } catch (e: Exception) { y = null; return@run 0 }\n\
         \x20   if (ok) y.length else 0\n\
         }\n\
         fun nothingCallBranchDoesNotEraseFacts(x: String?, stop: Boolean) = run {\n\
         \x20   var y = x\n\
         \x20   val ok = y != null\n\
         \x20   if (stop) { y = null; error(\"stop\") }\n\
         \x20   if (ok) y.length else 0\n\
         }\n\
         fun finallyWriteDropsFact(x: String?) = run {\n\
         \x20   var y = x\n\
         \x20   val ok = y != null\n\
         \x20   try { 0 } finally { y = null }\n\
         \x20   if (ok) y else \"\"\n\
         }\n\
         sealed class SignatureKey\n\
         class SignatureA(val size: Int) : SignatureKey()\n\
         class SignatureB(val size: Int) : SignatureKey()\n\
         val SignatureKey.smartSize get() = when (this) { is SignatureA -> size; is SignatureB -> size }\n\
         fun consumeSmartSize(key: SignatureKey) = key.smartSize + 1\n\
         fun assignmentRhsSignature(x: String?) = run {\n\
         \x20   var y = x\n\
         \x20   val ok = y != null\n\
         \x20   y = if (ok) y.trim() else \"\"\n\
         \x20   y\n\
         }\n\
         fun loopShadow(x: String?, cond: Boolean) {\n\
         \x20   val ok = x != null\n\
         \x20   while (cond) { var x: String? = null; x = null; break }\n\
         \x20   if (ok) x.length\n\
         }\n\
         fun doWhileShadow(x: String?) {\n\
         \x20   val ok = x != null\n\
         \x20   do { var x = 0 } while (x++ == 0)\n\
         \x20   if (ok) x.length\n\
         }\n\
         open class CompoundBase { operator fun plusAssign(value: Int) {} }\n\
         class CompoundDerived : CompoundBase() { fun derived(): Int = 1 }\n\
         fun inPlaceCompound(cond: Boolean) {\n\
         \x20   val x: CompoundBase = CompoundDerived()\n\
         \x20   val ok = x is CompoundDerived\n\
         \x20   while (cond) { x += 1 }\n\
         \x20   if (ok) x.derived()\n\
         }\n\
         fun mutableInPlaceCompound(cond: Boolean) {\n\
         \x20   var x: CompoundBase = CompoundDerived()\n\
         \x20   val ok = x is CompoundDerived\n\
         \x20   while (cond) { x += 1 }\n\
         \x20   if (ok) x.derived()\n\
         }\n\
         fun inferredMutableInPlaceCompound(cond: Boolean) = run {\n\
         \x20   var x: CompoundBase = CompoundDerived()\n\
         \x20   val ok = x is CompoundDerived\n\
         \x20   while (cond) { x += 1 }\n\
         \x20   if (ok) x.derived() else 0\n\
         }\n\
         fun selectedInPlaceCompoundReleasesLoopRoot(cond: Boolean) {\n\
         \x20   var x: CompoundBase = CompoundDerived()\n\
         \x20   val ok = x is CompoundDerived\n\
         \x20   while (cond) { x += 1; if (ok) x.derived(); break }\n\
         }\n\
         fun unreachableTryBodyWriteKeepsCatchFact(x: String?) = run {\n\
         \x20   var y = x\n\
         \x20   val ok = y != null\n\
         \x20   try { throw IllegalArgumentException(); y = null }\n\
         \x20   catch (e: IllegalArgumentException) { if (ok) y.length else 0 }\n\
         }\n\
         fun inPlaceCompoundKeepsCatchFact() = run {\n\
         \x20   var x: CompoundBase = CompoundDerived()\n\
         \x20   val ok = x is CompoundDerived\n\
         \x20   try { x += 1 }\n\
         \x20   catch (e: Throwable) { if (ok) x.derived() else 0 }\n\
         }\n\
         class CompoundContext\n\
         context(marker: CompoundContext)\n\
         operator fun CompoundBase.plusAssign(value: Long) {}\n\
         context(marker: CompoundContext)\n\
         fun contextualInPlaceCompound() = run {\n\
         \x20   var x: CompoundBase = CompoundDerived()\n\
         \x20   val ok = x is CompoundDerived\n\
         \x20   x += 1L\n\
         \x20   if (ok) x.derived() else 0\n\
         }\n\
         fun localInPlaceCompound() = run {\n\
         \x20   operator fun CompoundBase.plusAssign(value: Long) {}\n\
         \x20   var x: CompoundBase = CompoundDerived()\n\
         \x20   val ok = x is CompoundDerived\n\
         \x20   x += 1L\n\
         \x20   if (ok) x.derived() else 0\n\
         }\n\
         sealed class ProjectionKey\n\
         class ProjectionA : ProjectionKey()\n\
         class ProjectionB : ProjectionKey()\n\
         fun projectedSavedExclusion(key: ProjectionKey) = run {\n\
         \x20   val isA = key is ProjectionA\n\
         \x20   if (isA == true) return@run 0\n\
         \x20   when (key) { is ProjectionB -> 1 }\n\
         }\n\
         fun localClassShadow(cond: Boolean) {\n\
         \x20   val x: CompoundBase = CompoundDerived()\n\
         \x20   val ok = x is CompoundDerived\n\
         \x20   while (cond) { class Local(var x: Int) { fun mutate() { x++ } }; break }\n\
         \x20   if (ok) x.derived()\n\
         }\n\
         fun unwrittenVar(x: String?) { var y = x; val ok = y != null; if (ok) y.length }\n\
         fun inlineLambda(x: String?) { val ok = x != null; run { if (ok) x.length } }\n\
         fun lambda(x: String?) { val ok = x != null; val f = { if (ok) x.length }; f() }\n\
         fun localFunction(x: String?) { val ok = x != null; fun local() { if (ok) x.length }; local() }\n\
         class Owner<T> {\n\
         \x20   private fun notify(xs: List<T>) {}\n\
         \x20   fun use(xs: List<T>?) { val ok = !xs.isNullOrEmpty(); if (ok) notify(xs) }\n\
         }\n",
    );
}

/// Each form kotlinc rejects: a `var` holds no implications, a write to the root drops the fact
/// (unconditionally or on one branch), a property is not a local, a closure that mutates the root
/// after the val makes the cast impossible where the val is read, a closure that already mutates it
/// where the val is declared leaves no fact at all, and `||` proves nothing about either operand.
#[test]
fn an_unstable_boolean_or_root_proves_nothing_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            "fun mutableFlag(x: String?) { var ok = x != null; if (ok) x.length }\n\
             fun rewritten(x: String?, w: String?) { var y = x; val ok = y != null; y = w; if (ok) y.length }\n\
             fun rewrittenOnABranch(x: String?, w: String?) {\n\
             \x20   var y = x\n\
             \x20   val ok = y != null\n\
             \x20   if (w != null) y = w\n\
             \x20   if (ok) y.length\n\
             }\n\
             class Holder(val ok: Boolean)\n\
             fun property(x: String?) { val holder = Holder(x != null); if (holder.ok) x.length }\n\
             class Member(val x: String?) { val ok = x != null; fun use() { if (ok) x.length } }\n\
             fun closureWrites(x: String?) {\n\
             \x20   var y = x\n\
             \x20   val ok = y != null\n\
             \x20   val reset = { y = null }\n\
             \x20   if (ok) y.length\n\
             \x20   reset()\n\
             }\n\
             fun closureFirst(x: String?) {\n\
             \x20   var y = x\n\
             \x20   val reset = { y = null }\n\
             \x20   val ok = y != null\n\
             \x20   if (ok) y.length\n\
             \x20   reset()\n\
             }\n\
             fun localFunctionWrite(x: String?) {\n\
             \x20   var y = x\n\
             \x20   val ok = y != null\n\
             \x20   fun reset() { y = null }\n\
             \x20   if (ok) y.length\n\
             \x20   reset()\n\
             }\n\
             fun guardedWhen(x: String?, g: Boolean) = run {\n\
             \x20   val missing = x == null\n\
             \x20   when { missing if g -> \"\"; else -> x }\n\
             }\n\
             fun consumeGuardedWhen(): String = guardedWhen(null, false)\n\
             fun either(x: String?, b: Boolean) { val ok = x != null || b; if (ok) x.length }\n\
             fun shadowedRoot(x: String?) {\n\
             \x20   val ok = x != null\n\
             \x20   run { val x: String? = null; if (ok) x.length }\n\
             }\n\
             open class LoopBase\n\
             class LoopDerived : LoopBase() { fun derived() {} }\n\
             open class PlusBase { operator fun plus(value: Int): PlusBase = PlusBase() }\n\
             class PlusDerived : PlusBase() { fun derived() {} }\n\
             fun replacingCompoundDropsFact(cond: Boolean) {\n\
             \x20   var x: PlusBase = PlusDerived()\n\
             \x20   val ok = x is PlusDerived\n\
             \x20   while (cond) { x += 1 }\n\
             \x20   if (ok) x.derived()\n\
             }\n\
             fun replacingCompoundInvalidatesBackEdge(cond: Boolean) {\n\
             \x20   var x: PlusBase = PlusDerived()\n\
             \x20   val ok = x is PlusDerived\n\
             \x20   while (cond) { if (ok) x.derived(); x += 1 }\n\
             }\n\
             fun localFunctionKeepsValueBinding(cond: Boolean) {\n\
             \x20   var x: LoopBase = LoopDerived()\n\
             \x20   while (cond) {\n\
             \x20       fun x() {}\n\
             \x20       x.derived()\n\
             \x20       x = LoopBase()\n\
             \x20   }\n\
             }\n\
             fun explicitZeroParameterLambdaKeepsIt(cond: Boolean) {\n\
             \x20   var it: LoopBase = LoopDerived()\n\
             \x20   while (cond) {\n\
             \x20       it.derived()\n\
             \x20       run { -> it = LoopBase() }\n\
             \x20   }\n\
             }\n\
             fun primaryDefaultKeepsOuterBinding(cond: Boolean) {\n\
             \x20   var x: LoopBase = LoopDerived()\n\
             \x20   while (cond) {\n\
             \x20       x.derived()\n\
             \x20       class Local(val x: LoopBase = run { x = LoopBase(); x })\n\
             \x20       Local()\n\
             \x20   }\n\
             }\n\
             fun localFunctionDefaultKeepsOuterBinding(cond: Boolean) {\n\
             \x20   var x: LoopBase = LoopDerived()\n\
             \x20   while (cond) {\n\
             \x20       x.derived()\n\
             \x20       fun reset(x: LoopBase = run { x = LoopBase(); x }) {}\n\
             \x20       reset()\n\
             \x20   }\n\
             }\n\
             fun localClassMethodDefaultKeepsOuterBinding(cond: Boolean) {\n\
             \x20   var x: LoopBase = LoopDerived()\n\
             \x20   while (cond) {\n\
             \x20       x.derived()\n\
             \x20       class Local {\n\
             \x20           fun reset(x: LoopBase = run { x = LoopBase(); x }) {}\n\
             \x20       }\n\
             \x20       Local().reset()\n\
             \x20   }\n\
             }\n\
             fun abruptTryExitInvalidatesFinally(x: String?, stop: Boolean) = run {\n\
             \x20   var y = x\n\
             \x20   val ok = y != null\n\
             \x20   try {\n\
             \x20       if (stop) { y = null; return@run 0 }\n\
             \x20       0\n\
             \x20   } finally {\n\
             \x20       if (ok) y.length\n\
             \x20   }\n\
             }\n",
        )],
        &[],
    );
}

/// The narrowed reads run: each guarded branch takes the value the condition proved.
#[test]
fn a_cast_through_a_boolean_val_runs() {
    let source = "fun length(x: String?): Int { val ok = x != null; return if (ok) x.length else -1 }\n\
         fun contract(y: String?): Int { val empty = y.isNullOrEmpty(); return if (!empty) y.length else 0 }\n\
         fun box(): String =\n\
         \x20   if (length(\"abc\") == 3 && length(null) == -1 && contract(\"de\") == 2 && contract(null) == 0) \"OK\"\n\
         \x20   else \"FAIL\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "Main").as_deref(),
        Some("OK")
    );
}
