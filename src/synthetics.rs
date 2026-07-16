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
use crate::types::Ty;

/// A call site matched against the registry: the call's argument AST ids and the call expression
/// itself (so a body can read the checker-inferred result element type).
pub struct SynthCall<'a> {
    pub args: &'a [AstExprId],
    pub call: AstExprId,
}

pub(crate) trait SyntheticIrBuilder {
    fn emit(&mut self, expr: IrExpr) -> ExprId;
    fn lower_arg(&mut self, expr: AstExprId, target: &Ty) -> Option<ExprId>;
    fn synth_expr(&mut self, expr: AstExprId) -> Option<ExprId>;
    fn synth_is_branchy(&self, expr: AstExprId) -> bool;
    fn synth_array_elem(&self, call: AstExprId) -> Option<Ty>;
    fn synth_arg_lambda(&self, arg: AstExprId) -> Option<(Vec<String>, AstExprId)>;
    fn build_fill_array(
        &mut self,
        elem: Ty,
        size_arg: AstExprId,
        params: Vec<String>,
        body: AstExprId,
    ) -> Option<ExprId>;
}

/// Builds a synthetic call body, or returns `None` to fall through to normal lowering.
pub(crate) type BodyFn =
    fn(&'static Synthetic, &mut dyn SyntheticIrBuilder, &SynthCall<'_>) -> Option<ExprId>;

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
    // Unsigned vararg literals — `uintArrayOf(1u, 2u): UIntArray`. The element is `UInt`/`ULong`; the
    // physical array is the unboxed `[I`/`[J` (see `ir_lower`'s `Ty::Array(UInt)` mapping).
    syn("kotlin/uintArrayOf", "uintArrayOf", b_prim_vararg),
    syn("kotlin/ulongArrayOf", "ulongArrayOf", b_prim_vararg),
    // Primitive size constructors — `IntArray(n)` / `IntArray(n) { i -> e }`.
    syn("kotlin/IntArray", "IntArray", b_prim_size),
    syn("kotlin/LongArray", "LongArray", b_prim_size),
    syn("kotlin/DoubleArray", "DoubleArray", b_prim_size),
    syn("kotlin/FloatArray", "FloatArray", b_prim_size),
    syn("kotlin/BooleanArray", "BooleanArray", b_prim_size),
    syn("kotlin/CharArray", "CharArray", b_prim_size),
    syn("kotlin/ByteArray", "ByteArray", b_prim_size),
    syn("kotlin/ShortArray", "ShortArray", b_prim_size),
    // Unsigned size constructors — `UIntArray(n) { i -> e }` (unboxed `[I`/`[J`).
    syn("kotlin/UIntArray", "UIntArray", b_prim_size),
    syn("kotlin/ULongArray", "ULongArray", b_prim_size),
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
        "uintArrayOf" | "UIntArray" => Ty::UInt,
        "ulongArrayOf" | "ULongArray" => Ty::ULong,
        _ => return None,
    })
}

/// Lower each argument to a `Vararg` of `elem` (`int[]`/`T[]`/`Integer[]`). A branchy element is declined
/// (its stackmap frame would strand the partially-built array). A boxed-primitive element (`arrayOf(1)` →
/// `Integer[]`) is allocated as the wrapper array (the emitter's `array_element_jvm`); each value is boxed
/// by `lower_arg` / the Vararg emit. `intArrayOf` passes a primitive `Ty` here, so it stays `[I`.
fn vararg_of(lw: &mut dyn SyntheticIrBuilder, elem: Ty, args: &[AstExprId]) -> Option<ExprId> {
    let mut elements = Vec::new();
    for &arg in args {
        if lw.synth_is_branchy(arg) {
            return None;
        }
        elements.push(lw.lower_arg(arg, &elem)?);
    }
    // The whole array type (`kotlin/IntArray` / `kotlin/Array<Int>` / `kotlin/Array<String>`) drives the
    // emitter — a boxed `Array<Int>` becomes `Integer[]`, a primitive `IntArray` stays `[I`.
    Some(lw.emit(IrExpr::Vararg {
        array_type: Ty::array(elem),
        elements,
    }))
}

// ---- IR bodies ------------------------------------------------------------------------------------

/// `intArrayOf(1, 2, 3)` → a primitive `Vararg`.
fn b_prim_vararg(
    syn: &'static Synthetic,
    lw: &mut dyn SyntheticIrBuilder,
    c: &SynthCall<'_>,
) -> Option<ExprId> {
    vararg_of(lw, prim_elem(syn.name)?, c.args)
}

/// `IntArray(n)` → the `kotlin/IntArray.<init>` allocation intrinsic; `IntArray(n) { i -> e }` → a
/// fill loop. Other arities decline.
fn b_prim_size(
    syn: &'static Synthetic,
    lw: &mut dyn SyntheticIrBuilder,
    c: &SynthCall<'_>,
) -> Option<ExprId> {
    let elem = prim_elem(syn.name)?;
    match c.args {
        [size_arg] => {
            let size = lw.synth_expr(*size_arg)?;
            // The allocation intrinsic keys on the PHYSICAL primitive array class. An unsigned array is
            // the unboxed underlying signed array (`UIntArray` = `[I`, `ULongArray` = `[J`), so allocate
            // via `kotlin/IntArray.<init>` / `kotlin/LongArray.<init>` — the emitter has no
            // `kotlin/UIntArray.<init>`. Signed creators already name their physical class.
            let init_fqn = match elem {
                Ty::UInt => "kotlin/IntArray",
                Ty::ULong => "kotlin/LongArray",
                _ => syn.fqn,
            };
            Some(lw.emit(IrExpr::Call {
                callee: Callee::External(format!("{init_fqn}.<init>")),
                dispatch_receiver: None,
                args: vec![size],
            }))
        }
        [size_arg, init_arg] => {
            let (params, body) = lw.synth_arg_lambda(*init_arg)?;
            lw.build_fill_array(elem, *size_arg, params, body)
        }
        _ => None,
    }
}

/// `arrayOf(a, b, c)` → a reference `Vararg` (the checker already typed the call `Array<T>` and
/// rejected a primitive element).
fn b_ref_vararg(
    _syn: &'static Synthetic,
    lw: &mut dyn SyntheticIrBuilder,
    c: &SynthCall<'_>,
) -> Option<ExprId> {
    // Box a primitive element (`arrayOf(1,2,3)` → `Integer[]`); a reference element is unchanged.
    let elem = lw
        .synth_array_elem(c.call)
        .map(|t| t.boxed_ref().unwrap_or(t))
        .filter(|t| t.is_reference())?;
    vararg_of(lw, elem, c.args)
}

/// `Array<T>(n) { i -> e }` → a fill loop over a reference array. The element is a reference, or a boxed
/// primitive (`Array<Int>` = `Integer[]`): `build_fill_array` allocates the wrapper array and
/// `kotlin/Array.set` boxes each filled value. Declines a non-lambda call.
fn b_ref_array(
    _syn: &'static Synthetic,
    lw: &mut dyn SyntheticIrBuilder,
    c: &SynthCall<'_>,
) -> Option<ExprId> {
    let [size_arg, init_arg] = c.args else {
        return None;
    };
    // Box a primitive element so the array is `Integer[]` (the checker types `Array(n){…}` as
    // `Obj("kotlin/Array", [Int])`, element exposed unboxed). A reference element is unchanged.
    let elem = lw
        .synth_array_elem(c.call)
        .map(|t| t.boxed_ref().unwrap_or(t))
        .filter(|t| t.is_reference())?;
    let (params, body) = lw.synth_arg_lambda(*init_arg)?;
    lw.build_fill_array(elem, *size_arg, params, body)
}

/// `emptyArray<T>()` → an empty `Vararg` of the reified element (`new T[0]`).
fn b_empty(
    _syn: &'static Synthetic,
    lw: &mut dyn SyntheticIrBuilder,
    c: &SynthCall<'_>,
) -> Option<ExprId> {
    let elem = lw.synth_array_elem(c.call)?;
    vararg_of(lw, elem, &[])
}

/// `arrayOfNulls<T>(n)` → `new T[n]` (a reference array of nulls; a boxed primitive `Array<Int?>` =
/// `Integer[]`).
fn b_arr_nulls(
    _syn: &'static Synthetic,
    lw: &mut dyn SyntheticIrBuilder,
    c: &SynthCall<'_>,
) -> Option<ExprId> {
    let [size_arg] = c.args else { return None };
    let elem = lw
        .synth_array_elem(c.call)
        .map(|t| t.boxed_ref().unwrap_or(t))
        .filter(|t| t.is_reference())?;
    let size = lw.lower_arg(*size_arg, &Ty::Int)?;
    Some(lw.emit(IrExpr::NewArray {
        array_type: Ty::array(elem),
        size,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_each_registered_synthetic_by_source_name() {
        // Every TABLE entry is discoverable by its source call name, and the returned identity
        // (fqn/name) matches the entry — the identity shared with the JVM intrinsic registry.
        for s in TABLE {
            let got = lookup(s.name).expect("registered name must resolve");
            assert_eq!(got.name, s.name);
            assert_eq!(got.fqn, s.fqn);
        }
    }

    #[test]
    fn lookup_declines_unknown_name() {
        assert!(lookup("nope").is_none());
        assert!(lookup("").is_none());
        // A real classpath function that is deliberately NOT synthetic must not match.
        assert!(lookup("listOf").is_none());
        assert!(lookup("println").is_none());
    }

    #[test]
    fn lookup_covers_the_documented_creator_families() {
        // Vararg + size primitive families, plus the four reference creators.
        for name in [
            "intArrayOf",
            "longArrayOf",
            "IntArray",
            "LongArray",
            "uintArrayOf",
            "ulongArrayOf",
            "UIntArray",
            "ULongArray",
            "arrayOf",
            "Array",
            "emptyArray",
            "arrayOfNulls",
        ] {
            assert!(lookup(name).is_some(), "{name} should be synthetic");
        }
    }

    #[test]
    fn prim_elem_maps_both_the_vararg_and_size_spellings() {
        assert_eq!(prim_elem("intArrayOf"), Some(Ty::Int));
        assert_eq!(prim_elem("IntArray"), Some(Ty::Int));
        assert_eq!(prim_elem("longArrayOf"), Some(Ty::Long));
        assert_eq!(prim_elem("LongArray"), Some(Ty::Long));
        assert_eq!(prim_elem("doubleArrayOf"), Some(Ty::Double));
        assert_eq!(prim_elem("DoubleArray"), Some(Ty::Double));
        assert_eq!(prim_elem("floatArrayOf"), Some(Ty::Float));
        assert_eq!(prim_elem("FloatArray"), Some(Ty::Float));
        assert_eq!(prim_elem("booleanArrayOf"), Some(Ty::Boolean));
        assert_eq!(prim_elem("BooleanArray"), Some(Ty::Boolean));
        assert_eq!(prim_elem("charArrayOf"), Some(Ty::Char));
        assert_eq!(prim_elem("CharArray"), Some(Ty::Char));
        assert_eq!(prim_elem("byteArrayOf"), Some(Ty::Byte));
        assert_eq!(prim_elem("ByteArray"), Some(Ty::Byte));
        assert_eq!(prim_elem("shortArrayOf"), Some(Ty::Short));
        assert_eq!(prim_elem("ShortArray"), Some(Ty::Short));
        assert_eq!(prim_elem("uintArrayOf"), Some(Ty::UInt));
        assert_eq!(prim_elem("UIntArray"), Some(Ty::UInt));
        assert_eq!(prim_elem("ulongArrayOf"), Some(Ty::ULong));
        assert_eq!(prim_elem("ULongArray"), Some(Ty::ULong));
    }

    #[test]
    fn prim_elem_declines_reference_creators_and_unknowns() {
        // The reference creators (`arrayOf`, `Array`, …) are not primitive-element-fixed.
        assert_eq!(prim_elem("arrayOf"), None);
        assert_eq!(prim_elem("Array"), None);
        assert_eq!(prim_elem("emptyArray"), None);
        assert_eq!(prim_elem("arrayOfNulls"), None);
        assert_eq!(prim_elem("whatever"), None);
    }

    #[test]
    fn syn_constructs_the_expected_identity() {
        let s = syn("kotlin/intArrayOf", "intArrayOf", b_prim_vararg);
        assert_eq!(s.fqn, "kotlin/intArrayOf");
        assert_eq!(s.name, "intArrayOf");
    }

    #[test]
    fn registered_source_names_are_unique() {
        // lookup() returns the FIRST match; a duplicate source name would silently shadow — assert none.
        let mut names: Vec<&str> = TABLE.iter().map(|s| s.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "synthetic source names must be unique");
    }
}
