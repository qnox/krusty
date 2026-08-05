//! `krusty-ir` — the backend-agnostic, typed common IR.
//!
//! This is the shared layer between the front end (lex/parse/resolve) and the platform backends
//! (JVM today; WASM/JS future — see `docs/ARCHITECTURE.md`). It deliberately mirrors the **Kotlin
//! IR** node taxonomy (`IrClass`/`IrFunction`/`IrCall`/`IrWhen`/…) rather than inventing a novel
//! design, and it is **not** a low-level IR like LLVM — the JVM/JS/WASM targets are managed VMs that
//! need Kotlin's types, nullability, and object model preserved (which LLVM/MLIR discard too early).
//!
//! Representation choices (primitive vs boxed, erasure, calling conventions) are **not** encoded
//! here — they are decided by each backend's lowering of these nodes. Types are expressed in Kotlin
//! terms (`Ty`), never JVM descriptors.
//!
//! Storage follows krusty's index-based invariant: nodes live in parallel `Vec` arenas keyed by
//! `u32` ids (no `Box`/`Rc` graphs; bulk-freeable). Lowering (`ast → ir`) and the JVM backend
//! consuming IR are the next phases; today this module defines the node set + a builder + a printer.

use crate::libraries::InlineKind;
use crate::types::{Ty, TypeName, TypeNameList};

pub type ExprId = u32;
pub type FunId = u32;
pub type ClassId = u32;

/// The target of an `IrExpr::Call`. `Local` references a function defined in this IR file;
/// `External` references a symbol that is **not** — a stdlib `expect`/operator named by its Kotlin
/// FqName (`kotlin/Array.size`, `kotlin/String.plus`, `kotlin/collections/listOf`). Each backend
/// resolves an `External` the way kotlinc does: if it is one of the handful in the **intrinsic
/// table** (array access, arithmetic, …) it emits target bytecode directly; otherwise it resolves
/// the platform **`actual`** from the linked stdlib (`kotlin-stdlib-jvm`/`-js`) and emits a normal
/// call. Either way it is *data* (a FqName), never a new IR node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callee {
    Local(FunId),
    /// The `$default` synthetic of a same-file top-level function/extension (`FunId`) — emitted as
    /// `invokestatic <facade>.<name>$default(realparams, int mask, Object marker)ret`. Like `Local` the
    /// facade is resolved at emit (`self.facade`); the descriptor appends the trailing `I` mask +
    /// `Object` marker to the real function's parameters. Used when a call omits a (possibly non-const)
    /// defaulted argument, mirroring kotlinc's default-argument ABI.
    LocalDefault(FunId),
    External(String),
    /// A top-level function defined in ANOTHER source file of the same multi-file compilation —
    /// `invokestatic <facade>.<name>(params)ret`. Carries the signature as backend-agnostic `Ty`s
    /// (the JVM backend builds the descriptor), so `ir_lower` needn't know JVM descriptors. Distinct
    /// from `Local` (same IrFile, by index) and `Static` (a resolved classpath/library method).
    CrossFile {
        facade: TypeName,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
    },
    /// A resolved classpath static method — `invokestatic owner.name:descriptor`. Used for stdlib
    /// extension/top-level functions resolved from the classpath (`StringsKt.repeat`, `RangesKt.until`),
    /// carrying the exact JVM descriptor so no name is hardcoded in the backend.
    /// `inline` carries the callee's inline-ness in one field (was `inline` + `must_inline`):
    /// [`InlineKind::CanInline`] => a Kotlin `inline` function whose compiled body the JVM backend may
    /// splice here instead of emitting the `invokestatic`; [`InlineKind::MustInline`] => a NON-PUBLIC
    /// `@InlineOnly` callee (`require`/`check`/`error`) with no legal `invokestatic` fallback, so the
    /// backend MUST splice the body (a body it can't splice — e.g. branchy on a non-empty operand stack —
    /// skips the whole file, never miscompiled).
    Static {
        owner: TypeName,
        name: String,
        descriptor: String,
        inline: InlineKind,
    },
    /// An instance method (or property accessor) — `invokevirtual`/`invokeinterface owner.name(sig)` on
    /// the `dispatch_receiver`; `interface` ⇒ `invokeinterface`. `owner` is the receiver's static type.
    /// The SOLE virtual-dispatch callee (classpath, same-file, and sibling-file all unified — no
    /// cp/module/local split). The descriptor comes from ONE source, like [`IrExpr::New`]:
    /// - `params: Some((param_tys, ret))` — a user method (typically sibling-file) whose descriptor the
    ///   JVM backend builds from `Ty`s, and whose value-class name-mangle/erasure the pass applies.
    /// - else `descriptor` — a verbatim JVM descriptor (classpath, or an already-resolved same-file form).
    Virtual {
        owner: TypeName,
        name: String,
        descriptor: String,
        params: Option<(Vec<Ty>, Ty)>,
        interface: bool,
    },
    /// A non-virtual instance call — `invokespecial owner.name:descriptor` on the `dispatch_receiver`.
    /// Used for `super.method(…)`, which dispatches to the named base-class method directly (skipping the
    /// receiver's override). `owner` is the base class declaring the method.
    Special {
        owner: TypeName,
        name: String,
        descriptor: String,
        /// `owner` is an INTERFACE (a diamond `super.f()` dispatched to a superinterface's DEFAULT method):
        /// the method reference must be an `InterfaceMethodref` and the call an `invokespecial` on it.
        interface: bool,
    },
}

/// A compile-time constant (`IrConst` in Kotlin IR).
#[derive(Clone, Debug, PartialEq)]
pub enum IrConst {
    Boolean(bool),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// A Kotlin `Char` — one UTF-16 code UNIT, not a code point. Lone surrogates (D800..DFFF) are
    /// legal `Char` values (`Char.MIN_HIGH_SURROGATE`), so this cannot be a Rust `char`: converting
    /// through `char::from_u32` rejects them and silently folds them to NUL.
    Char(u16),
    /// A Kotlin `String` — a sequence of UTF-16 code units. Same reason as `Char`: `"\uD800"` and
    /// `"😀"` have no Rust `String` spelling one code unit at a time.
    String(crate::kt_string::KtString),
    Null,
}

impl IrConst {
    pub fn zero_for_value_type(ty: Ty) -> IrConst {
        match ty {
            Ty::Boolean => IrConst::Boolean(false),
            Ty::Byte | Ty::UByte => IrConst::Byte(0),
            Ty::Short | Ty::UShort => IrConst::Short(0),
            Ty::Int | Ty::UInt => IrConst::Int(0),
            Ty::Long | Ty::ULong => IrConst::Long(0),
            Ty::Float => IrConst::Float(0.0),
            Ty::Double => IrConst::Double(0.0),
            Ty::Char => IrConst::Char(0),
            _ => IrConst::Null,
        }
    }
}

/// An IR expression node (a subset of Kotlin IR's `IrExpression` hierarchy). Operands reference
/// other expressions by `ExprId` into the arena.
#[derive(Clone, Debug)]
pub enum IrExpr {
    Const(IrConst),
    /// A class-literal constant — `ldc class <internal>` (a `java.lang.Class`). Used e.g. for the
    /// `PropertyReference0Impl(Class, …)` argument in delegated-property setup. `internal = None`
    /// is the current-facade sentinel for places lowered before the facade name is known.
    ClassConst {
        internal: Option<TypeName>,
    },
    /// Read a value parameter / variable by its declaration index.
    GetValue(u32),
    /// Assign to a variable (`IrSetValue`).
    SetValue {
        var: u32,
        value: ExprId,
    },
    /// A call to a function/constructor/operator/stdlib intrinsic (`IrCall`). The `callee` is a
    /// resolved [`Callee`]: a local function, or an intrinsic identified by Kotlin FqName that each
    /// backend maps to its platform (`kotlin/String.plus`, `kotlin/io/println`, …). This single node
    /// expresses every call — there is no dedicated node per stdlib operation.
    Call {
        callee: Callee,
        dispatch_receiver: Option<ExprId>,
        args: Vec<ExprId>,
    },
    /// A placeholder a compiler-extension plugin must specialize before emit. Core lowering produces
    /// it generically, without plugin-specific ABI details, and the plugin rewrites this arena slot into
    /// concrete IR in its body phase. `exprs` are already-lowered operands, `data` carries resolved
    /// name ids; the meaning of both is private to the named plugin. A node that survives to emit is
    /// declined by `jvm_can_emit`.
    PluginPlaceholder {
        /// Which plugin specializes this node.
        plugin: &'static str,
        /// The plugin-specific operation.
        kind: &'static str,
        /// Already-lowered operand expressions, in a plugin-defined order.
        exprs: Vec<ExprId>,
        /// Resolved name ids the plugin needs.
        data: Vec<TypeName>,
    },
    /// `IrReturn` from the enclosing function.
    Return(Option<ExprId>),
    /// `IrBlock` — a sequence of statements; value is the last expression (or Unit).
    Block {
        stmts: Vec<ExprId>,
        value: Option<ExprId>,
    },
    /// `IrWhen` — branches of (condition → result); the AST `if`/`when` lower here. `else` is the
    /// branch with a `None` condition.
    When {
        branches: Vec<(Option<ExprId>, ExprId)>,
    },
    /// `IrTypeOperatorCall` — `is`/`!is`/`as`/`as?`/implicit casts/coercions.
    TypeOp {
        op: IrTypeOp,
        arg: ExprId,
        type_operand: Ty,
    },
    /// `IrWhile` loop. `update` (if present) runs after `body` each iteration, at the `continue`
    /// target — it carries a `for`-loop's increment so `continue` advances the loop rather than
    /// skipping it. A plain `while` has `update: None` (then `continue` re-tests `cond`). `post_test`
    /// ⇒ a `do…while` (the body runs once before `cond` is first tested).
    While {
        cond: ExprId,
        body: ExprId,
        update: Option<ExprId>,
        post_test: bool,
        label: Option<String>,
    },
    /// `break` — exit the innermost enclosing loop, or the loop carrying `label` (`break@outer`).
    Break {
        label: Option<String>,
    },
    /// `continue` — jump to the innermost enclosing loop's `update`/condition (or the labeled loop's).
    Continue {
        label: Option<String>,
    },
    /// A local variable declaration (`IrVariable`), value optional (`lateinit`).
    Variable {
        index: u32,
        ty: Ty,
        init: Option<ExprId>,
        /// `true` for a NAMED source variable (`val x = …`, a destructuring component, a loop
        /// variable); `false` for a compiler-introduced temp (elvis/safe-call materialization,
        /// suspension hoists). The suspend state machine spills every named reference variable in
        /// scope at a suspension point (kotlinc's rule — liveness-irrelevant), but a temp only by
        /// LIVENESS: kotlinc holds those values on the operand stack, which is empty across a
        /// suspension unless the value is still needed.
        named: bool,
    },
    /// A built-in primitive binary operator (`+`/`-`/`<`/`==`/…) on numeric/boolean operands. One
    /// parameterized node (not one-per-intrinsic): Kotlin IR models these as `IrCall` to the
    /// operator function, but the built-in numeric/boolean ops are universal across backends, so a
    /// single node lets each emit the native instruction (JVM `iadd`, JS `+`). Every *other*
    /// operator/stdlib operation — `String.plus`, `toString`, `println`, collections — is an
    /// ordinary `Call` to a `Callee::External` symbol the backend resolves; there is no per-op node.
    PrimitiveBinOp {
        op: IrBinOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    /// Built-in numeric unary negation.
    PrimitiveNeg {
        operand: ExprId,
        ty: Ty,
    },
    /// A Kotlin string template `"a${x}b"` as an ordered list of parts (string constants + interpolated
    /// values, with empty constant chunks dropped). The JVM backend emits it as kotlinc does: a single
    /// part → `String.valueOf(part)`; multiple parts → one `StringBuilder` with a typed `append` per part
    /// and a final `toString()` (vs the old `String.plus` chain, which made one StringBuilder per `+`).
    StringConcat(Vec<ExprId>),
    /// Read a PROPERTY of `owner` — the Kotlin operation, with no accessor in it. A property is a
    /// declaration, not a method: `Dispatchers.IO` names the property `IO` on `kotlinx/coroutines/
    /// Dispatchers`, and there is no `getIO` anywhere in the language. HOW the read is realized — a field
    /// load, an instance accessor, a receiverless static accessor, a `@JvmName`-spelled or value-class-
    /// mangled one — is the target's business, derived by the backend from the owner's declaration. The
    /// node is the same whatever the owner is (this file, a sibling file, the classpath) and whatever the
    /// receiver is; there is no per-origin property node. `ty` is the property's LOGICAL Kotlin type; a
    /// realization whose physical result is erased/boxed is bridged to it by the backend. `receiver` is
    /// the value the property is read on, and stays an expression the program evaluates even when the
    /// realization takes no receiver.
    PropertyRead {
        receiver: ExprId,
        owner: TypeName,
        name: String,
        ty: Ty,
        /// Semantic owner shape selected by resolution. A JVM backend normally reads this from the
        /// compiled declaration, but a sibling source file has no classfile in the shared classpath yet.
        interface: bool,
        /// Exact field declaration selected by resolution, when the property is physically a field.
        /// The complete target is retained so no backend reconstructs owner/name from receiver origin.
        field: Option<Box<crate::libraries::InstanceFieldRef>>,
        /// Stable identity assigned by [`IrFile::add_expr`]. Backend passes can move/clone an operation
        /// into a new expression slot; this identity follows the node so side-table realization facts do
        /// not accidentally remain attached to the obsolete arena index.
        operation: Option<u32>,
    },
    /// Write a PROPERTY of `owner` (statement) — the write analogue of [`IrExpr::PropertyRead`], and the
    /// same rule: it names the property, and how the target writes it (a field store, an instance setter,
    /// a receiverless one) is derived by the backend from the owner's declaration. `ty` is the property's
    /// Kotlin type, which the assigned `value` is bridged to.
    PropertyWrite {
        receiver: ExprId,
        owner: TypeName,
        name: String,
        value: ExprId,
        ty: Ty,
        interface: bool,
        operation: Option<u32>,
    },
    /// Read an instance field (`IrGetField`): `receiver.<fields[index]>` of class `class`.
    GetField {
        receiver: ExprId,
        class: ClassId,
        index: u32,
    },
    /// The RAW value of a `lateinit` backing field, WITHOUT the throw-if-null guard every ordinary
    /// read of one carries. Exists so `::prop.isInitialized` can test the field a normal read would
    /// reject; lowering compares it against null, which is what kotlinc emits (no reflection, no
    /// `KProperty` value).
    LateinitInitialized {
        receiver: ExprId,
        class: ClassId,
        index: u32,
    },
    /// Write an instance field (`IrSetField`): `receiver.<fields[index]> = value` (statement).
    SetField {
        receiver: ExprId,
        class: ClassId,
        index: u32,
        value: ExprId,
    },
    /// Read a top-level (module) property — `statics[index]`, a static field on the file facade.
    GetStatic(u32),
    /// Write a top-level (module) property — `statics[index] = value` (statement).
    SetStatic {
        index: u32,
        value: ExprId,
    },
    /// Construct an instance (`IrConstructorCall`) of `class` with constructor `args` (in field order).
    /// Construct a class: `new <internal>; dup; <args>; invokespecial <init>`. The owner is named by
    /// `internal` — resolve the in-IR class (`classes.get`/`class_info_name`) only where a consumer needs
    /// its `ClassId`; a name with no in-IR class is an external (classpath or other-module-file)
    /// construction. This is the SOLE construction node — there is no cp/module/local variant split.
    ///
    /// The constructor descriptor comes from exactly one source, checked in order:
    /// - `ctor_desc: Some(d)` — a verbatim JVM descriptor for a classpath construction whose signature
    ///   krusty does not model as `Ty`s (erased/library types). Wins; `ctor_params` is then ignored.
    /// - else the owner is an in-IR class — `ctor_params: None` selects its primary constructor (descriptor
    ///   derived from the class, after the value-class pass), `Some(types)` a secondary with that list.
    /// - else (an other-file/module class not in this IR) — `ctor_params: Some(types)` gives the parameter
    ///   types krusty builds the descriptor from.
    New {
        internal: TypeName,
        args: Vec<ExprId>,
        ctor_params: Option<Vec<Ty>>,
        ctor_desc: Option<String>,
    },
    /// A virtual call to a class instance method `methods[index]` of `class` on `receiver`. `args[i] =
    /// None` means parameter `i` is omitted and takes its default (`p.copy(y=5)`, `f(a)` of `f(a, b=…)`);
    /// the meaning is backend-agnostic — the JVM realizes omitted args via the `$default` stub + mask,
    /// another backend may fill them inline. All-`Some` is an ordinary full call.
    MethodCall {
        class: ClassId,
        index: u32,
        receiver: ExprId,
        args: Vec<Option<ExprId>>,
    },
    /// Read an enum entry constant: `Enum.ENTRY` — `getstatic <class>.<entry>:L<class>;`.
    EnumEntry {
        class: ClassId,
        index: u32,
    },
    /// Read a static field holding a singleton instance (Kotlin IR's `IrGetObjectValue`):
    /// `getstatic <owner>.<field>:L<ty>;`. An `object`'s `INSTANCE` (`owner == ty`), or a
    /// `companion`'s `Companion` field on the outer class (`owner` = outer, `ty` = companion).
    StaticInstance {
        owner: ClassId,
        ty: ClassId,
        field: &'static str,
    },
    /// Read a static field of a CLASSPATH class by name — `getstatic owner.name:descriptor`. Used for a
    /// classpath `object` referenced as a value (`EmptyCoroutineContext` → `getstatic kotlin/coroutines/
    /// EmptyCoroutineContext.INSTANCE:Lkotlin/coroutines/EmptyCoroutineContext;`). Unlike `StaticInstance`
    /// (a user `ClassId`) and `GetStatic` (a facade statics index), this names an external owner directly.
    ExternalStaticField {
        owner: TypeName,
        name: String,
        descriptor: String,
    },
    /// Call a static method of a class (`Enum.values()`, `Enum.valueOf(s)`).
    EnumValues {
        class: ClassId,
    },
    EnumValueOf {
        class: ClassId,
        arg: ExprId,
    },
    /// A lambda literal — emitted as `invokedynamic` + `LambdaMetafactory`. `impl_fn` is the
    /// synthesized static method holding the body; `captures` are the free-variable values bound into
    /// the call site (empty = non-capturing). `sam` is `None` for a plain Kotlin lambda (target
    /// `kotlin/jvm/functions/Function{arity}.invoke`), or `Some((interface, method, descriptor))`
    /// for a SAM conversion. Same-module descriptors are derived by the backend.
    /// `inline_body` is the lambda's *value-producing* body form (no synthetic `return`), emitted
    /// directly when the lambda is inlined into a stdlib `inline fun` splice — so a user `return` in the
    /// lambda becomes a real return from the *enclosing* method (correct non-local return). `None` for a
    /// callable reference (`::foo`), which has no inlinable body.
    Lambda {
        impl_fn: u32,
        arity: u8,
        captures: Vec<ExprId>,
        sam: Option<(String, String, Option<String>)>,
        inline_body: Option<ExprId>,
    },
    /// The `kotlin.Unit` singleton value (`IrGetObjectValue` of `Unit`). On the JVM, `getstatic
    /// kotlin/Unit.INSTANCE:Lkotlin/Unit;` — what a `Unit`-returning lambda body yields so its
    /// `FunctionN.invoke` returns an `Object`. Another backend realizes the unit value differently.
    UnitInstance,
    /// The enclosing suspend function's own `Continuation` — the receiver bound to the lambda parameter
    /// of `suspendCoroutineUninterceptedOrReturn { c -> … }`. A placeholder emitted by `ir_lower` that the
    /// CPS pass (`jvm/suspend.rs`) rewrites to the real continuation value (`GetValue(<cont slot>)`) once
    /// the trailing `Continuation` parameter exists. It must never survive to the emitter.
    CurrentContinuation,
    /// Invoke a function value (`f(args)` where `f: (A,…) -> R`) via the `FunctionN.invoke` interface
    /// method. Arguments are boxed to `Object`; the `Object` result is cast/unboxed to `ret`.
    /// `params` retains the semantic Kotlin parameter types through backend carrier lowering so an
    /// adapter can distinguish equal carriers with different wrappers (`UInt` versus `Int`) without
    /// rediscovering the signature from the expression that produced `func`.
    InvokeFunction {
        func: ExprId,
        args: Vec<ExprId>,
        params: Vec<Ty>,
        ret: Ty,
    },
    /// The not-null assertion `operand!!` — yields `operand`, throwing if it is null. On the JVM this
    /// is `kotlin/jvm/internal/Intrinsics.checkNotNull` applied to a duplicate of the value.
    NotNullAssert {
        operand: ExprId,
    },
    /// A `lateinit` read: yields `operand`, throwing `UninitializedPropertyAccessException(name)` if it
    /// is still null. Emitted as `<operand>; dup; ifnonnull L; ldc name;
    /// invokestatic Intrinsics.throwUninitializedPropertyAccessException; L:` — the same guard the
    /// member-field lateinit read uses, here for a `lateinit var` LOCAL slot read.
    LateinitCheck {
        operand: ExprId,
        name: String,
    },
    /// Read a static field holding a singleton on a class defined OUTSIDE this compilation (a classpath
    /// class with no `IrClass`): `getstatic <owner>.<field>:L<ty>;`. Like `StaticInstance` but the owner
    /// and field type are given by internal name directly, not resolved through `ir.classes`.
    ExternalStaticInstance {
        owner: TypeName,
        ty: TypeName,
        field: String,
    },
    /// A `kotlin/jvm/internal/Ref$XxxRef` holder boxing a mutable local that a closure captures: a
    /// new `Ref$IntRef`/`Ref$ObjectRef`/… whose `element` field is initialized to `init`. `elem` is
    /// the boxed value's type (selects the `Ref` subclass + the `element` field descriptor). Evaluates
    /// to the holder, so it's the initializer of the local that holds the box.
    RefNew {
        elem: Ty,
        init: ExprId,
    },
    /// Read a boxed mutable local: `holder.element` (`getfield Ref$XxxRef.element`).
    RefGet {
        holder: ExprId,
        elem: Ty,
    },
    /// Write a boxed mutable local: `holder.element = value` (`putfield`), evaluating to `value`.
    RefSet {
        holder: ExprId,
        elem: Ty,
        value: ExprId,
    },
    /// `throw operand` — throws the (Throwable) value; control never falls through (`Nothing`).
    Throw {
        operand: ExprId,
    },
    /// A `vararg` argument at a call site (Kotlin IR's `IrVararg`): the spread/listed elements and
    /// their element type. The JVM backend packs them into an array; another backend may differ.
    Vararg {
        /// The whole array type (`kotlin/IntArray`, `kotlin/Array<Int>`, `kotlin/Array<String>`), NOT the
        /// bare element — the JVM emitter derives the element + `newarray`/`anewarray` (and boxing of a
        /// `kotlin/Array<Int>` = `Integer[]`) from it via `ir_ty_to_jvm`. The element alone is ambiguous
        /// (`Obj("kotlin/Int")` is both a primitive `IntArray` element and a boxed `Array<Int>` element).
        array_type: Ty,
        /// Parallel to `elements`; a set entry contributes an array rather than one scalar element.
        spreads: Vec<bool>,
        elements: Vec<ExprId>,
    },
    /// Allocate an uninitialized array of `size` elements (`anewarray` for a reference element,
    /// `newarray` for a primitive) — the sized constructor `Array<T>(n) { … }` / `arrayOfNulls<T>(n)`
    /// fills it afterwards. (`Vararg` is the *literal* form with a statically-known element list.)
    NewArray {
        /// The whole array type — see [`IrExpr::Vararg::array_type`].
        array_type: Ty,
        size: ExprId,
    },
    /// `try { body } catch (e: E) { … } … [finally { f }]`. `result` is the value type (`Unit` when
    /// used as a statement). Each catch binds the exception to a value index and runs its body. A
    /// `finally` block runs on every exit (normal, each catch, and an uncaught exception via a
    /// catch-all that re-throws); it is emitted (inlined) at each.
    Try {
        body: ExprId,
        catches: Vec<IrCatch>,
        finally: Option<ExprId>,
        result: Ty,
    },
}

/// One `catch (var: exc_internal) { body }` clause of an [`IrExpr::Try`].
#[derive(Clone, Debug)]
pub struct IrCatch {
    /// Value index the caught exception is bound to.
    pub var: u32,
    /// Source parameter name, absent for compiler-generated handlers.
    pub name: Option<String>,
    /// JVM internal name of the caught exception type.
    pub exc_internal: TypeName,
    pub body: ExprId,
}

/// Built-in binary operators carried by `IrExpr::PrimitiveBinOp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    /// Referential identity (`===`/`!==`): a JVM `if_acmp*` on two reference operands, never the
    /// structural `Intrinsics.areEqual` that `==`/`!=` (`Eq`/`Ne`) uses for references.
    RefEq,
    RefNe,
    And,
    Or,
    /// Bitwise/shift on `Int`/`Long` (Kotlin's `and`/`or`/`xor`/`shl`/`shr`/`ushr` infix functions).
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Ushr,
}

/// The `IrTypeOperatorCall` operators (Kotlin IR's `IrTypeOperator`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrTypeOp {
    InstanceOf,    // `is T`
    NotInstanceOf, // `!is T`
    Cast,          // `as T?` (or `as <primitive>`): a plain `checkcast` — `null` passes
    /// `as T` to a non-null reference type: null-check (`Intrinsics.checkNotNull`) then `checkcast`,
    /// so casting `null` throws — matching kotlinc.
    CastNonNull,
    SafeCast, // `as? T`
    /// Representation coercion the backend inserts (e.g. JVM box/unbox) — explicit in the IR so it
    /// is visible and testable, not hidden in codegen.
    ImplicitCoercion,
}

/// A function/method declaration (`IrFunction`).
#[derive(Clone, Debug)]
pub struct IrFunction {
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// The body expression (typically an `IrBlock`), or `None` for abstract/external.
    pub body: Option<ExprId>,
    pub is_static: bool,
    /// `Some(class internal)` for an instance method — `this` is value index 0, params follow.
    pub dispatch_receiver: Option<TypeName>,
    /// Per-parameter `Some(name)` when the backend should guard it with a non-null assertion at method
    /// entry (`Intrinsics.checkNotNullParameter` on the JVM) — non-null reference parameters of a
    /// visible (non-private) function. Empty for synthesized methods (no guards). Parallel to `params`.
    pub param_checks: Vec<Option<String>>,
}

/// One entry of an `enum class` in [`IrClass`]. Groups what were parallel `Vec`s keyed by entry index
/// (the `(name, args)` tuple plus the separate `subclass` vec), so an entry's name / constructor args /
/// synthesized-subclass marker can't desync.
#[derive(Clone, Debug)]
pub struct IrEnumEntry {
    /// Entry name (`RED`).
    pub name: String,
    /// Lowered constructor-argument value ids (`RED(0xFF0000)`); empty for an arg-less entry. Filled in a
    /// later lowering pass — built empty when the entry list is first created.
    pub args: Vec<ExprId>,
    /// `Some(subclass_internal)` when the entry has a body and is constructed as an instance of a synthesized
    /// anonymous subclass (`new Enum$ENTRY(name, ordinal, args)`); `None` when constructed as the enum
    /// itself.
    /// 1-based source line of the entry's declaration, for the `<clinit>` `LineNumberTable`.
    pub decl_line: u32,
    pub subclass: Option<TypeName>,
}

/// One instance field of an [`IrClass`]. Groups what were parallel `Vec`s keyed by field index, so a
/// field's type / generic-param name / constant default / finality / visibility can't desync.
/// Bit-packed boolean flags for an [`IrField`], collapsing `has_default`/`is_final`/`is_private`/
/// `is_lateinit` into one byte. Read through the `IrField` accessors of the same names; built with the
/// `with_*` chain. Headroom for four more flags before the byte fills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrfFlags(u8);

impl IrfFlags {
    const HAS_DEFAULT: u8 = 1 << 0;
    const IS_FINAL: u8 = 1 << 1;
    const IS_PRIVATE: u8 = 1 << 2;
    const IS_LATEINIT: u8 = 1 << 3;

    #[inline]
    const fn with(mut self, mask: u8, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    const fn has(self, mask: u8) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_has_default(self, on: bool) -> Self {
        self.with(Self::HAS_DEFAULT, on)
    }
    #[inline]
    pub const fn with_is_final(self, on: bool) -> Self {
        self.with(Self::IS_FINAL, on)
    }
    #[inline]
    pub const fn with_is_private(self, on: bool) -> Self {
        self.with(Self::IS_PRIVATE, on)
    }
    #[inline]
    pub const fn with_is_lateinit(self, on: bool) -> Self {
        self.with(Self::IS_LATEINIT, on)
    }
}

#[derive(Clone, Debug)]
pub struct IrField {
    pub name: String,
    pub ty: Ty,
    /// The source type-parameter NAME the field was declared with (`val x: T` → `Some("T")`), else
    /// `None`. Platform-neutral; lets the value-class pass pick the CORRECT bound for a generic
    /// underlying (vs guessing), independent of erasure dropping the name.
    pub type_param: Option<String>,
    /// The CONSTANT default from a primary-constructor default (`val b: Int = 5` → `Some(Int(5))`,
    /// `val t: T? = null` → `Some(Null)`), else `None` (no default, or a non-constant one). Later
    /// compiler passes may use it; the core backend ignores it.
    pub default: Option<IrConst>,
    /// Bit-packed `has_default`/`is_final`/`is_private`/`is_lateinit` (read via the accessors below).
    /// `has_default` — the primary-constructor parameter declared ANY default (constant or not, e.g.
    /// `routes: List<String> = emptyList()`); distinct from `default` (constant-only), needed so the
    /// `@Metadata` emitter sets the `DECLARES_DEFAULT_VALUE` value-parameter flag as kotlinc does.
    /// `is_final` — the backing field is immutable (`val`), emitted `final`. `is_private` — private
    /// backing field (the Kotlin default, reached via accessors); `false` for a field read/written
    /// cross-class (a coroutine continuation's `result`/`label`). `is_lateinit` — backs a `lateinit
    /// var`; every backend read null-checks it and throws when still unset, matching kotlinc.
    pub flags: IrfFlags,
}

impl IrField {
    /// A plain backing field with Kotlin defaults: mutable-unknown (`is_final = false`), `private`, no
    /// generic-param name, no constant default. Synthesized classes build fields from this.
    pub fn new(name: String, ty: Ty) -> IrField {
        IrField {
            name,
            ty,
            type_param: None,
            default: None,
            flags: IrfFlags::default().with_is_private(true),
        }
    }

    #[inline]
    pub fn has_default(&self) -> bool {
        self.flags.has(IrfFlags::HAS_DEFAULT)
    }
    #[inline]
    pub fn is_final(&self) -> bool {
        self.flags.has(IrfFlags::IS_FINAL)
    }
    #[inline]
    pub fn is_private(&self) -> bool {
        self.flags.has(IrfFlags::IS_PRIVATE)
    }
    #[inline]
    pub fn is_lateinit(&self) -> bool {
        self.flags.has(IrfFlags::IS_LATEINIT)
    }

    /// Chainable override of `is_final` on top of [`IrField::new`] (replaces a `..IrField::new` spread).
    #[inline]
    pub fn with_is_final(mut self, on: bool) -> Self {
        self.flags = self.flags.with_is_final(on);
        self
    }
    /// Chainable override of `is_private` on top of [`IrField::new`].
    #[inline]
    pub fn with_is_private(mut self, on: bool) -> Self {
        self.flags = self.flags.with_is_private(on);
        self
    }
}

/// One primary-constructor parameter of an [`IrClass`], in declaration order. Folds what were the
/// index-parallel `ctor_args` tuple and `ctor_param_checks` vec, so a parameter's type / `is_field`
/// flag / null-check name can't desync.
#[derive(Clone, Debug)]
pub struct IrCtorArg {
    /// Source parameter name. Synthetic constructor parameters have no name.
    pub name: Option<String>,
    /// The parameter type (carries declared nullability — a nullable value-class param erases like its
    /// field).
    pub ty: Ty,
    /// `true` ⇒ a `val`/`var` property whose arg is stored to a field (the property fields are
    /// `fields[0..]` in the same relative order); `false` ⇒ a plain parameter, an argument only,
    /// available as a local in `<init>` for property initializers / `init` blocks.
    pub is_field: bool,
    pub has_default: bool,
    pub type_param: Option<u32>,
    /// `Some(name)` when the backend should guard this parameter with a non-null assertion
    /// (`Intrinsics.checkNotNullParameter`) at `<init>` entry — a non-null reference param. `None` for a
    /// primitive, nullable, or class-type-parameter param, and for the synthetic inner `this$0`.
    pub check: Option<String>,
}

/// A property a class DECLARES. A property is a declaration, not a pair of methods: `val a: Int` is one
/// thing, and the `getA()` a target may emit for it is a realization of it. The front end lowers only
/// what is genuinely Kotlin — a source-written accessor's BODY — and leaves naming, descriptors and
/// dispatch to the backend.
#[derive(Clone, Debug)]
pub struct IrProperty {
    pub name: String,
    pub ty: Ty,
    /// Index into [`IrClass::fields`] for the backing field, `None` for a computed/delegated property
    /// (which stores nothing).
    pub backing_field: Option<u32>,
    pub is_var: bool,
    /// Non-final: the accessor a backend emits for it must be overridable.
    pub is_open: bool,
    /// A `private` property. kotlinc emits NO accessor for one — in-class reads go straight to the
    /// backing field — so a use from outside the declaring class has nothing to call, and whichever
    /// path is lowering it does not own the access.
    pub is_private: bool,
    /// `true` for a `var` whose setter alone is declared `private`. This is declaration visibility, not
    /// a JVM flag: every backend must preserve it when realizing a default setter.
    pub setter_is_private: bool,
    /// The lowered body of a source-written getter/setter (a computed, `field`-using, or delegated
    /// property). `None` for a plain backing-field property, whose accessor has no source body at all.
    pub getter: Option<FunId>,
    pub setter: Option<FunId>,
    /// The JVM name a backend must use for the synthesized accessor when the plain spelling is wrong —
    /// a value-class-typed property's accessor is `@JvmName`-mangled. Stamped by the pass that knows the
    /// value classes; `None` means the ordinary spelling applies.
    pub getter_jvm_name: Option<String>,
    pub setter_jvm_name: Option<String>,
    /// Some use of this PRIVATE property reaches it from outside the declaring class — an `inline`
    /// function's body, spliced into its caller. The declaring class must then expose a synthetic
    /// accessor for it (`access$get<X>$p` on the JVM); without one the splice would be illegal, and
    /// silently degrading the `inline` call instead would change what the program does.
    pub needs_access_bridge: bool,
}

/// A class/interface/object declaration (`IrClass`). Instance fields come from the primary
/// constructor's `val`/`var` parameters (in order); the constructor stores each.
#[derive(Clone, Debug)]
pub struct IrClass {
    pub fq_name: TypeName,
    /// `@JvmInline value class` — a single-field class represented unboxed (as its one field's type) by
    /// the JVM `jvm::value_classes` IR pass. The IR otherwise treats it as a plain class.
    pub is_value: bool,
    /// `data class` — carried so the metadata emitter can reproduce kotlinc's `IS_DATA` class flag and
    /// its synthesized `componentN`/`copy`/`equals`/`hashCode`/`toString` function metadata.
    pub is_data: bool,
    /// 1-based source line of the class declaration (0 = unknown). The emitter maps the
    /// `LineNumberTable` of synthesized members (ctor/accessors) to this line, as kotlinc does.
    pub decl_line: u32,
    /// Declared non-`Any` generic upper bounds (`<T: String>` → `("T", String)`), carried verbatim from
    /// the source. Platform-neutral metadata; the JVM value-class pass uses it to erase a value class's
    /// underlying type parameter to its bound (`value class S<T: String>` → `String`).
    pub type_param_bounds: Vec<(String, Ty)>,
    /// ALL declared generic type-parameter names in order (`class C<A, B>` → `["A","B"]`), including
    /// those with only the implicit `Any` bound (unlike [`type_param_bounds`], which lists only non-`Any`
    /// bounds). Empty for a non-generic class.
    pub type_params: Vec<String>,
    pub supertypes: Vec<Ty>,
    /// Instance fields. The first `ctor_param_count` are the primary-constructor parameters (stored
    /// directly from args, in order); any after them are class-body properties initialized by `init_body`.
    /// The properties this class declares — the DECLARATION, alongside the backing `fields` that store
    /// them and (for now) the accessor methods the front end still synthesizes into `methods`.
    pub properties: Vec<IrProperty>,
    pub fields: Vec<IrField>,
    /// How many leading `fields` are property constructor parameters (`val`/`var`) — the rest are body
    /// properties. NOTE: this is the count of constructor params that BACK A FIELD, not the total
    /// constructor arity (a non-`val`/`var` parameter is an argument only, no field) — see `ctor_args`.
    pub ctor_param_count: u32,
    /// ALL primary-constructor parameters in declaration order (each an [`IrCtorArg`] with type,
    /// `is_field`, and optional null-check name). Empty for synthesized/enum/object classes (then the
    /// constructor arity is `ctor_param_count`).
    pub ctor_args: Vec<IrCtorArg>,
    /// Constructor body run after `super(…)`: an effect `Block` lowered with `this` = value 0 and the
    /// constructor parameters as values `1..=N`. When [`explicit_param_stores`] is set it BEGINS with the
    /// `val`/`var` param→field stores (the desugared primary-constructor sugar); it also carries body-
    /// property initializers (`SetField`) and `init { … }` blocks. `None` when there's nothing to run.
    pub init_body: Option<ExprId>,
    /// `true` when `init_body` already stores the primary-constructor `val`/`var` params (and inner
    /// `this$0`) to their fields — the desugared form. The JVM backend then must NOT auto-store them (it
    /// would double-store). `false` for synthesized classes that still rely on the backend's implicit
    /// param→field store.
    pub explicit_param_stores: bool,
    /// Instance methods — `FunId`s into `IrFile.functions` (each with `dispatch_receiver = Some`).
    pub methods: Vec<FunId>,
    pub is_interface: bool,
    /// `true` for a Kotlin `annotation class`. Emitted as a JVM annotation INTERFACE (`ACC_ANNOTATION|
    /// ACC_INTERFACE|ACC_ABSTRACT`, extends `java/lang/annotation/Annotation`, one abstract accessor per
    /// member named after the property — from `fields`). NOT a plain class.
    pub is_annotation: bool,
    /// `Some(annotation_interface_internal)` when this class is the synthetic IMPLEMENTATION of an
    /// annotation (kotlinc's `…$annotationImpl$A$0`): it implements the annotation interface and the JVM
    /// `java.lang.annotation.Annotation` contract (per-member accessors + content `equals`/`hashCode`/
    /// `toString`/`annotationType`), so `A(args)` can construct an annotation instance. `fields` are the
    /// members in order. The backend emits the whole contract from `fields`.
    pub annotation_impl_of: Option<TypeName>,
    /// `true` for a `sealed class`/`sealed interface`.
    pub is_sealed: bool,
    /// Direct subclasses known from the whole source module.
    pub sealed_subclasses: TypeNameList,
    /// `true` for an `abstract class` (not `sealed`).
    pub is_abstract: bool,
    /// `true` for a source `open`/`sealed` class. Needed by backends because a subclass may be emitted
    /// from a different `IrFile`, so same-file subclass scans are not enough to decide JVM finality.
    pub is_open: bool,
    /// Semantic superclass internal name (`kotlin/Any` normally, or a user base class for
    /// `class B : A(args)`). Target-specific representation classes such as JVM enum bases are chosen by
    /// the backend.
    pub superclass: TypeName,
    /// Arguments to the base-class constructor (`: A(args)`) — lowered IR value ids, evaluated with
    /// `this`=value 0 and the primary-constructor params as values `1..=ctor_param_count`. Empty
    /// unless `superclass` is a user base class.
    pub super_args: Vec<ExprId>,
    /// Enum entries in declaration order. Non-empty only for an `enum class`; the backend emits a static
    /// field per entry, a `$VALUES` array, a `<clinit>` that constructs them, and `values()`/
    /// `valueOf(String)`. Each [`IrEnumEntry`] carries its name, lowered constructor args, and optional
    /// synthesized-subclass fq name.
    pub enum_entries: Vec<IrEnumEntry>,
    /// `Some(user_field_types)` marks this class as a synthesized enum-entry subclass: it extends the
    /// enum (`superclass`), has no own fields, and its constructor is `(String name, int ordinal,
    /// <user_field_types>)V` delegating to the enum's `(String,int,<user>)V` constructor.
    pub enum_entry_of: Option<Vec<Ty>>,
    /// `Some(..)` marks this class as a synthesized property-reference singleton: a `final class
    /// extends kotlin/jvm/internal/PropertyReference1Impl` (the `superclass`) with a `public static
    /// final INSTANCE`, a constructor `super(owner.class, name, signature, 0)`, and a `get(Object)
    /// Object` override that reads the referenced property via its getter.
    pub prop_ref: Option<PropRef>,
    /// When `Some`, this class is a synthesized function-reference subclass (`<Owner>$ref$N extends
    /// kotlin/jvm/internal/FunctionReferenceImpl implements Function<arity>`), emitted by
    /// `emit_func_ref_class`. Gives callable references real Kotlin reference EQUALITY (the base class
    /// compares owner/name/signature/boundReceiver) — `::f == ::f`, `a::m != b::m`.
    pub func_ref: Option<FuncRef>,
    /// Synthetic bridge methods: an override whose erased signature differs from the supertype's
    /// (a generic/covariant override) needs an `ACC_BRIDGE` method with the supertype's descriptor
    /// that adapts arguments and delegates to the concrete override.
    pub bridges: Vec<Bridge>,
    /// Implemented interface internal names (`class C : I, J`). The class file lists them as
    /// `implements`; an interface declaration lists its super-interfaces here.
    pub interfaces: TypeNameList,
    /// `object Foo` — a singleton: a `public static final Foo INSTANCE` field, a private no-arg
    /// constructor, and a `<clinit>` that constructs the instance.
    pub is_object: bool,
    /// `true` for a synthesized `C$Companion` class: a private no-arg constructor and no own singleton
    /// field (the `Companion` instance is held by the outer class).
    pub is_companion: bool,
    /// `Some(companion_fq)` on a class with a `companion object`: emit a `public static final
    /// <companion> Companion` field, initialized in this class's `<clinit>`.
    pub companion_class: Option<TypeName>,
    /// Secondary constructors — each an extra `<init>(params)` that delegates to the primary
    /// constructor (`constructor(…) : this(args)`) then runs its body. Empty for most classes.
    pub secondary_ctors: Vec<IrSecondaryCtor>,
    /// `false` for a class with NO primary constructor: the backend emits no primary `<init>`; every
    /// `<init>` comes from `secondary_ctors` (a `Super`-delegating one carries the init body). `true`
    /// for every other class (including synthesized/enum/object).
    pub has_primary_ctor: bool,
    /// RUNTIME-retained annotations applied to this class (`@Anno(...) class TTT`), emitted into the
    /// class's `RuntimeVisibleAnnotations` attribute. Empty for a class with none.
    pub applied_annotations: Vec<AppliedAnnotation>,
    /// User annotations applied to this class's fields (property backing fields and enum-constant
    /// fields), by field name — emitted into each field's `Runtime[In]VisibleAnnotations`. Empty for a
    /// class whose fields carry none.
    pub field_annotations: Vec<FieldAnnotations>,
    /// For an `annotation class`: `true` when its Kotlin retention is RUNTIME (the default) — the emitter
    /// then writes a `@java.lang.annotation.Retention(RUNTIME)` meta-annotation on the annotation interface
    /// so the JVM keeps the annotation's uses visible to reflection.
    pub runtime_retained: bool,
}

/// A resolved JVM annotation value (`element_value`, JVMS §4.7.16.1) — an annotation argument folded to
/// the constant the class file encodes.
#[derive(Clone, Debug)]
pub enum AnnoValue {
    /// A primitive/`String` constant (encoded by tag `B`/`C`/`D`/`F`/`I`/`J`/`S`/`Z`/`s`).
    Const(IrConst),
    /// An enum constant `(enum_type_internal, const_name)` — tag `e`.
    Enum(TypeName, String),
    /// A class literal `T::class` `(type_internal)` — tag `c` (its type descriptor).
    Class(TypeName),
    /// A nested annotation instance `A(...)` — tag `@`.
    Annotation(AppliedAnnotation),
    /// An array `[…]` — tag `[`.
    Array(Vec<AnnoValue>),
}

/// User annotations on one field, split by JVM retention: RUNTIME → `RuntimeVisibleAnnotations`,
/// BINARY → `RuntimeInvisibleAnnotations` (SOURCE-retained ones are dropped during lowering).
#[derive(Clone, Debug)]
pub struct FieldAnnotations {
    pub field: String,
    pub visible: Vec<AppliedAnnotation>,
    pub invisible: Vec<AppliedAnnotation>,
}

/// An applied annotation (`@Anno(...)`) to encode into a `RuntimeVisibleAnnotations` attribute.
#[derive(Clone, Debug)]
pub struct AppliedAnnotation {
    /// The annotation type's internal name (`Anno`).
    pub internal: TypeName,
    /// `element_value_pairs`: `(element_name, value)` in declaration order.
    pub values: Vec<(String, AnnoValue)>,
}

/// How a function-reference subclass's `invoke` dispatches to its target.
#[derive(Clone, Debug)]
pub enum FrDispatch {
    /// Top-level / static target: `invokestatic call_owner.call_name(call_desc)`. All invoke params are
    /// the call arguments.
    Static,
    /// Unbound member `Type::m`: the FIRST invoke param is the receiver; `invokevirtual` on it.
    VirtualUnbound,
    /// Bound member `obj::m`: the receiver is captured (`this.receiver`); `invokevirtual` on it. All
    /// invoke params are the call arguments.
    VirtualBound,
    /// Bound extension `obj::ext`: the receiver is captured (`this.receiver`) and passed as the FIRST
    /// argument of `invokestatic call_owner.call_name(receiver, args…)`. `target_param_tys` leads with
    /// the receiver type; `param_tys` (the invoke args) map to `target_param_tys[1..]`.
    StaticBound,
    /// Suspend conversion: a NON-suspend function VALUE captured as the receiver, adapted to a
    /// `suspend` function type. `invoke((args…), continuation)` delegates
    /// `invokeinterface call_owner(=Function{n}).invoke(args…)` with the trailing continuation
    /// DROPPED (a plain function never suspends; its erased result is returned as the completion
    /// value). The class also implements the `kotlin/coroutines/jvm/internal/SuspendFunction` marker.
    SuspendConvert,
}

/// A synthesized function-reference subclass of `kotlin/jvm/internal/FunctionReferenceImpl`. See
/// `emit_func_ref_class`. `param_tys`/`ret_ty` are the LOGICAL `invoke` signature (for `VirtualUnbound`,
/// `param_tys[0]` is the receiver); the SAM interface erases them to `Object`, so `invoke` casts.
#[derive(Clone, Debug)]
pub struct FuncRef {
    pub bound: bool,
    pub arity: u8,
    /// Class passed to `super(...)` (the reference's declaring class); `None` = the file facade.
    pub owner_class: Option<TypeName>,
    pub fn_name: String,
    pub flags: i32,
    pub dispatch: FrDispatch,
    /// Class the target method is invoked on; `None` = the file facade.
    pub call_owner: Option<TypeName>,
    pub call_name: String,
    pub reflection_name: Option<String>,
    /// The target method is declared on an INTERFACE (`invokeinterface`, not `invokevirtual`).
    pub call_interface: bool,
    /// The LOGICAL `invoke` parameter types. For `VirtualUnbound`, `param_tys[0]` is the receiver
    /// (excluded from the method descriptor / signature). The emitter derives the JVM signature and
    /// reference metadata signature from these + `ret_ty`.
    pub param_tys: Vec<Ty>,
    pub ret_ty: Ty,
    /// The PHYSICAL target-call parameter/return types after backend lowerings such as JVM value-class
    /// erasure. Same shape as `param_tys` (including the unbound receiver slot when present).
    pub target_param_tys: Vec<Ty>,
    pub target_ret_ty: Ty,
    /// Per logical invoke parameter: `Some(value_class_internal)` means the erased Object argument is a
    /// boxed value-class instance and must be unboxed before the physical target call.
    pub unbox_params: Vec<Option<TypeName>>,
    /// Parallel to `unbox_params`: nullable value-class parameters unbox `null` to a null underlying.
    pub unbox_param_nullable: Vec<bool>,
    /// `Some(value_class_internal)` means the physical target returns the value-class underlying and the
    /// function-reference `invoke` must box it back before returning Object.
    pub box_ret: Option<TypeName>,
    /// `StaticBound` only: `Some(value_class_internal)` when the CAPTURED receiver is a value class
    /// (`Z(42)::ext`). The receiver is stored boxed as `Object`; the emitter `checkcast`s it to the box
    /// class then `unbox-impl`s it to the underlying before the mangled `invokestatic ext-<hash>(under)`.
    pub staticbound_recv_unbox: Option<TypeName>,
}

/// A synthesized property-reference class's metadata (`Type::prop` → `Type$prop$N`): the referenced
/// property's owner, name, getter, and value type. The backend emits the `PropertyReference1Impl`
/// subclass from this.
#[derive(Clone, Debug)]
pub struct PropRef {
    /// Referenced property's owner class; `None` = the file facade.
    pub owner_internal: Option<TypeName>,
    pub prop_name: String,
    pub getter_name: String,
    pub getter_descriptor: Option<String>,
    pub setter_name: Option<String>,
    pub setter_descriptor: Option<String>,
    pub prop_ty: Ty,
    /// `false` = an unbound `Type::prop` (a `PropertyReference1Impl` singleton with `get(Object)`);
    /// `true` = a bound `obj::prop` (a `PropertyReference0Impl` constructed with the captured receiver,
    /// whose `get()` reads `this.receiver`).
    pub bound: bool,
    /// A top-level property reference `::foo` (a `(Mutable)PropertyReference0Impl` singleton): the
    /// getter/setter are STATIC on the file facade, so `get`/`set` dispatch via `invokestatic`
    /// (`owner_internal = None` is resolved at emit). No receiver is captured.
    pub static_dispatch: bool,
    /// The referenced property is a `var` — emit a `set(Object)` override (calls `setName`). Only
    /// meaningful with `static_dispatch` (a `MutablePropertyReference0Impl`).
    pub mutable: bool,
    /// An EXTENSION property reference (`obj::ext`, `Type::ext` where `val Recv.ext`): the getter/setter
    /// are STATIC methods on this facade taking the receiver as the first argument (`getExt(Recv)` /
    /// `setExt(Recv, v)`), unlike a member reference's instance `getExt()`. `None` for member/top-level
    /// references. The reference's receiver-class metadata still lives in `owner_internal`.
    pub ext_facade: Option<Option<TypeName>>,
}

impl FuncRef {
    pub fn owner_class_or_facade(&self, facade: &str) -> String {
        self.owner_class
            .map(TypeName::render)
            .unwrap_or_else(|| facade.to_string())
    }

    pub fn call_owner_or_facade(&self, facade: &str) -> String {
        self.call_owner
            .map(TypeName::render)
            .unwrap_or_else(|| facade.to_string())
    }

    pub fn call_owner_key(&self) -> String {
        self.call_owner.map(TypeName::render).unwrap_or_default()
    }

    pub fn call_owner_is_facade(&self) -> bool {
        self.call_owner.is_none()
    }
}

impl PropRef {
    pub fn owner_or_facade(&self, facade: &str) -> String {
        self.owner_internal
            .map(TypeName::render)
            .unwrap_or_else(|| facade.to_string())
    }

    pub fn owner(&self) -> Option<String> {
        self.owner_internal.map(TypeName::render)
    }

    pub fn ext_facade_or_facade(&self, facade: &str) -> Option<String> {
        self.ext_facade.as_ref().map(|f| {
            f.as_ref()
                .map(|facade| facade.render())
                .unwrap_or_else(|| facade.to_string())
        })
    }
}

impl IrClass {
    pub fn fq_name_id(&self) -> TypeName {
        self.fq_name
    }

    pub fn fq_name(&self) -> String {
        self.fq_name.render()
    }

    pub fn fq_name_matches(&self, internal: &str) -> bool {
        self.fq_name.matches(internal)
    }

    pub fn superclass(&self) -> String {
        self.superclass.render()
    }

    pub fn superclass_matches(&self, internal: &str) -> bool {
        self.superclass.matches(internal)
    }

    pub fn has_non_top_superclass(&self) -> bool {
        !self.superclass.matches("")
            && !self.superclass.matches("java/lang/Object")
            && !self.superclass.matches("kotlin/Any")
    }

    pub fn annotation_impl_of(&self) -> Option<String> {
        self.annotation_impl_of.map(TypeName::render)
    }

    pub fn companion_class(&self) -> Option<String> {
        self.companion_class.map(TypeName::render)
    }

    pub fn companion_class_matches(&self, internal: &str) -> bool {
        self.companion_class
            .is_some_and(|name| name.matches(internal))
    }

    pub fn is_singleton(&self) -> bool {
        self.is_object || self.is_companion
    }
}

/// A secondary constructor: `<init>(params)` runs `delegate_prelude`, loads `delegate_args`, calls the
/// delegate target, then runs `body`. `this` is value 0 and parameters are values `1..=params.len()`.
#[derive(Clone, Debug)]
pub struct IrSecondaryCtor {
    pub params: Vec<Ty>,
    pub defaults: Vec<Option<ExprId>>,
    /// Source-ordered temp declarations for delegation arguments.
    pub delegate_prelude: Vec<ExprId>,
    pub delegate_args: Vec<ExprId>,
    pub body: Option<ExprId>,
    /// Which `<init>` this constructor delegates to, and whether it runs the class init body.
    pub delegate: CtorDelegateTarget,
    /// kotlinc marks this ctor `ACC_SYNTHETIC` (0x1000) — e.g. a `@Serializable` deserialization ctor.
    pub synthetic: bool,
}

/// The delegation target of a secondary constructor.
#[derive(Clone, Debug)]
pub enum CtorDelegateTarget {
    /// `this(args)` → `invokespecial` an own `<init>(target_params)` (the primary, or a sibling
    /// secondary). The class init body runs in the reached constructor, not here.
    This {
        target_params: Vec<Ty>,
        to_primary: bool,
        default_masks: Vec<i32>,
    },
    /// `super(args)` (or implicit) → the exact checker-selected superclass constructor.
    Super {
        owner: TypeName,
        target_params: Vec<Ty>,
        default_masks: Vec<i32>,
    },
}

/// A synthetic bridge method (`name(erased_params)erased_ret` → `name(concrete_params)concrete_ret`).
#[derive(Clone, Debug)]
pub struct Bridge {
    pub name: String,
    pub erased_params: Vec<Ty>,
    pub erased_ret: Ty,
    pub concrete_params: Vec<Ty>,
    pub concrete_ret: Ty,
    /// Whether incompatible erased arguments return the collection operation's neutral result.
    pub type_safe_barrier: bool,
    /// The method this bridge delegates to, when it differs from `name` — a value-class-returning
    /// override is emitted under a mangled name (`foo-<hash>`), so the unmangled bridge (`foo`, the
    /// supertype's erased signature) must call the mangled one. `None` ⇒ same as `name`.
    pub target_name: Option<String>,
    /// When set, the bridge boxes its (unboxed value-class) result with `<owner>.box-impl` before
    /// returning — a value-class-returning override seen through a supertype hands back a boxed `X`.
    pub box_ret: Option<TypeName>,
    /// Per concrete parameter, the boxed value class to `checkcast` + `unbox-impl` before the target
    /// call — a generic supertype method (`B.f(T,U)` → erased `f(Object,Object)`) delegates to a
    /// mangled concrete override taking the value class's UNDERLYING, while the incoming arg is a
    /// boxed `X`. Empty (or all-`None`) ⇒ plain checkcast/convert (the common case). JVM/value-class
    /// concern, populated by the value-class pass; the front end leaves it empty.
    pub unbox_params: Vec<Option<TypeName>>,
}

/// A top-level (module) property: a static field on the file facade, initialized in `<clinit>`.
#[derive(Clone, Debug)]
pub struct IrStatic {
    pub name: String,
    pub ty: Ty,
    /// The initializer expression (run in `<clinit>` in declaration order).
    pub init: ExprId,
    /// `var` (mutable) ⇒ a setter is emitted and the backing field is non-`final`.
    pub is_var: bool,
    /// `const val` ⇒ kotlinc keeps the field `public static final` (inlined at use) with no accessor;
    /// a plain top-level `val`/`var` is `private static [final]` + a `public static` getter/setter.
    pub is_const: bool,
    /// The class this static field belongs to. `None` = the file facade (a top-level property). `Some`
    /// = a specific class — a `companion object`'s `const val` lives on the OUTER class (kotlinc emits
    /// `public static final` + `ConstantValue` there), not the facade.
    pub owner: Option<TypeName>,
    /// Declaration visibility (`public` by default). A PRIVATE top-level property gets NO public
    /// accessors; cross-class reads inside the file go through a synthesized `access$get<X>$p` bridge
    /// (kotlinc's shape).
    pub visibility: crate::types::Visibility,
    /// `true` when this backing field has a CUSTOM accessor (`val x = init get() = field…`): the field
    /// is still emitted + initialized in `<clinit>`, but the trivial `getX`/`setX` accessors are NOT
    /// auto-generated here — the custom `getX`/`setX` are emitted as ordinary facade methods (their
    /// bodies lowered with `field` bound to this static). Prevents a duplicate-accessor collision.
    pub custom_accessor: bool,
}

impl IrStatic {
    pub fn is_facade_owned(&self) -> bool {
        self.owner.is_none()
    }

    pub fn owner_matches(&self, internal: &str) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|owner| owner.matches(internal))
    }
}

#[derive(Clone, Default, Debug)]
pub struct FnParamInfo {
    pub names: Vec<String>,
    pub defaults: Option<Vec<Option<ExprId>>>,
}

impl FnParamInfo {
    pub fn names(names: Vec<String>) -> Self {
        Self {
            names,
            defaults: None,
        }
    }

    pub fn defaults(names: Vec<String>, defaults: Vec<Option<ExprId>>) -> Self {
        Self {
            names,
            defaults: Some(defaults),
        }
    }
}

/// One lowered source file (`IrFile`) — its arenas. Index-based, bulk-freeable.
#[derive(Default)]
pub struct IrFile {
    pub package: Option<String>,
    pub source_line_count: u32,
    pub functions: Vec<IrFunction>,
    pub classes: Vec<IrClass>,
    /// Top-level properties — static fields on the facade, initialized in `<clinit>` in order.
    pub statics: Vec<IrStatic>,
    pub exprs: Vec<IrExpr>,
    /// Sparse `ExprId` → 1-based source line for the `LineNumberTable`: statement roots, loop
    /// updates, and the implicit `Unit` return (the block's closing-brace line, kotlinc's mapping).
    /// Absent = no line mark starts at that expression.
    pub expr_lines: std::collections::HashMap<u32, u32>,
    /// Source line for every lowered expression whose AST node has a source location.
    pub expr_source_lines: std::collections::HashMap<u32, u32>,
    /// Source end line for every lowered expression whose AST node has a source location.
    pub expr_end_lines: std::collections::HashMap<u32, u32>,
    /// Source names for `IrExpr::Variable` nodes included in `LocalVariableTable`.
    /// Compiler-generated temporaries are omitted.
    pub value_names: std::collections::HashMap<u32, String>,
    /// `ExprId` → the expression's LOGICAL (source) type as the checker inferred it, recorded verbatim by
    /// the lowerer — NOT erased. The value-class pass consults it to recover the representation of a value
    /// whose IR node alone is ambiguous: a library call returns a physical `Object` descriptor, but its
    /// logical type may be a value class (`runCatching{…}: Result`), so the pass knows the result is the
    /// value class's UNBOXED underlying, not an opaque `Object`. Populated for every lowered expression;
    /// consumed by the value-class pass (the sole owner of value-class knowledge) and — for scalar and
    /// `String` types only, where logical = physical representation — by the suspend pass's operand
    /// snapshot typing (`hoisted_value_ty`) for external callees.
    pub logical_types: std::collections::HashMap<u32, Ty>,
    /// Physical type before a semantic read coercion.
    pub physical_types: std::collections::HashMap<u32, Ty>,
    /// `FunId` → source parameter names and, when present, default-value expressions.
    pub fn_params: std::collections::HashMap<u32, FnParamInfo>,
    /// Per declared method/function, whether each SOURCE parameter was declared nullable (`a: String?`).
    /// The checker `Ty` drops nullability and `IrFunction::params` is kept non-null on purpose — it feeds
    /// the value-class name mangle, which must NOT see recovered nullability (a nullable-VC param mangles
    /// like a non-null one; perturbing it renames the method away from its call sites). `@Metadata` and
    /// the `@NotNull`/`@Nullable` parameter annotations, which DO need the declared nullability, consult
    /// this side-table instead. Empty ⇒ treat every parameter as non-null (the prior behavior).
    pub fn_param_declared_nullable: std::collections::HashMap<u32, Vec<bool>>,
    /// Value-class internal name → the lowered default expression of its single primary-constructor
    /// property, when it has one (`value class ItemId(val value: String = IdGen.next())`).
    /// Lowered in the STATIC `constructor-impl` frame (the sole param is value-index 0, no `this`); the
    /// value-class JVM pass registers it as `constructor-impl`'s param default so the backend emits the
    /// synthetic `constructor-impl$default(U, int, DefaultConstructorMarker)` kotlinc requires.
    value_ctor_defaults: std::collections::HashMap<TypeName, u32>,
    /// Regular (non-value) class internal name → per-primary-constructor-parameter default expression
    /// (`None` = required), when ANY parameter has a default (`data class Wk(val n: String, val s: Int = 5)`).
    /// Lowered in the INSTANCE `<init>` frame (`this` = value 0, params = 1..=n); the backend emits the
    /// synthetic `<init>(params…, int mask, DefaultConstructorMarker)` overload kotlinc requires.
    class_ctor_defaults: std::collections::HashMap<TypeName, Vec<Option<u32>>>,
    /// Instance methods kotlinc leaves NON-`final` even in a final class — currently the data-class
    /// `Object`-overrides (`toString`/`hashCode`/`equals`), which kotlinc emits `public` (open) rather
    /// than `public final`. The JVM backend omits `ACC_FINAL` for a `FunId` in this set.
    pub open_methods: std::collections::HashSet<u32>,
    /// Instance methods kotlinc emits `private` — currently a property's `private set` setter. The JVM
    /// backend uses `ACC_PRIVATE` instead of `ACC_PUBLIC` for a `FunId` in this set.
    pub private_methods: std::collections::HashSet<u32>,
    /// Lambda impls pre-marked `inline_only` by `mark_must_inline_lambdas` (a must-inline callee's
    /// message lambda, assumed spliced). If emission nonetheless records an `invokedynamic` for one,
    /// the two-pass driver RESCUES it — emits the method after all — so the reference never dangles.
    pub must_inline_lambdas: std::collections::HashSet<u32>,
    /// Methods kotlinc marks `ACC_SYNTHETIC` — currently a value class's `box-impl`/`unbox-impl` (the
    /// compiler-manufactured box adapters). The JVM backend ORs `0x1000` for a `FunId` in this set.
    pub synthetic_methods: std::collections::HashSet<u32>,
    /// Methods kotlinc marks `ACC_BRIDGE` (0x40) — e.g. a `@Serializable` serializer's
    /// `typeParametersSerializers`. The JVM backend ORs `0x40` for a `FunId` in this set.
    pub bridge_methods: std::collections::HashSet<u32>,
    /// Per-method (`FunId`) `(param index, boxed value-class type)` for params whose value class has a
    /// NULLABLE underlying: the base (mangled) method unboxes them, but its `<name>$default` synthetic
    /// keeps them BOXED (kotlinc — a `$default` can't disambiguate the unboxed signature without the
    /// `-<hash>` mangling). Recorded by the value-class pass BEFORE erasure; read by `emit_default_stub`
    /// (signature + box-on-fill + unbox-on-delegate) AND the `$default` CALL site (boxed arg + descriptor).
    pub default_stub_boxed_params: std::collections::HashMap<u32, Vec<(usize, crate::types::Ty)>>,
    /// The subset declared in this MODULE's SOURCE (this file or a sibling). Whether such a class ends up
    /// carrying an `@Metadata` record is decided by its own emit, so a record here cannot assume it does —
    /// unlike a CLASSPATH value class, whose value-class-ness is itself decoded from that record.
    pub module_source_value_classes: std::collections::HashSet<TypeName>,
    /// Internal names of classes kotlinc marks `ACC_SYNTHETIC` (0x1000) on the class itself — e.g. a
    /// `@Serializable` class's generated `$$serializer` object.
    synthetic_classes: std::collections::HashSet<TypeName>,
    /// `FunId`s of methods carrying a `Deprecated` classfile attribute (from `@Deprecated`) — e.g. a
    /// `@Serializable` class's `get<Prop>$annotations()` markers, which kotlinc deprecates HIDDEN. ASM
    /// surfaces the attribute as `ACC_DEPRECATED` (0x20000) in the access int, so the ABI gate compares it.
    pub deprecated_methods: std::collections::HashSet<u32>,
    /// Internal names of classes carrying a `Deprecated` classfile attribute (from `@Deprecated`) — e.g. a
    /// `@Serializable` class's generated `$$serializer` object, which kotlinc deprecates HIDDEN.
    deprecated_classes: std::collections::HashSet<TypeName>,
    /// Internal names of classes whose primary constructor has a value-class-typed parameter (a
    /// `data class Rec(val id: ItemId, …)`). kotlinc makes such a primary `<init>` PRIVATE and adds a
    /// PUBLIC|SYNTHETIC accessor `<init>(…args, DefaultConstructorMarker)` that delegates to it — its ABI
    /// for a constructor mentioning an inline class. Recorded by the value-class pass BEFORE it erases the
    /// parameter types (which lose the value-class identity).
    value_param_ctors: std::collections::HashSet<TypeName>,
    /// Lambda impl functions that are INLINE-ONLY — their body has a non-local `return` (returning from
    /// the enclosing function), which is valid only when the lambda is spliced at the call site, never as
    /// a standalone closure method (a non-local return can't compile to a separate method — its `areturn`
    /// would carry the enclosing fn's return type, mismatching the lambda's). The splice reads the
    /// lambda's `inline_body`, not this method, so the backend must NOT emit a `FunId` in this set.
    pub inline_only_fns: std::collections::HashSet<u32>,
    /// `FunId`s of `suspend fun`s, tagged by ir_lower. The coroutine pass (`jvm::suspend`) owns the
    /// whole transform: it rewrites each to the continuation-passing-style ABI (an extra
    /// `kotlin.coroutines.Continuation` parameter, return type erased to `Object`) and, for a function
    /// with suspension points, builds the state machine + continuation class. ir_lower itself lowers a
    /// `suspend fun` as a plain function (mirroring how value classes stay plain until their pass).
    pub suspend_funs: Vec<u32>,
    /// Methods the source declares WITHOUT `override` — a fresh declaration rather than an override of a
    /// supertype member. A language fact nothing else in the IR records: `IrFunction` carries a signature,
    /// not the modifier, and a SYNTHESIZED method (absent here) is deliberately indistinguishable from an
    /// override, since both can legitimately be reached through a supertype's descriptor. The JVM bridge
    /// pass needs it to refuse to point a supertype's descriptor at a same-named fresh declaration.
    pub fresh_method_decls: Vec<u32>,
    /// Each rewritten `suspend fun`'s DECLARED signature `(params, ret)`, captured by the coroutine
    /// pass just before it appends the `Continuation` and erases the return. `@Metadata` and the JVM
    /// generic `Signature` both describe the function in Kotlin terms — `suspend fun f(a: String)`,
    /// not `f(String, Continuation): Object` — so both need the signature the CPS rewrite consumed.
    pub suspend_declared_sigs: std::collections::HashMap<u32, (Vec<Ty>, Ty)>,
    /// Each function the value-class pass rewrote, keyed by `FunId` → its DECLARED `(name, params,
    /// ret)` before the pass mangled the name and erased the value classes to their underlying types.
    /// `@Metadata` names the Kotlin function and its declared parameter types (`ItemId`, not
    /// `String`); the mangled name and erased descriptor ride along as a `JvmMethodSignature`. Captured
    /// before the CPS rewrite, so a function that is both `suspend` and value-class-typed reports the
    /// fully declared signature here rather than the half-lowered one.
    pub vc_declared_sigs: std::collections::HashMap<u32, (String, Vec<Ty>, Ty)>,
    /// Each `suspend fun` whose LOGICAL return is a NON-NULL `@JvmInline value class` → that class's
    /// internal name, recorded by the value-class pass as it boxes the function's tail. A CPS return
    /// is `Object`, so the value crosses the resume boundary in its BOXED form (`X.box-impl`) — the
    /// erasure that turns `X` into its underlying type everywhere else would otherwise leave the
    /// coroutine pass with no way to know which class to `checkcast` + `unbox-impl` on the resume
    /// side. Value-class knowledge stays in the value-class pass; this records only the erasure it
    /// deliberately did NOT apply, which the coroutine pass must undo per suspension.
    pub suspend_boxed_value_class_returns: std::collections::HashMap<u32, TypeName>,
    /// `ExprId` of each direct call to a `suspend fun` → the callee's LOGICAL return type (the source
    /// return, before CPS erasure to `Object`). Recorded by ir_lower from the resolver
    /// (`flags.suspend`), so the coroutine pass recognizes a suspend call to ANOTHER file or a classpath
    /// dependency — whose `FunId` is absent from this file's `suspend_funs`. Same-file/member suspend
    /// calls are caught by `suspend_funs`; this is the cross-unit complement.
    pub suspend_calls: std::collections::HashMap<u32, Ty>,
    /// Non-call expressions whose evaluation is a coroutine SUSPENSION POINT → their logical result
    /// type. Unlike [`Self::suspend_calls`], these nodes do not name a callee and must not have a
    /// continuation argument appended by the coroutine pass. The motivating intrinsic is an inlined
    /// `suspendCoroutineUninterceptedOrReturn` block: lowering has already materialized its continuation
    /// use inside the block, while the coroutine pass still needs to split and resume around the block as
    /// one atomic point. Keeping this semantic category separate prevents a structural block from being
    /// mistaken for a cross-unit call merely because both can suspend.
    pub intrinsic_suspension_points: std::collections::HashMap<u32, Ty>,
    /// A `suspend` LAMBDA's `invokeSuspend` that contains MULTIPLE suspensions / control flow and needs
    /// a state machine with the lambda instance itself as the continuation — `(invokeSuspend FunId,
    /// lambda ClassId, field_base)`. `field_base` is the first free field index on the lambda class
    /// (after its captures/parameters), where the coroutine pass appends the `result`/`label`/spilled
    /// fields. ir_lower builds `invokeSuspend` with the plain body (suspend calls un-threaded); the pass
    /// flattens it. (Single-suspension lambdas are handled inline by ir_lower instead.) `field_base` is
    /// the number of leading capture/parameter fields — the pass reloads them into locals `2..` at each
    /// `invokeSuspend` entry (so a captured/parameter value survives a re-entry), excludes them from
    /// spilling, and places the result/label/spilled fields after them.
    pub suspend_lambda_sm: Vec<(u32, u32, u32)>,
    /// `FunId` → the backend-agnostic generic-signature SHAPE of a type-parameterized function. The JVM
    /// backend formats this into a `Signature` attribute; the IR itself holds no target descriptors.
    pub signatures: std::collections::HashMap<u32, IrGenericSig>,
    /// Kotlin declaration visibility by `(class internal name, property name)`. This is distinct from
    /// the backing field's JVM visibility.
    pub prop_visibilities: std::collections::HashMap<(String, String), crate::types::Visibility>,
    /// Declaring class to indices in `statics` for class properties whose JVM field is static.
    pub declared_class_statics: std::collections::HashMap<TypeName, Vec<u32>>,
    /// (class internal name, property name) → 1-based source line of a BODY property's declaration.
    /// kotlinc attributes both the property's getter and its constructor-side initializer to this line.
    pub prop_decl_lines: std::collections::HashMap<(String, String), u32>,
    /// FunId → 1-based source line of its `fun` declaration, for the method's `LineNumberTable`.
    /// A side map (not a field on `IrFunction`) so the 40-odd construction sites stay untouched.
    pub fn_decl_lines: std::collections::HashMap<u32, u32>,
    /// FunId → 1-based source line of a BLOCK body's closing `}` — kotlinc maps a `Unit` fn's
    /// implicit `return` there in the `LineNumberTable`. Same side-map rationale as `fn_decl_lines`.
    pub fn_close_lines: std::collections::HashMap<u32, u32>,
    /// Class fq-internal-name → its generic-signature SHAPE (type parameters + bounds), for a generic
    /// class. The JVM backend formats it into the class `Signature` attribute.
    class_signatures: std::collections::HashMap<TypeName, IrGenericSig>,
    /// Class fq-internal-name → `(field name, type-parameter name)` for each field whose declared type
    /// is a bare type parameter (`class Pair<A, B>(val a: A)` → `[("a", "A")]`). The JVM backend formats
    /// each into a field `Signature` (`TA;`). Backend-agnostic: only the type-parameter name is stored.
    field_signatures: std::collections::HashMap<TypeName, Vec<(String, String)>>,
    /// `(data-class fq-internal-name, field name)` → the JVM owner internal the field's `hashCode()`
    /// dispatches on, as chosen by `field_hash` (ir_lower, which has the classpath). A concrete-class
    /// field owns its own `hashCode`; an INTERFACE/collection field (`List`, `Set`, `Map`, …) dispatches
    /// `java/lang/Object.hashCode`. The pool seeder reads this to intern the SAME methodref the body
    /// emits — otherwise it would seed `List.hashCode` (an orphan) while the body uses `Object.hashCode`.
    data_hashcode_owners: std::collections::HashMap<(String, String), String>,
    /// Classpath `@JvmInline value class` (fq-internal-name → erased underlying `Ty`) REFERENCED in
    /// this file. The JVM value-class pass merges these into its erasure map so a dependency value class
    /// unboxes exactly like a same-file declaration. Populated by ir_lower (which has the classpath);
    /// native unsigned builtins keep their dedicated `Ty`/runtime handling and are not recorded here.
    external_value_classes: std::collections::HashMap<TypeName, Ty>,
    /// Expression identity → `(declared value-class name, erased underlying type)` for a construction
    /// rewritten in place by the JVM value-class pass. This records semantic origin rather than the
    /// generated helper's spelling: a source `new` remains distinguishable from an unrelated static call
    /// whose user-written name happens to resemble a backend helper. Consumers must treat the entry as
    /// valid only while the rewritten expression remains at the same arena index.
    erased_value_constructions: std::collections::HashMap<ExprId, (TypeName, Ty)>,
    /// Getter method name (`getV`) for each classpath `@JvmInline value class` in
    /// [`Self::external_value_classes`] — lets the value-class pass recognize a sole-property read emitted
    /// as `invokevirtual X.getV()` and rewrite it to identity (the receiver IS the unboxed underlying).
    external_value_class_getters: std::collections::HashMap<TypeName, String>,
    /// Call `ExprId` → reified-type substitution for a `<reified T>` CLASSPATH inline extension whose
    /// compiled body the backend must splice: `[(type-parameter name, concrete JVM internal name)]`
    /// (`[("T", "lib/Prov")]`). The bytecode splicer feeds this to `substitute_reified` so a
    /// `reifiedOperationMarker`/`T::class` in the spliced body specializes to the concrete type — the
    /// classpath analogue of the IR inliner's `reified_subst` (which only has same-file bodies). The
    /// concrete type is a backend-agnostic `Ty`; the JVM splicer maps it to an internal name.
    pub reified_call_subst: std::collections::HashMap<u32, Vec<(String, Ty)>>,
    /// Extension-call `ExprId` → the extension's DECLARED (un-erased) receiver source type, forwarded
    /// verbatim from the resolved callable's `source_receiver`. `ir_lower` records it with NO value-class
    /// reasoning of its own; the value-class pass reads it to decide box/unbox at the receiver. The signal
    /// distinguishes `fun Result<T>.getOrThrow()` (receiver `kotlin/Result` — a value class whose facade
    /// method takes the UNBOXED underlying, so a `Boxed` receiver unboxes) from a generic `fun <T> T.foo()`
    /// (receiver a type variable — erases to `Object`, receiver stays boxed) even though both erase
    /// identically in the JVM descriptor. Only concrete declared receivers are recorded (a `Var` receiver
    /// is `None` at the source and never inserted).
    pub ext_call_source_receiver: std::collections::HashMap<u32, Ty>,
    /// Call `ExprId` → the callee's DECLARED (un-erased, pre-substitution) return type, forwarded
    /// verbatim from the resolved library member's `declared_ret`. `ir_lower` records it with NO
    /// value-class reasoning of its own; the value-class pass reads it to decide the RESULT's
    /// representation, exactly as `ext_call_source_receiver` does for the receiver.
    ///
    /// The distinction it carries cannot be recovered from the descriptor: a value class returned by
    /// declaration (`A.create(): A<String>`, whose mangled method hands back the erased carrier) and
    /// the same value class arriving BOXED out of a generic slot (`List<TokenBox>.get`) both
    /// spell `()Ljava/lang/Object;`. The declaration separates them — `create` declares `A`, `get`
    /// declares the type parameter `E` (never recorded, since it is not a class). Only NON-NULL
    /// declared returns are recorded: a nullable value class really is boxed.
    pub call_declared_ret: std::collections::HashMap<u32, Ty>,
    /// Stable property-operation identity → the declaration's semantic value type before
    /// use-site generic substitution. Resolution knows this fact uniformly for every source owner;
    /// recording it here lets a backend derive the physical accessor boundary without asking whether
    /// the declaration came from this file, a sibling file, or a dependency. This deliberately carries
    /// no accessor spelling or target descriptor. As with the JVM realization table below, the stable
    /// operation identity survives backend rewrites that move a property node to another arena slot.
    pub property_declaration_types: std::collections::HashMap<u32, Ty>,
    /// Stable property-operation identity → JVM accessor spelling and physical property-value type,
    /// selected by the value-class pass for an owner in another source file. The common node keeps the
    /// Kotlin name and logical type; this backend side table carries the declaration-less target
    /// realization only after the JVM pass has enough erasure information. It is deliberately not keyed
    /// by expression arena index because boxing and identity rewrites can move a node.
    pub property_accessor_jvm_realizations: std::collections::HashMap<u32, (String, Ty)>,
    /// Lifted-lambda function id → the parameter INDEX at which the lambda's OWN parameters begin (its
    /// captured variables occupy the lower indices). A lambda's own parameters arrive through the
    /// `FunctionN` generic (`Object`) invoke slot, so a reference-underlying value-class parameter is
    /// BOXED there — the value-class pass reads this to type such a slot as the boxed value class (so
    /// `it.getOrThrow()` unboxes it), without the lowerer probing value-class-ness itself.
    pub lambda_own_params_from: std::collections::HashMap<u32, u32>,
    /// Lifted-lambda function id → the DECLARED parameter types and return type of the user
    /// `fun interface` method the lambda was SAM-converted to. Absent for a plain `FunctionN` lambda,
    /// whose `invoke` slots are all generic. The distinction only matters to a target that erases
    /// some declared types away (the JVM's value classes): a generic slot carries a value class
    /// BOXED, while a slot the SAM method spells as the value class itself carries the erased
    /// underlying — so the lambda's impl method must match whichever the interface actually declares.
    /// The lowerer records the declaration; deciding what erases is the backend pass's job.
    pub lambda_sam_signature: std::collections::HashMap<u32, (Vec<Ty>, Ty)>,
}

/// Backend-agnostic generic-signature shape of a declaration (the data a JVM `Signature` / a future
/// platform's equivalent needs). NO target descriptors here — each backend formats its own.
#[derive(Clone, Debug)]
pub struct IrGenericSig {
    /// Each type parameter: its name and its upper bound as a Kotlin `Ty` (`kotlin/Any` when none).
    pub type_params: Vec<(String, Ty)>,
    /// Per value parameter: `Some(name)` when it is a bare type-parameter reference, else `None` (the
    /// backend uses the parameter's own erased type). Empty for a class signature.
    pub param_tparams: Vec<Option<String>>,
    /// `Some(name)` when the return type is a bare type-parameter reference, else `None`.
    pub ret_tparam: Option<String>,
    /// For a CLASS signature with a PARAMETERIZED supertype: the superclass + superinterfaces as
    /// platform-agnostic `Ty`s carrying their type arguments (`[Any, Operation<Result<Int>>]`), so a
    /// cross-module reader recovers a member's concrete generic return. The backend formats these into the
    /// JVM `Signature` string. Empty ⇒ no parameterized supertype (backend emits the default `Object`
    /// superclass). Empty for a function signature.
    pub supers: Vec<Ty>,
}

impl IrFile {
    pub(crate) fn expr_diverges_by(
        &self,
        expression: ExprId,
        leaf: &impl Fn(ExprId, &IrExpr) -> bool,
    ) -> bool {
        match self.expr(expression) {
            IrExpr::Return(_)
            | IrExpr::Throw { .. }
            | IrExpr::Break { .. }
            | IrExpr::Continue { .. } => true,
            IrExpr::Block { stmts, value } => match value {
                Some(value) => self.expr_diverges_by(*value, leaf),
                None => stmts
                    .last()
                    .is_some_and(|stmt| self.expr_diverges_by(*stmt, leaf)),
            },
            IrExpr::When { branches } => {
                branches.iter().any(|(condition, _)| condition.is_none())
                    && branches
                        .iter()
                        .all(|(_, body)| self.expr_diverges_by(*body, leaf))
            }
            IrExpr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                finally.is_some_and(|finally| self.expr_diverges_by(finally, leaf))
                    || (self.expr_diverges_by(*body, leaf)
                        && catches
                            .iter()
                            .all(|catch| self.expr_diverges_by(catch.body, leaf)))
            }
            _ => leaf(expression, self.expr(expression)),
        }
    }

    pub fn with_package(package: Option<String>) -> Self {
        IrFile {
            package,
            ..Default::default()
        }
    }

    pub fn class_const(&mut self, internal: Option<&str>) -> ExprId {
        let internal = internal.map(crate::types::type_name);
        self.add_expr(IrExpr::ClassConst { internal })
    }

    pub fn external_static_field(
        &mut self,
        owner: &str,
        name: impl Into<String>,
        descriptor: impl Into<String>,
    ) -> ExprId {
        let owner = crate::types::type_name(owner);
        self.add_expr(IrExpr::ExternalStaticField {
            owner,
            name: name.into(),
            descriptor: descriptor.into(),
        })
    }

    pub fn external_static_instance(
        &mut self,
        owner: &str,
        ty: &str,
        field: impl Into<String>,
    ) -> ExprId {
        let owner = crate::types::type_name(owner);
        let ty = crate::types::type_name(ty);
        self.add_expr(IrExpr::ExternalStaticInstance {
            owner,
            ty,
            field: field.into(),
        })
    }

    pub fn new_external(
        &mut self,
        internal: &str,
        ctor_desc: impl Into<String>,
        args: Vec<ExprId>,
    ) -> ExprId {
        let internal = crate::types::type_name(internal);
        self.add_expr(IrExpr::New {
            internal,
            args,
            ctor_params: None,
            ctor_desc: Some(ctor_desc.into()),
        })
    }

    pub fn new_cross_file(&mut self, internal: &str, params: Vec<Ty>, args: Vec<ExprId>) -> ExprId {
        let internal = crate::types::type_name(internal);
        self.add_expr(IrExpr::New {
            internal,
            args,
            ctor_params: Some(params),
            ctor_desc: None,
        })
    }

    pub fn mark_synthetic_class(&mut self, internal: &str) {
        self.synthetic_classes
            .insert(crate::types::type_name(internal));
    }

    pub fn is_synthetic_class(&self, internal: &str) -> bool {
        self.synthetic_classes
            .contains(&crate::types::type_name(internal))
    }

    pub fn mark_deprecated_class(&mut self, internal: &str) {
        self.deprecated_classes
            .insert(crate::types::type_name(internal));
    }

    pub fn is_deprecated_class(&self, internal: &str) -> bool {
        self.deprecated_classes
            .contains(&crate::types::type_name(internal))
    }

    pub fn mark_value_param_ctor(&mut self, internal: &str) {
        self.mark_value_param_ctor_name(crate::types::type_name(internal));
    }

    pub fn mark_value_param_ctor_name(&mut self, internal: TypeName) {
        self.value_param_ctors.insert(internal);
    }

    pub fn has_value_param_ctor(&self, internal: &str) -> bool {
        self.value_param_ctors
            .contains(&crate::types::type_name(internal))
    }

    pub fn insert_value_ctor_default(&mut self, internal: &str, expr: u32) {
        self.value_ctor_defaults
            .insert(crate::types::type_name(internal), expr);
    }

    pub fn value_ctor_default(&self, internal: &str) -> Option<u32> {
        self.value_ctor_defaults
            .get(&crate::types::type_name(internal))
            .copied()
    }

    pub fn insert_class_ctor_defaults(&mut self, internal: &str, defaults: Vec<Option<u32>>) {
        self.class_ctor_defaults
            .insert(crate::types::type_name(internal), defaults);
    }

    pub fn class_ctor_defaults(&self, internal: &str) -> Option<&Vec<Option<u32>>> {
        self.class_ctor_defaults
            .get(&crate::types::type_name(internal))
    }

    pub fn insert_class_signature(&mut self, internal: &str, sig: IrGenericSig) {
        self.class_signatures
            .insert(crate::types::type_name(internal), sig);
    }

    pub fn class_signature(&self, internal: &str) -> Option<&IrGenericSig> {
        self.class_signatures
            .get(&crate::types::type_name(internal))
    }

    pub fn insert_field_signatures(&mut self, internal: &str, sigs: Vec<(String, String)>) {
        self.field_signatures
            .insert(crate::types::type_name(internal), sigs);
    }

    pub fn field_signatures(&self, internal: &str) -> Option<&Vec<(String, String)>> {
        self.field_signatures
            .get(&crate::types::type_name(internal))
    }

    /// Record the JVM owner a data-class field's `hashCode()` dispatches on (see `data_hashcode_owners`).
    pub fn set_data_hashcode_owner(&mut self, class_internal: &str, field: &str, owner: String) {
        self.data_hashcode_owners
            .insert((class_internal.to_string(), field.to_string()), owner);
    }

    /// The JVM `hashCode()` owner recorded for a data-class field, if any.
    pub fn data_hashcode_owner(&self, class_internal: &str, field: &str) -> Option<&str> {
        self.data_hashcode_owners
            .get(&(class_internal.to_string(), field.to_string()))
            .map(String::as_str)
    }

    pub fn insert_external_value_class_name(&mut self, internal: TypeName, underlying: Ty) {
        self.external_value_classes.insert(internal, underlying);
    }

    pub fn external_value_class_name(&self, internal: TypeName) -> Option<&Ty> {
        self.external_value_classes.get(&internal)
    }

    pub fn has_external_value_class_name(&self, internal: TypeName) -> bool {
        self.external_value_class_name(internal).is_some()
    }

    /// Resolve a value class's erased underlying type without making callers branch on whether the
    /// declaration belongs to this source file or was recovered from dependency metadata.
    fn value_class_underlying_name(&self, internal: TypeName) -> Option<Ty> {
        self.external_value_class_name(internal)
            .copied()
            .or_else(|| {
                self.classes
                    .iter()
                    .find(|class| class.is_value && class.fq_name == internal)
                    .and_then(|class| class.fields.first().map(|field| field.ty))
            })
    }

    /// Preserve the source meaning of a value-class construction after the JVM pass replaces its
    /// generic `New` node with a target helper call. The safety gate uses this pass-produced fact instead
    /// of trusting generated method names, which are neither semantic identities nor reserved names.
    pub(crate) fn record_erased_value_construction(
        &mut self,
        expression: ExprId,
        owner: TypeName,
        underlying: Ty,
    ) {
        self.erased_value_constructions
            .insert(expression, (owner, underlying));
    }

    /// The in-IR (same-file) `ClassId` for `internal`, or `None` when the name is an external/other-module
    /// class not compiled in this file. The bridge from the unified [`IrExpr::New`]'s owner name back to a
    /// `ClassId` for consumers that need the in-IR class (emit, function/property-reference detection).
    pub fn class_id_by_name(&self, internal: TypeName) -> Option<ClassId> {
        self.classes
            .iter()
            .position(|c| c.fq_name == internal)
            .map(|i| i as ClassId)
    }

    /// Whether `internal` names a value class — an in-IR (same-file) one OR an external/other-module one.
    /// The single name-keyed value-class test for the unified [`IrExpr::New`] (which no longer carries a
    /// same-file `ClassId` to branch on).
    pub fn is_value_class_name(&self, internal: TypeName) -> bool {
        self.classes
            .iter()
            .any(|c| c.is_value && c.fq_name == internal)
            || self.has_external_value_class_name(internal)
    }

    pub fn insert_external_value_class_getter_name(&mut self, internal: TypeName, getter: String) {
        self.external_value_class_getters.insert(internal, getter);
    }

    pub fn external_value_class_getters(&self) -> impl Iterator<Item = (TypeName, &str)> + '_ {
        self.external_value_class_getters
            .iter()
            .map(|(&internal, getter)| (internal, getter.as_str()))
    }

    pub fn param_defaults(&self, fid: u32) -> Option<&Vec<Option<ExprId>>> {
        self.fn_params.get(&fid)?.defaults.as_ref()
    }
    pub fn has_param_defaults(&self, fid: u32) -> bool {
        self.param_defaults(fid).is_some()
    }
    pub fn param_names(&self, fid: u32) -> Option<&[String]> {
        Some(&self.fn_params.get(&fid)?.names)
    }
    pub fn expr(&self, id: ExprId) -> &IrExpr {
        &self.exprs[id as usize]
    }
    pub fn add_expr(&mut self, mut e: IrExpr) -> ExprId {
        let id = self.exprs.len() as u32;
        match &mut e {
            IrExpr::PropertyRead { operation, .. } | IrExpr::PropertyWrite { operation, .. } => {
                // Preserve an identity already assigned to a moved/cloned property operation. Fresh
                // lowering constructors pass `None` and receive their original arena index here.
                operation.get_or_insert(id);
            }
            _ => {}
        }
        self.exprs.push(e);
        id
    }
    pub fn add_fun(&mut self, f: IrFunction) -> FunId {
        let id = self.functions.len() as u32;
        self.functions.push(f);
        id
    }
    pub fn add_class(&mut self, c: IrClass) -> ClassId {
        let id = self.classes.len() as u32;
        self.classes.push(c);
        id
    }
}

/// Invoke `f` on each direct child expression of `e`. The single structural definition of an
/// `IrExpr`'s sub-expressions — tree walks (index shifting, scans) delegate here so a new variant is
/// covered in one place. Written EXHAUSTIVELY (no `_` arm) on purpose: adding an `IrExpr` variant must
/// fail to compile here rather than silently drop its children from every walk.
pub fn for_each_child(exprs: &[IrExpr], e: ExprId, f: &mut impl FnMut(ExprId)) {
    match &exprs[e as usize] {
        IrExpr::Block { stmts, value } => {
            stmts.iter().for_each(|&s| f(s));
            value.iter().for_each(|&v| f(v));
        }
        IrExpr::When { branches } => branches.iter().for_each(|(c, b)| {
            c.iter().for_each(|&c| f(c));
            f(*b);
        }),
        IrExpr::Return(v) => v.iter().for_each(|&v| f(v)),
        IrExpr::TypeOp { arg, .. }
        | IrExpr::NotNullAssert { operand: arg }
        | IrExpr::LateinitCheck { operand: arg, .. }
        | IrExpr::Throw { operand: arg }
        | IrExpr::EnumValueOf { arg, .. }
        | IrExpr::RefNew { init: arg, .. }
        | IrExpr::RefGet { holder: arg, .. }
        | IrExpr::NewArray { size: arg, .. }
        | IrExpr::PrimitiveNeg { operand: arg, .. } => f(*arg),
        IrExpr::StringConcat(parts) => parts.iter().for_each(|&p| f(p)),
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } => {
            f(*lhs);
            f(*rhs);
        }
        IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => f(*value),
        IrExpr::SetField {
            receiver, value, ..
        }
        | IrExpr::PropertyWrite {
            receiver, value, ..
        }
        | IrExpr::RefSet {
            holder: receiver,
            value,
            ..
        } => {
            f(*receiver);
            f(*value);
        }
        IrExpr::Variable { init, .. } => init.iter().for_each(|&i| f(i)),
        IrExpr::GetField { receiver, .. }
        | IrExpr::LateinitInitialized { receiver, .. }
        | IrExpr::PropertyRead { receiver, .. } => f(*receiver),
        IrExpr::Call {
            args,
            dispatch_receiver,
            ..
        } => {
            dispatch_receiver.iter().for_each(|&r| f(r));
            args.iter().for_each(|&a| f(a));
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            f(*receiver);
            args.iter().flatten().for_each(|&a| f(a));
        }
        IrExpr::InvokeFunction { func, args, .. } => {
            f(*func);
            args.iter().for_each(|&a| f(a));
        }
        IrExpr::New { args, .. } | IrExpr::Vararg { elements: args, .. } => {
            args.iter().for_each(|&a| f(a))
        }
        IrExpr::Lambda {
            captures,
            inline_body,
            ..
        } => {
            captures.iter().for_each(|&c| f(c));
            inline_body.iter().for_each(|&b| f(b));
        }
        IrExpr::While {
            cond, body, update, ..
        } => {
            f(*cond);
            f(*body);
            update.iter().for_each(|&u| f(u));
        }
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            f(*body);
            catches.iter().for_each(|c| f(c.body));
            finally.iter().for_each(|&fin| f(fin));
        }
        IrExpr::PluginPlaceholder { exprs: kids, .. } => kids.iter().for_each(|&k| f(k)),
        IrExpr::Const(_)
        | IrExpr::ClassConst { .. }
        | IrExpr::GetValue(_)
        | IrExpr::GetStatic(_)
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::EnumEntry { .. }
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::StaticInstance { .. }
        | IrExpr::EnumValues { .. }
        | IrExpr::UnitInstance
        | IrExpr::CurrentContinuation => {}
    }
}

/// Whether evaluating `expr` can run NO code at all — a literal or a local read. Strictly conservative:
/// anything that touches a type (a static read, an enum entry, a singleton) can trigger that type's
/// initializer, which is arbitrary user code, so it is NOT in this set. Used where a value the source
/// program computes is discarded by the target form and the consumer must decide whether it still has to
/// be evaluated.
pub fn expr_runs_no_code(ir: &IrFile, expr: ExprId) -> bool {
    matches!(
        ir.expr(expr),
        IrExpr::Const(_) | IrExpr::ClassConst { .. } | IrExpr::GetValue(_) | IrExpr::UnitInstance
    )
}

/// Whether a top-level `foo$default` synthetic can be SAFELY emitted for `fid`. The function name must be
/// unmangled — a value-class-parameter-mangled `foo-<hash>` needs box/unbox adaptation the plain facade
/// stub doesn't model — and every registered default expression must be simple enough to re-emit inside
/// the stub: no lambda, no `invoke`, no value-class-mangled call, and no reference to a value index beyond
/// the parameters (a default that spilled a temp or captured a closure). A plain OBJECT/`new` construction
/// (`filters: F = F()`) IS allowed — the stub re-emits it like any other value. Conservative — an unknown
/// shape is rejected, so the caller falls back to the unchanged inline call-site fill (never a miscompile).
pub fn toplevel_default_stub_safe(ir: &IrFile, fid: u32) -> bool {
    let f = &ir.functions[fid as usize];
    // This gate runs at TWO times: during lowering (call-site routing, before the value-class pass)
    // and at emission (stub gating, after it). The value-class pass MANGLES a function whose
    // parameters mention a value class (`foo` → `foo-<hash>`) and erases those parameter types; the
    // stub follows along (`foo-<hash>$default(erased params…)`, kotlinc's shape), so a value-class
    // signature is allowed — with carve-outs the erased stub can't model, checked by whichever
    // evidence the current time has: pre-pass by the still-value-class-typed parameters, post-pass
    // by the mangled name / the pass's own records. Both runs must agree for every carve-out the
    // POST-pass can see, or a call is routed to a stub that is never emitted (NoSuchMethodError);
    // a pre-pass-only rejection merely leaves an uncalled stub. (Only the PARAMETERS matter: a
    // file-class function's value-class return does not mangle its name — see `vc_mangle`.)
    let vc_of = |t: &Ty| {
        t.non_null()
            .obj_internal()
            .filter(|&n| ir.is_value_class_name(n))
    };
    let vc_params: Vec<TypeName> = f.params.iter().filter_map(vc_of).collect();
    if f.name.contains('-') || !vc_params.is_empty() {
        // A `suspend` function's CPS rewrite appends a `Continuation` parameter the stub shape
        // doesn't model — reject a suspend function whose name/params show value-class mangling.
        if ir.suspend_funs.contains(&fid) {
            return false;
        }
        // A NULLABLE-underlying value-class param stays BOXED in kotlinc's stub signature; the plain
        // facade stub emits fully erased params — not modeled. Post-pass evidence: the value-class
        // pass recorded the boxed stub params (`vc_underlying_nullable`, which also recurses through
        // a nested-VC underlying). Pre-pass evidence: the underlying's DIRECT nullability — the
        // recursive nested case is covered by the nested-VC reject below, so the two runs agree.
        if ir
            .default_stub_boxed_params
            .get(&fid)
            .is_some_and(|v| !v.is_empty())
        {
            return false;
        }
        for &n in &vc_params {
            // A GENERIC value class (`X<T>(val s: T)`) or a NESTED value-class underlying erases
            // through extra layers the placeholder/adaptation here doesn't cover — conservative
            // pre-pass reject (post-pass still emits the stub; it just gets no routed calls).
            if !vc_stub_shape_ok(ir, n) {
                return false;
            }
        }
    }
    // A user function literally named `<name>$default` (a back-ticked identifier) would collide with the
    // synthetic — don't emit the stub (kotlinc also treats that as a conflicting declaration).
    let stub_name = format!("{}$default", f.name);
    if ir
        .functions
        .iter()
        .any(|g| g.dispatch_receiver.is_none() && g.name == stub_name)
    {
        return false;
    }
    // Overloaded top-level functions may all have `<name>$default` siblings; the descriptor selects the
    // concrete overload, just as it does for the real method. The lowerer reaches this path only after the
    // checker has selected a source declaration / function id.
    let n = f.params.len() as u32;
    let Some(defaults) = ir.param_defaults(fid) else {
        return false;
    };
    defaults.iter().enumerate().all(|(i, d)| match d {
        Some(d) => {
            default_expr_stub_safe(ir, *d, n) && default_root_slot_ok(ir, *d, f.params.get(i))
        }
        None => true,
    })
}

/// The erased representations must MATCH where a default value lands: a value-class construction
/// default (`z: Z = Z(42)`) re-emits as the unboxed `constructor-impl` call, so it can only fill a
/// slot whose erased type IS that underlying — i.e. a parameter of exactly the same NON-nullable
/// value class. A `Z?` (boxed slot), `Any`, or interface parameter would need a `box-impl` the
/// erased stub doesn't model — rejected (a pre-pass-only reject merely leaves an uncalled stub:
/// post-pass the default is already a rewritten call and passes trivially).
fn default_root_slot_ok(ir: &IrFile, e: ExprId, param: Option<&Ty>) -> bool {
    // Post-pass view: provenance recorded while rewriting the generic construction tells us the
    // expression's erased result without inspecting a JVM helper name. The erased slot must be exactly
    // that underlying type; a boxed or generic slot would require adaptation the facade does not model.
    if let Some((_, underlying)) = ir.erased_value_constructions.get(&e) {
        return param.is_some_and(|param| param == underlying);
    }
    match &ir.exprs[e as usize] {
        // Pre-pass view: the default is a value-class `new` — the slot must be exactly the same
        // NON-nullable value class (erased representations match).
        IrExpr::New { internal, .. } if ir.is_value_class_name(*internal) => {
            param.is_some_and(|p| !p.is_nullable() && p.obj_internal() == Some(*internal))
        }
        root => {
            // A param whose erased slot is PRIMITIVE (pre-pass: a non-nullable value-class param
            // over a primitive underlying; post-pass: the already-erased param) can only be filled
            // by a root carrying the erased representation — a const, a local slot, or a
            // construction (handled above). Any OTHER value-class-typed root (a field/getter read,
            // a coercion) carries the BOXED representation — stored raw in the stub, it fails
            // class-load verification.
            let primitive_slot = param.is_some_and(|p| {
                !p.is_reference()
                    || (!p.is_nullable()
                        && p.obj_internal()
                            .and_then(|name| ir.value_class_underlying_name(name))
                            .is_some_and(|underlying| !underlying.is_reference()))
            });
            if !primitive_slot {
                return true;
            }
            let root_is_vc = ir.logical_types.get(&e).is_some_and(|t| {
                t.non_null()
                    .obj_internal()
                    .is_some_and(|name| ir.is_value_class_name(name))
            });
            !root_is_vc || matches!(root, IrExpr::Const(_) | IrExpr::GetValue(_))
        }
    }
}

/// Whether a value class is simple enough for the erased `$default` stub: non-generic, with a
/// resolvable, non-nullable, non-nested-VC underlying — the carve-outs of
/// [`toplevel_default_stub_safe`], shared by the parameter check and the default-expression
/// construction check ([`default_expr_stub_safe`]).
fn vc_stub_shape_ok(ir: &IrFile, n: TypeName) -> bool {
    let vc_of = |t: &Ty| {
        t.non_null()
            .obj_internal()
            .filter(|&name| ir.is_value_class_name(name))
    };
    let same_file = ir.classes.iter().find(|c| c.is_value && c.fq_name == n);
    if same_file.is_some_and(|c| !c.type_params.is_empty()) {
        return false;
    }
    let underlying = ir.value_class_underlying_name(n);
    match underlying {
        Some(u) if u.is_nullable() => false,
        Some(u) if vc_of(&u).is_some() => false,
        Some(_) => true,
        None => false,
    }
}

fn default_expr_stub_safe(ir: &IrFile, e: ExprId, n: u32) -> bool {
    match &ir.exprs[e as usize] {
        IrExpr::GetValue(i) if *i >= n => return false,
        IrExpr::SetValue { var, .. } if *var >= n => return false,
        IrExpr::Variable { index, .. } if *index >= n => return false,
        // A plain `new`/object construction (`f: F = F()`) is fine — the stub re-emits it. A
        // VALUE/inline-class construction erases to its unboxed underlying — but the value-class
        // pass rewrites it to a plain `constructor-impl` static call in registered defaults exactly
        // as in function bodies, so the erased stub CAN re-emit it — behind the same carve-outs as
        // a value-class parameter (a generic / nullable-underlying / nested-VC shape keeps the
        // conservative reject: the file falls back to the inline call-site fill / skip).
        IrExpr::New { internal, .. }
            if ir.is_value_class_name(*internal) && !vc_stub_shape_ok(ir, *internal) =>
        {
            return false;
        }
        // A default LAMBDA (`f: (Int) -> Int = { it + 1 }`) is re-emittable: the closure construction
        // only reads its captures, and those are checked as children (a capture of a spilled temp /
        // enclosing local — any value `>= n` — rejects above). Its `inline_body` is in the lambda's OWN
        // value numbering, but it is never emitted by the stub (the stub instantiates the closure), so
        // a false child rejection there is merely conservative. A callable-ref (`RefNew`) or an
        // `invoke` still reaches state the static stub layout doesn't carry.
        IrExpr::Lambda { .. } => {}
        IrExpr::RefNew { .. } | IrExpr::InvokeFunction { .. } => return false,
        IrExpr::Call {
            callee: Callee::Static { name, .. },
            ..
        } if name.contains('-')
            // Only a construction that THIS pass rewrote is known safe. Generated-looking spellings
            // are not reserved: accepting them by string would let an unrelated user/library call
            // bypass the conservative mangled-call gate.
            && !ir.erased_value_constructions.contains_key(&e) =>
        {
            return false;
        }
        _ => {}
    }
    let mut ok = true;
    for_each_child(&ir.exprs, e, &mut |c| {
        if !default_expr_stub_safe(ir, c, n) {
            ok = false;
        }
    });
    ok
}

/// Shift every value index (`GetValue`/`SetValue`/`Variable`) `>= threshold` by `by`, throughout the
/// expression tree rooted at `e`. Used when a pass **appends parameters** to a function: the body's
/// locals (numbered from the old parameter count) must move up by the number of new parameters so
/// they don't collide with the inserted parameter slots.
pub fn shift_value_indices(ir: &mut IrFile, e: ExprId, threshold: u32, by: u32) {
    match &mut ir.exprs[e as usize] {
        IrExpr::GetValue(i) if *i >= threshold => *i += by,
        IrExpr::SetValue { var, .. } if *var >= threshold => *var += by,
        IrExpr::Variable { index, .. } if *index >= threshold => *index += by,
        // A `catch (e) { … }` variable is DECLARED by the `IrCatch.var` field (not a `Variable` node); its
        // uses inside the catch body are `GetValue`s shifted by the recursion below, so the field must
        // shift too or the binding and its reads desync.
        IrExpr::Try { catches, .. } => {
            for c in catches.iter_mut() {
                if c.var >= threshold {
                    c.var += by;
                }
            }
        }
        _ => {}
    }
    // A nested `Lambda`'s CAPTURES reference the ENCLOSING scope's value slots (shift them), but its
    // `inline_body` is a copy of the lambda's own body in the lambda's OWN value numbering (captures +
    // params) — recursing into it would corrupt those internal slots with this enclosing threshold/delta.
    // So for a `Lambda`, shift only the captures (the impl method's body is a separate function, already
    // untouched here).
    if let IrExpr::Lambda { captures, .. } = &ir.exprs[e as usize] {
        let caps = captures.clone();
        for c in caps {
            shift_value_indices(ir, c, threshold, by);
        }
        return;
    }
    let mut kids = Vec::new();
    for_each_child(&ir.exprs, e, &mut |c| kids.push(c));
    for c in kids {
        shift_value_indices(ir, c, threshold, by);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_trivial_function_ir() {
        // Model `fun answer(): Int = 42` in the IR by hand (lowering comes in a later phase).
        let mut f = IrFile::default();
        let lit = f.add_expr(IrExpr::Const(IrConst::Int(42)));
        let ret = f.add_expr(IrExpr::Return(Some(lit)));
        let body = f.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        let fun = f.add_fun(IrFunction {
            name: "answer".to_string(),
            params: vec![],
            ret: Ty::obj("kotlin/Int"),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        assert_eq!(f.functions[fun as usize].name, "answer");
        // The return type is a Kotlin FqName, not a JVM descriptor — the backend maps it.
        match f.functions[fun as usize].ret.obj_internal() {
            Some(fq) => assert_eq!(fq, "kotlin/Int"),
            None => panic!("expected class type"),
        }
        assert!(matches!(f.expr(body), IrExpr::Block { .. }));
    }

    #[test]
    fn expr_diverges_by_handles_branches_and_custom_leaves() {
        let mut f = IrFile::default();
        let condition = f.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let first = f.add_expr(IrExpr::Return(None));
        let second = f.add_expr(IrExpr::Throw { operand: condition });
        let exhaustive = f.add_expr(IrExpr::When {
            branches: vec![(Some(condition), first), (None, second)],
        });
        let non_exhaustive = f.add_expr(IrExpr::When {
            branches: vec![(Some(condition), first)],
        });
        let custom_leaf = f.add_expr(IrExpr::Const(IrConst::Int(1)));

        assert!(f.expr_diverges_by(exhaustive, &|_, _| false));
        assert!(!f.expr_diverges_by(non_exhaustive, &|_, _| false));
        assert!(f.expr_diverges_by(custom_leaf, &|id, _| id == custom_leaf));
    }

    #[test]
    fn zero_for_value_type_tracks_primitive_carriers() {
        assert_eq!(
            IrConst::zero_for_value_type(Ty::Boolean),
            IrConst::Boolean(false)
        );
        assert_eq!(IrConst::zero_for_value_type(Ty::Int), IrConst::Int(0));
        assert_eq!(IrConst::zero_for_value_type(Ty::UInt), IrConst::Int(0));
        assert_eq!(IrConst::zero_for_value_type(Ty::ULong), IrConst::Long(0));
        assert_eq!(IrConst::zero_for_value_type(Ty::String), IrConst::Null);
    }

    #[test]
    fn shift_value_indices_shifts_lambda_captures_not_inline_body() {
        // A `Lambda` whose CAPTURE references the enclosing slot 1 and whose `inline_body` references the
        // lambda's OWN slot 1. Shifting the enclosing scope (threshold 1, +2) must shift the capture
        // (1 → 3) but leave the lambda-internal `inline_body` reference (1) untouched.
        let mut f = IrFile::default();
        let cap = f.add_expr(IrExpr::GetValue(1)); // capture of enclosing value 1
        let inner = f.add_expr(IrExpr::GetValue(1)); // the lambda's OWN value 1
        let lam = f.add_expr(IrExpr::Lambda {
            impl_fn: 0,
            arity: 0,
            captures: vec![cap],
            sam: None,
            inline_body: Some(inner),
        });
        let outer = f.add_expr(IrExpr::GetValue(1)); // an enclosing value 1, sibling of the lambda
        let block = f.add_expr(IrExpr::Block {
            stmts: vec![lam],
            value: Some(outer),
        });
        shift_value_indices(&mut f, block, 1, 2);
        assert!(
            matches!(f.exprs[cap as usize], IrExpr::GetValue(3)),
            "capture must shift 1 -> 3"
        );
        assert!(
            matches!(f.exprs[outer as usize], IrExpr::GetValue(3)),
            "enclosing ref must shift 1 -> 3"
        );
        assert!(
            matches!(f.exprs[inner as usize], IrExpr::GetValue(1)),
            "lambda-internal inline_body ref must NOT shift"
        );
    }

    #[test]
    fn ir_field_new_uses_kotlin_defaults() {
        let f = IrField::new("x".to_string(), Ty::Int);
        assert_eq!(f.name, "x");
        assert_eq!(f.ty, Ty::Int);
        assert_eq!(f.type_param, None);
        assert_eq!(f.default, None);
        // Kotlin default: private backing field, not known-final, not lateinit.
        assert!(f.is_private());
        assert!(!f.is_final());
        assert!(!f.is_lateinit());
    }

    #[test]
    fn arena_builders_append_and_index() {
        let mut f = IrFile::default();
        let a = f.add_expr(IrExpr::Const(IrConst::Int(1)));
        let b = f.add_expr(IrExpr::Const(IrConst::Int(2)));
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert!(matches!(f.expr(a), IrExpr::Const(IrConst::Int(1))));

        let fid = f.add_fun(IrFunction {
            name: "g".to_string(),
            params: vec![],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        assert_eq!(fid, 0);
        let cid = f.add_class(IrClass {
            fq_name: "demo/C".into(),
            ..blank_class("demo/C")
        });
        assert_eq!(cid, 0);
        assert!(f.classes[cid as usize].fq_name_matches("demo/C"));
    }

    #[test]
    fn for_each_child_visits_every_direct_operand() {
        let mut f = IrFile::default();
        let lhs = f.add_expr(IrExpr::Const(IrConst::Int(1)));
        let rhs = f.add_expr(IrExpr::Const(IrConst::Int(2)));
        let bin = f.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs,
            rhs,
        });
        let mut kids = Vec::new();
        for_each_child(&f.exprs, bin, &mut |c| kids.push(c));
        assert_eq!(kids, vec![lhs, rhs]);

        // A leaf node (Const) has no children.
        let mut none = Vec::new();
        for_each_child(&f.exprs, lhs, &mut |c| none.push(c));
        assert!(none.is_empty());

        // A block visits its statements then its value.
        let blk = f.add_expr(IrExpr::Block {
            stmts: vec![lhs, rhs],
            value: Some(bin),
        });
        let mut bk = Vec::new();
        for_each_child(&f.exprs, blk, &mut |c| bk.push(c));
        assert_eq!(bk, vec![lhs, rhs, bin]);
    }

    /// A minimal well-formed `IrClass` for tests that only exercise fields/functions on the file.
    fn blank_class(fq: &str) -> IrClass {
        IrClass {
            fq_name: fq.into(),
            is_value: false,
            is_data: false,
            decl_line: 0,
            type_param_bounds: Vec::new(),
            type_params: Vec::new(),
            supertypes: Vec::new(),
            properties: Vec::new(),
            fields: Vec::new(),
            field_annotations: Vec::new(),
            ctor_param_count: 0,
            ctor_args: Vec::new(),
            init_body: None,
            explicit_param_stores: false,
            methods: Vec::new(),
            is_interface: false,
            is_annotation: false,
            annotation_impl_of: None,
            is_sealed: false,
            sealed_subclasses: Default::default(),
            is_abstract: false,
            is_open: false,
            superclass: "kotlin/Any".into(),
            super_args: Vec::new(),
            enum_entries: Vec::new(),
            enum_entry_of: None,
            prop_ref: None,
            func_ref: None,
            bridges: Vec::new(),
            interfaces: Default::default(),
            is_object: false,
            is_companion: false,
            companion_class: None,
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
            applied_annotations: Vec::new(),
            runtime_retained: false,
        }
    }

    fn add_toplevel_fn(f: &mut IrFile, name: &str, param: Ty) -> u32 {
        f.add_fun(IrFunction {
            name: name.to_string(),
            params: vec![param],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        })
    }

    #[test]
    fn toplevel_default_stub_safe_accepts_a_simple_constant_default() {
        let mut f = IrFile::default();
        let fid = add_toplevel_fn(&mut f, "greet", Ty::Int);
        let def = f.add_expr(IrExpr::Const(IrConst::Int(5)));
        f.fn_params
            .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
        assert!(toplevel_default_stub_safe(&f, fid));
    }

    #[test]
    fn toplevel_default_stub_safe_accepts_mangled_and_rejects_missing_defaults() {
        // A value-class-MANGLED name (the post-pass view of a VC-param function) emits the mangled
        // `foo-<hash>$default` stub, kotlinc's shape — accepted since the erased params carry no
        // extra carve-out evidence.
        let mut f = IrFile::default();
        let fid = add_toplevel_fn(&mut f, "greet-abc123", Ty::Int);
        let def = f.add_expr(IrExpr::Const(IrConst::Int(5)));
        f.fn_params
            .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
        assert!(toplevel_default_stub_safe(&f, fid));

        // A mangled SUSPEND function's CPS-appended Continuation param is not modeled — rejected.
        f.suspend_funs.push(fid);
        assert!(!toplevel_default_stub_safe(&f, fid));

        let mut g = IrFile::default();
        let gid = add_toplevel_fn(&mut g, "hello", Ty::Int);
        assert!(!toplevel_default_stub_safe(&g, gid));
    }

    #[test]
    fn toplevel_default_stub_safe_allows_overloaded_and_rejects_unsafe_default() {
        let mut f = IrFile::default();
        let fid = add_toplevel_fn(&mut f, "over", Ty::Int);
        add_toplevel_fn(&mut f, "over", Ty::String);
        let def = f.add_expr(IrExpr::Const(IrConst::Int(0)));
        f.fn_params
            .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
        assert!(toplevel_default_stub_safe(&f, fid));

        let mut g = IrFile::default();
        let gid = add_toplevel_fn(&mut g, "spill", Ty::Int);
        let bad = g.add_expr(IrExpr::GetValue(3));
        g.fn_params
            .insert(gid, FnParamInfo::defaults(Vec::new(), vec![Some(bad)]));
        assert!(!toplevel_default_stub_safe(&g, gid));
    }

    #[test]
    fn toplevel_default_stub_safe_accepts_a_lambda_capturing_only_parameters() {
        // `fun foo(base: Int, f: () -> Int = { base })` — the default lambda captures a PARAMETER
        // (value 0, in scope inside the stub), so the stub can re-emit the closure construction.
        let mut f = IrFile::default();
        let fid = f.add_fun(IrFunction {
            name: "foo".to_string(),
            params: vec![Ty::Int, Ty::obj("kotlin/jvm/functions/Function0")],
            ret: Ty::Int,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let cap = f.add_expr(IrExpr::GetValue(0));
        let lam = f.add_expr(IrExpr::Lambda {
            impl_fn: 0,
            arity: 0,
            captures: vec![cap],
            sam: None,
            inline_body: None,
        });
        f.fn_params.insert(
            fid,
            FnParamInfo::defaults(Vec::new(), vec![None, Some(lam)]),
        );
        assert!(toplevel_default_stub_safe(&f, fid));
    }

    #[test]
    fn toplevel_default_stub_safe_rejects_a_lambda_capturing_a_spilled_temp() {
        // A capture beyond the parameter range (a spilled temp / enclosing local) is not in scope in
        // the static stub frame — rejected.
        let mut f = IrFile::default();
        let fid = add_toplevel_fn(&mut f, "foo", Ty::Int);
        let cap = f.add_expr(IrExpr::GetValue(7));
        let lam = f.add_expr(IrExpr::Lambda {
            impl_fn: 0,
            arity: 0,
            captures: vec![cap],
            sam: None,
            inline_body: None,
        });
        f.fn_params
            .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(lam)]));
        assert!(!toplevel_default_stub_safe(&f, fid));
    }

    #[test]
    fn toplevel_default_stub_safe_value_class_param_pre_mangling() {
        // The PRE-pass view of a VC-param function: plain name, value-class-typed param. A plain
        // non-nullable underlying routes through the (soon-mangled) stub; a NULLABLE underlying stays
        // boxed in kotlinc's stub signature (not modeled) — rejected.
        let mut f = IrFile::default();
        let mut c = blank_class("X");
        c.is_value = true;
        c.fields.push(IrField {
            name: "s".to_string(),
            ty: Ty::String,
            type_param: None,
            default: None,
            flags: IrfFlags::default().with_is_final(true),
        });
        f.add_class(c);
        let fid = add_toplevel_fn(&mut f, "foo", Ty::obj("X"));
        let def = f.add_expr(IrExpr::Const(IrConst::Int(5)));
        f.fn_params
            .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
        assert!(toplevel_default_stub_safe(&f, fid));

        f.classes[0].fields[0].ty = Ty::nullable(Ty::String);
        assert!(!toplevel_default_stub_safe(&f, fid));
    }

    #[test]
    fn default_stub_trusts_value_construction_provenance_not_helper_spelling() {
        // A generated-looking JVM name is not proof that a call came from value-class lowering: source
        // and dependency declarations can use escaped identifiers. Keep the call rejected until the
        // value-class pass explicitly records that this expression replaced a semantic construction.
        let mut f = IrFile::default();
        let fid = add_toplevel_fn(&mut f, "consume", Ty::Int);
        let call = f.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: crate::types::type_name("example/Wrapper"),
                name: "constructor-impl".to_string(),
                descriptor: "(I)I".to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: Vec::new(),
        });
        f.fn_params
            .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(call)]));

        assert!(!toplevel_default_stub_safe(&f, fid));

        f.record_erased_value_construction(
            call,
            crate::types::type_name("example/Wrapper"),
            Ty::Int,
        );
        assert!(toplevel_default_stub_safe(&f, fid));
    }

    #[test]
    fn default_stub_uses_external_value_class_metadata_without_a_source_class() {
        // Dependency metadata and same-file declarations must enter the representation checks through
        // one query. No synthetic `IrClass` is added here: the external semantic record alone identifies
        // the value class and its primitive underlying slot.
        let mut f = IrFile::default();
        let wrapper = crate::types::type_name("dependency/Wrapper");
        f.insert_external_value_class_name(wrapper, Ty::Int);
        let fid = add_toplevel_fn(&mut f, "consume", Ty::obj_name(wrapper));
        let argument = f.add_expr(IrExpr::Const(IrConst::Int(7)));
        let construction = f.add_expr(IrExpr::New {
            internal: wrapper,
            args: vec![argument],
            ctor_params: Some(vec![Ty::Int]),
            ctor_desc: None,
        });
        f.fn_params.insert(
            fid,
            FnParamInfo::defaults(Vec::new(), vec![Some(construction)]),
        );

        assert!(toplevel_default_stub_safe(&f, fid));
    }
}
