//! A `companion object` whose declared supertype carries TYPE ARGUMENTS (`companion object :
//! CoroutineContext.Key<Elem>`): the companion's supertype list used to register as BARE internal
//! names, so a generic call like `operator fun <E : Element> get(key: Key<E>): E?` saw the companion
//! as a raw `Key` and fell back to `E`'s upper bound (`return type mismatch: expected 'Elem?',
//! actual 'Element?'`). The companion's supertypes now keep their `TypeRef`s end-to-end (like a
//! regular class's), and generic-call binding walks the argument's APPLIED supertypes, so the type
//! argument binds.
//!
//! Found on intellij-community's ActionUtil.kt (`currentThreadContext()[ActionContextElement]`).
//!
//! Box-run note: the exact stdlib shape (`class Elem : AbstractCoroutineContextElement(Elem)`) hits
//! TWO pre-existing, unrelated IR-backend gates (subclassing an abstract classpath base with abstract
//! obligations; a companion implementing a classpath interface directly), so the runtime tests mirror
//! it with an emittable same-file key interface and pin the exact repro on the front end.

use super::common;

/// Pin the front end: the source compiles without diagnostics (the fix's checker half). Returns the
/// diagnostics for message-level assertions.
fn checker_diags(src: &str) -> Vec<String> {
    common::checker_diags_with_stdlib(src).expect("stdlib toolchain provisioned")
}

#[test]
fn companion_key_type_arg_binds_get_frontend() {
    // The EXACT /tmp/fix21.kt repro: `c[Elem]` types as `Elem?` — previously `return type mismatch:
    // expected 'Elem?', actual 'coroutines.CoroutineContext.Element?'.` at 8:40. (Emission is gated
    // by the pre-existing IR limits noted in the module doc, so this pins the front end.)
    const SRC: &str = "import kotlin.coroutines.AbstractCoroutineContextElement\n\
import kotlin.coroutines.CoroutineContext\n\
\n\
class Elem(val id: String) : AbstractCoroutineContextElement(Elem), CoroutineContext.Element {\n\
    companion object : CoroutineContext.Key<Elem>\n\
}\n\
\n\
fun test(c: CoroutineContext): Elem? = c[Elem]\n\
\n\
fun box(): String = \"OK\"\n";
    assert_eq!(checker_diags(SRC), Vec::<String>::new());
}

#[test]
fn companion_key_type_arg_binds_get() {
    // The `context[Elem]` shape through a REAL CoroutineContext: the companion implements a same-file
    // `Key` marker (a `CoroutineContext.Key<Elem>` subinterface — the shape the backend emits), and
    // `get`'s `E` binds `Elem` through the companion's supertype type argument. `Elem` overrides
    // `get`/`fold` explicitly: the backend doesn't generate Kotlin-DefaultImpls bridges for classpath
    // interfaces, an unrelated pre-existing limit.
    const SRC: &str = "import kotlin.coroutines.CoroutineContext\n\
\n\
interface ElemKey : CoroutineContext.Key<Elem>\n\
\n\
class Elem(val id: String) : CoroutineContext.Element {\n\
    override val key: CoroutineContext.Key<*> get() = Elem\n\
    override fun <E : CoroutineContext.Element> get(key: CoroutineContext.Key<E>): E? =\n\
        if (key == this.key) this as E else null\n\
    override fun <R> fold(initial: R, operation: (R, CoroutineContext.Element) -> R): R =\n\
        operation(initial, this)\n\
    companion object : ElemKey\n\
}\n\
\n\
fun box(): String {\n\
    val ctx: CoroutineContext = Elem(\"x\")\n\
    val e: Elem? = ctx[Elem]\n\
    return if (e?.id == \"x\") \"OK\" else \"F\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "CompanionKeyGet");
}

#[test]
fn companion_key_type_arg_binds_plus_and_minus_key() {
    // `ctx + Elem("y")` then `[Elem]` returns the replacement element; `minusKey(Elem)` removes it.
    // (`PairContext` stands in for the stdlib-internal `CombinedContext`; `plus`/`minusKey` are
    // overridden for the same DefaultImpls reason as above.)
    const SRC: &str = "import kotlin.coroutines.CoroutineContext\n\
import kotlin.coroutines.EmptyCoroutineContext\n\
\n\
interface ElemKey : CoroutineContext.Key<Elem>\n\
\n\
class PairContext(val l: CoroutineContext, val e: CoroutineContext.Element) : CoroutineContext {\n\
    override fun <E : CoroutineContext.Element> get(key: CoroutineContext.Key<E>): E? = e[key] ?: l[key]\n\
    override fun <R> fold(initial: R, operation: (R, CoroutineContext.Element) -> R): R =\n\
        operation(l.fold(initial, operation), e)\n\
    override fun plus(context: CoroutineContext): CoroutineContext =\n\
        if (context === EmptyCoroutineContext) this else PairContext(this, context as Elem)\n\
    override fun minusKey(key: CoroutineContext.Key<*>): CoroutineContext {\n\
        if (e.key == key) return l.minusKey(key)\n\
        val newL = l.minusKey(key)\n\
        return if (newL === l) this else PairContext(newL, e)\n\
    }\n\
}\n\
\n\
class Elem(val id: String) : CoroutineContext.Element {\n\
    override val key: CoroutineContext.Key<*> get() = Elem\n\
    override fun <E : CoroutineContext.Element> get(key: CoroutineContext.Key<E>): E? =\n\
        if (key == this.key) this as E else null\n\
    override fun <R> fold(initial: R, operation: (R, CoroutineContext.Element) -> R): R =\n\
        operation(initial, this)\n\
    override fun plus(context: CoroutineContext): CoroutineContext =\n\
        if (context === EmptyCoroutineContext) this else PairContext(this, context as Elem)\n\
    override fun minusKey(key: CoroutineContext.Key<*>): CoroutineContext =\n\
        if (this.key == key) EmptyCoroutineContext else this\n\
    companion object : ElemKey\n\
}\n\
\n\
fun box(): String {\n\
    val a: CoroutineContext = Elem(\"x\")\n\
    val ctx: CoroutineContext = a + Elem(\"y\")\n\
    val e: Elem? = ctx[Elem]\n\
    if (e?.id != \"y\") return \"F1\"\n\
    val rest: CoroutineContext = ctx.minusKey(Elem)\n\
    return if (rest[Elem] == null) \"OK\" else \"F2\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "CompanionKeyPlusMinus");
}

#[test]
fn companion_supertype_type_arg_binds_non_coroutines() {
    // The same mechanism isolated from stdlib coroutines: a companion implementing a generic
    // interface must bind the call's type variable from the supertype's type argument.
    const SRC: &str = "interface Key<T>\n\
\n\
class Holder {\n\
    fun tag(): String = \"OK\"\n\
    companion object : Key<Holder>\n\
}\n\
\n\
fun <T> pick(k: Key<T>): T? = null\n\
\n\
fun box(): String {\n\
    // No expected-type hint on `h`: the member read only type-checks when `pick`'s `T` bound to\n\
    // `Holder` from the companion's supertype type argument. (`pick` returns null, so the value\n\
    // path falls through — the binding is what is under test.)\n\
    val h = pick(Holder)\n\
    h?.tag()\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "CompanionKeyGenericAnalogue");
}

#[test]
fn companion_key_type_arg_binds_with_extra_companion_members() {
    // The real ActionUtil shape: the companion declares members (a function + a const) BEYOND its
    // `Key` supertype — the supertype's type argument must still bind. (Front-end pin; emission hits
    // the pre-existing abstract-classpath-base gate.)
    const SRC: &str = "import kotlin.coroutines.AbstractCoroutineContextElement\n\
import kotlin.coroutines.CoroutineContext\n\
\n\
class ActionContextElement(val threadContext: String) :\n\
    AbstractCoroutineContextElement(ActionContextElement), CoroutineContext.Element {\n\
    companion object : CoroutineContext.Key<ActionContextElement> {\n\
        const val TAG: String = \"ctx\"\n\
        fun describe(): String = TAG\n\
    }\n\
}\n\
\n\
fun read(ctx: CoroutineContext): String {\n\
    val e: ActionContextElement? = ctx[ActionContextElement]\n\
    return if (e?.threadContext == ActionContextElement.TAG) \"OK\" else \"F\"\n\
}\n";
    assert_eq!(checker_diags(SRC), Vec::<String>::new());
}

#[test]
fn companion_without_key_supertype_still_rejected() {
    // NEGATIVE pin: a companion WITHOUT the `Key` supertype is not a valid key. (kotlinc: type
    // mismatch.) Pin krusty's diagnostic for the shape so the fix doesn't silently admit it.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    const SRC: &str = "interface Key<T>\n\
\n\
class NoKey {\n\
    companion object\n\
}\n\
\n\
fun <T> pick(k: Key<T>): T? = null\n\
\n\
fun use() {\n\
    pick(NoKey)\n\
}\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[stdlib], Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|m| m.contains("unresolved reference")),
        "companion without the Key supertype used as a key must be rejected; got {diagnostics:?}"
    );
}

#[test]
fn companion_value_ctor_arg_without_supertype_probe() {
    // BACKLOG probe (same family): a companion-value ctor arg (`AbstractCoroutineContextElement(Probe)`)
    // where the companion declares NO supertype. kotlinc rejects this too (the companion isn't a
    // `Key<*>`), but with a type mismatch — krusty reports `unresolved reference` because a
    // supertype-less companion isn't registered as a value at all. Pinned to document the current
    // behavior; a companion-value fix would flip this to a type mismatch.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    const SRC: &str = "import kotlin.coroutines.AbstractCoroutineContextElement\n\
import kotlin.coroutines.CoroutineContext\n\
\n\
class Probe(val id: String) : AbstractCoroutineContextElement(Probe), CoroutineContext.Element {\n\
    companion object\n\
}\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[stdlib], Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|m| m.contains("unresolved reference")),
        "expected the backlog 'unresolved reference' diagnostic; got {diagnostics:?}"
    );
}

/// A classpath generic class for the two `bind_member_return` soundness pins below.
const NUMBER_BOX_LIB: &str = "class Box {\n\
    fun <T : Number> id(a: T): T = a\n\
    fun <T : Number> pick(a: T, b: T): T = b\n\
}\n";

#[test]
fn conflicting_type_var_witnesses_fall_back_to_erased_bound() {
    // SOUNDNESS pin (review probe): `pick`'s `T` is witnessed by BOTH args with different types
    // (`Int` and `Long`), and `pick` returns its SECOND argument. kotlinc infers the common
    // supertype and rejects the `Int` initializer. The bound-type-variable early return in
    // `bind_member_return` must NOT pick the FIRST witness (`Int`) — that compiled and then
    // CCE'd at runtime (`Long cannot be cast to Integer`). Conflicting witnesses fall back to
    // the erased bound (`Number`), so the initializer is a compile-time type error again.
    const MAIN: &str = "fun use() {\n\
    val z: Int = Box().pick(1, 1L)\n\
}\n";
    let Some(diagnostics) = common::diagnostics_against("pick-conflict", NUMBER_BOX_LIB, MAIN)
    else {
        return;
    };
    assert!(
        diagnostics.iter().any(|m| m.contains("mismatch")),
        "conflicting T witnesses must be a compile-time type error; got {diagnostics:?}"
    );
}

#[test]
fn unambiguous_type_var_witness_keeps_bounded_refinement() {
    // The early return survives on UNAMBIGUOUS witnesses: a single-arg `id` binds `T` from its
    // only witness, and `pick`'s widened-to-`Number` result still runs (returning the `Long`
    // second argument at runtime — no cast, no CCE).
    const MAIN: &str = "fun box(): String {\n\
    val x: Int = Box().id(3)\n\
    val y: Long = Box().id(4L)\n\
    val w: Number = Box().pick(1, 1L)\n\
    return if (x == 3 && y == 4L && w.toString() == \"1\") \"OK\" else \"F\"\n\
}\n";
    common::expect_box_ok_against("number-box-refinement", NUMBER_BOX_LIB, MAIN);
}
