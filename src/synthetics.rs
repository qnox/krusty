//! Registry of compiler-**synthetic** functions: a simple map **FQN → IR body**.
//!
//! A synthetic is a function kotlinc realizes in codegen with no callable classpath body. The registry
//! is the front end's **IR-level override**: during lowering a call is matched here *before* classpath
//! resolution, and the matched body contributes the call's IR directly. It has priority over the
//! classpath, but a user-declared function of the same name still shadows it (the kotlinc rule).
//!
//! Each body is emitted **inline at the callsite** by construction — there is no out-of-line synthetic
//! function, so "inline" is not a stored attribute. A body may still *decline* (`None`) when it can't
//! safely override a given call (a branchy element, an undeterminable reified type); the caller then
//! falls through to normal resolution.
//!
//! This is purely the IR map. The complementary **JVM intrinsic registry**
//! (`jvm::ir_emit::emit_intrinsic`) is the **callsite bytecode override**: it realizes an IR `Call` to
//! a known FQN as inline bytecode (`kotlin/Array.size` → `arraylength`). The single array-allocation
//! leaf these bodies bottom out in — `IrExpr::NewArray { element_type, size }` — is realized there
//! (`newarray int` for `Array<Int>`, `anewarray Integer` for `Array<Int?>`): the IR carries one node,
//! the emitter picks the opcode.
//!
//! Functions that DO have a real (inline) classpath body — `require`/`check`/`println`/`listOf`/… — are
//! deliberately NOT here; they resolve through the classpath, with signatures recovered from `@Metadata`.

use crate::ast::ExprId as AstExprId;
use crate::ir::{Callee, ExprId, IrExpr};
use crate::ir_lower::{ty_to_ir, Lower};
use crate::types::Ty;

/// A call site matched against the registry: the call's argument AST ids and the call expression
/// itself (so a body can read the checker-inferred result element type).
pub struct SynthCall<'a> {
    pub args: &'a [AstExprId],
    pub call: AstExprId,
}

/// A synthetic's IR **body** — builds its IR with ordinary nodes against the active `Lower`. Returns
/// the body's result expr, or `None` to decline (the caller falls through to normal resolution). Gets
/// its own `Synthetic` so one body can serve an element-parameterized family (`intArrayOf`/`longArrayOf`).
pub(crate) type BodyFn = fn(&'static Synthetic, &mut Lower<'_>, &SynthCall<'_>) -> Option<ExprId>;

/// One synthetic function: its fully-qualified name (the identity shared with the JVM intrinsic
/// registry), the source call name lookup matches on, and its mandatory IR body.
pub struct Synthetic {
    pub fqn: &'static str,
    pub name: &'static str,
    pub(crate) body: BodyFn,
}

/// The synthetic whose source call name is `name`, or `None`. Has priority over the classpath; the
/// caller is responsible for honoring user-declared shadowing first.
pub fn lookup(name: &str) -> Option<&'static Synthetic> {
    TABLE.iter().find(|s| s.name == name)
}

const fn syn(fqn: &'static str, name: &'static str, body: BodyFn) -> Synthetic {
    Synthetic { fqn, name, body }
}

static TABLE: &[Synthetic] = &[
    // Primitive vararg literals — `intArrayOf(1, 2, 3): IntArray`.
    syn("kotlin/intArrayOf", "intArrayOf", b_prim_vararg),
    syn("kotlin/longArrayOf", "longArrayOf", b_prim_vararg),
    syn("kotlin/doubleArrayOf", "doubleArrayOf", b_prim_vararg),
    syn("kotlin/floatArrayOf", "floatArrayOf", b_prim_vararg),
    syn("kotlin/booleanArrayOf", "booleanArrayOf", b_prim_vararg),
    syn("kotlin/charArrayOf", "charArrayOf", b_prim_vararg),
    syn("kotlin/byteArrayOf", "byteArrayOf", b_prim_vararg),
    syn("kotlin/shortArrayOf", "shortArrayOf", b_prim_vararg),
    // Primitive size constructors — `IntArray(n)` / `IntArray(n) { i -> e }`.
    syn("kotlin/IntArray", "IntArray", b_prim_size),
    syn("kotlin/LongArray", "LongArray", b_prim_size),
    syn("kotlin/DoubleArray", "DoubleArray", b_prim_size),
    syn("kotlin/FloatArray", "FloatArray", b_prim_size),
    syn("kotlin/BooleanArray", "BooleanArray", b_prim_size),
    syn("kotlin/CharArray", "CharArray", b_prim_size),
    syn("kotlin/ByteArray", "ByteArray", b_prim_size),
    syn("kotlin/ShortArray", "ShortArray", b_prim_size),
    // Reference creators.
    syn("kotlin/arrayOf", "arrayOf", b_ref_vararg),
    syn("kotlin/Array", "Array", b_ref_array),
    syn("kotlin/emptyArray", "emptyArray", b_empty),
    syn("kotlin/arrayOfNulls", "arrayOfNulls", b_arr_nulls),
];

/// The primitive element of an array creator whose name fixes it (`IntArray`/`intArrayOf` → `Int`).
/// Local to the array bodies — kept out of the core `Synthetic` so the registry stays general.
fn prim_elem(name: &str) -> Option<Ty> {
    Some(match name {
        "intArrayOf" | "IntArray" => Ty::Int,
        "longArrayOf" | "LongArray" => Ty::Long,
        "doubleArrayOf" | "DoubleArray" => Ty::Double,
        "floatArrayOf" | "FloatArray" => Ty::Float,
        "booleanArrayOf" | "BooleanArray" => Ty::Boolean,
        "charArrayOf" | "CharArray" => Ty::Char,
        "byteArrayOf" | "ByteArray" => Ty::Byte,
        "shortArrayOf" | "ShortArray" => Ty::Short,
        _ => return None,
    })
}

/// Lower each argument to a `Vararg` of `elem` (`int[]`/`T[]`). A branchy element is declined (its
/// stackmap frame would strand the partially-built array).
fn vararg_of(lw: &mut Lower<'_>, elem: Ty, args: &[AstExprId]) -> Option<ExprId> {
    // A boxed-primitive element (`arrayOf(1)`/`emptyArray<Int>()` → `Integer[]`) would need each value
    // boxed into the reference array; declined for now (the type + element access are supported, but
    // constructing a boxed primitive array is future work). `intArrayOf` passes a primitive `Ty` here
    // (not a boxed `Obj`), so it is unaffected.
    if elem.unboxed_primitive().is_some() {
        return None;
    }
    let elem_ir = ty_to_ir(elem);
    let mut elements = Vec::new();
    for &arg in args {
        if lw.synth_is_branchy(arg) {
            return None;
        }
        elements.push(lw.lower_arg(arg, &elem_ir)?);
    }
    Some(lw.emit(IrExpr::Vararg {
        element_type: elem_ir,
        elements,
    }))
}

// ---- IR bodies ------------------------------------------------------------------------------------

/// `intArrayOf(1, 2, 3)` → a primitive `Vararg`.
fn b_prim_vararg(syn: &'static Synthetic, lw: &mut Lower<'_>, c: &SynthCall<'_>) -> Option<ExprId> {
    vararg_of(lw, prim_elem(syn.name)?, c.args)
}

/// `IntArray(n)` → the `kotlin/IntArray.<init>` allocation intrinsic; `IntArray(n) { i -> e }` → a
/// fill loop. Other arities decline.
fn b_prim_size(syn: &'static Synthetic, lw: &mut Lower<'_>, c: &SynthCall<'_>) -> Option<ExprId> {
    let elem = prim_elem(syn.name)?;
    match c.args.len() {
        1 => {
            let size = lw.synth_expr(c.args[0])?;
            Some(lw.emit(IrExpr::Call {
                callee: Callee::External(format!("{}.<init>", syn.fqn)),
                dispatch_receiver: None,
                args: vec![size],
            }))
        }
        2 => {
            let (params, body) = lw.synth_arg_lambda(c.args[1])?;
            lw.build_fill_array(elem, c.args[0], params, body)
        }
        _ => None,
    }
}

/// `arrayOf(a, b, c)` → a reference `Vararg` (the checker already typed the call `Array<T>` and
/// rejected a primitive element).
fn b_ref_vararg(_syn: &'static Synthetic, lw: &mut Lower<'_>, c: &SynthCall<'_>) -> Option<ExprId> {
    let elem = lw.synth_array_elem(c.call)?;
    if !elem.is_reference() {
        return None;
    }
    vararg_of(lw, elem, c.args)
}

/// `Array<T>(n) { i -> e }` → a fill loop over a reference array. Declines a primitive element (a boxed
/// `Array<Int>` is `Integer[]`, modeled elsewhere) and a non-lambda call.
fn b_ref_array(_syn: &'static Synthetic, lw: &mut Lower<'_>, c: &SynthCall<'_>) -> Option<ExprId> {
    if c.args.len() != 2 {
        return None;
    }
    let elem = lw
        .synth_array_elem(c.call)
        .filter(|t| t.is_reference() && t.unboxed_primitive().is_none())?;
    let (params, body) = lw.synth_arg_lambda(c.args[1])?;
    lw.build_fill_array(elem, c.args[0], params, body)
}

/// `emptyArray<T>()` → an empty `Vararg` of the reified element (`new T[0]`).
fn b_empty(_syn: &'static Synthetic, lw: &mut Lower<'_>, c: &SynthCall<'_>) -> Option<ExprId> {
    let elem = lw.synth_array_elem(c.call)?;
    vararg_of(lw, elem, &[])
}

/// `arrayOfNulls<T>(n)` → `new T[n]` (a reference array of nulls).
fn b_arr_nulls(_syn: &'static Synthetic, lw: &mut Lower<'_>, c: &SynthCall<'_>) -> Option<ExprId> {
    if c.args.len() != 1 {
        return None;
    }
    let elem = lw
        .synth_array_elem(c.call)
        .filter(|t| t.is_reference() && t.unboxed_primitive().is_none())?;
    let size = lw.lower_arg(c.args[0], &ty_to_ir(Ty::Int))?;
    Some(lw.emit(IrExpr::NewArray {
        element_type: ty_to_ir(elem),
        size,
    }))
}
