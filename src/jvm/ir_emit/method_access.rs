//! JVM access flags of a declared function's method, following kotlinc's `calculateMethodFlags`.

use crate::ir::IrFile;
use crate::jvm::classfile::{
    ACC_BRIDGE, ACC_FINAL, ACC_PRIVATE, ACC_PUBLIC, ACC_STATIC, ACC_SYNTHETIC, ACC_VARARGS,
};

use super::{is_high_arity_function, IrExpr, LambdaMode, LambdaModes};

/// The access word of the method emitted for `fid`. `holder_static` is an interface member's body
/// realized as a static on its holder class, which takes the receiver as its first parameter.
pub(super) fn declared_method_access(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    instance: bool,
    holder_static: bool,
    lambda_modes: LambdaModes,
) -> u16 {
    let private = ir.private_methods.contains(&fid);
    let owner_is_iface = ir
        .classes
        .iter()
        .any(|o| o.fq_name_matches(owner) && o.is_interface);
    let access = if holder_static {
        // STATIC, with the member's own visibility: a private interface member's body is a PRIVATE
        // static on the holder, as kotlinc emits it.
        if private {
            ACC_PRIVATE | ACC_STATIC
        } else {
            ACC_PUBLIC | ACC_STATIC
        }
    } else if instance {
        // Top-level/`static` functions are always `final` (kotlinc emits `public static final`). An
        // instance method of a *final* class (nothing extends it) is also `final` and can never be
        // overridden, so marking it is safe; in an open/extended class we conservatively leave it
        // non-`final` (a method-level `open`/`override` model would refine this).
        // kotlinc keeps an `Object`-override (a data class's toString/hashCode/equals) open even in a
        // final class, so honor `open_methods`; otherwise a method of a final class is itself final.
        let final_class = !ir.classes.iter().any(|o| o.superclass_matches(owner));
        // An interface default method must NOT be `final` (the JVM rejects a final interface method).
        let fin = final_class && !ir.open_methods.contains(&fid) && !owner_is_iface;
        // A `private set` setter is `private final` (kotlinc); else `public` (+`final` per above).
        let vis = if private { ACC_PRIVATE } else { ACC_PUBLIC };
        // A private method is `final` on a CLASS, but a private INTERFACE method must NOT carry `ACC_FINAL`
        // (`ClassFormatError: illegal modifiers 0x12`) — private already makes it non-virtual.
        vis | if fin || (private && !owner_is_iface) {
            ACC_FINAL
        } else {
            0
        }
    } else {
        // A `static` method is `<vis> static final` (kotlinc) — EXCEPT on an interface, where a `final`
        // static method is illegal (`ClassFormatError`), or a value class's `constructor-impl`/
        // `<name>-impl` delegate members, which kotlinc emits `public static` (non-`final`) and marks via
        // `open_methods`. `box-impl`/`equals-impl0` stay `public static final` (not opened). Visibility
        // derives from the member's own (a private declaration — or a lambda impl, which kotlinc always
        // emits private — is `ACC_PRIVATE`).
        let vis = if private {
            // Under `-Xlambdas=class` the body is called from the lambda's OWN class, so a private
            // impl would be an `IllegalAccessError` at the delegating `invoke`. kotlinc has no such
            // method to place — it moves the body into `invoke` — so package-private here is the
            // narrowest visibility that keeps the delegation working.
            // A static interface method must carry exactly one of ACC_PUBLIC / ACC_PRIVATE
            // (JVMS 4.6), so an interface's impl opens all the way to public instead.
            match (
                lambda_impl_uses_class_strategy(ir, fid, lambda_modes),
                owner_is_iface,
            ) {
                (true, true) => ACC_PUBLIC,
                (true, false) => 0,
                (false, _) => ACC_PRIVATE,
            }
        } else {
            ACC_PUBLIC
        };
        if owner_is_iface || ir.open_methods.contains(&fid) {
            vis | ACC_STATIC
        } else {
            vis | ACC_STATIC | ACC_FINAL
        }
    };
    // A value class's `box-impl`/`unbox-impl` are compiler-manufactured box adapters — kotlinc marks
    // them `ACC_SYNTHETIC`.
    let synthetic = if ir.synthetic_methods.contains(&fid) {
        ACC_SYNTHETIC
    } else {
        0
    };
    let bridge = if ir.bridge_methods.contains(&fid) {
        ACC_BRIDGE
    } else {
        0
    };
    access | synthetic | bridge | varargs_access(ir, fid)
}

/// kotlinc sets `ACC_VARARGS` exactly when the method's LAST physical parameter is the declared
/// `vararg`, so Java may call it in element form. Captured values and receivers lead the physical
/// list and do not move it; a suspend function's trailing continuation does.
fn varargs_access(ir: &IrFile, fid: u32) -> u16 {
    let trailing = ir.fn_varargs.get(&fid).is_some_and(|vararg| vararg.is_last);
    if trailing && !ir.suspend_funs.contains(&fid) {
        ACC_VARARGS
    } else {
        0
    }
}

/// Whether `fid` is the implementation selected for a class-realized closure. This consumes the
/// explicit IR edge from a lambda to its implementation; generated method spelling is never used as
/// identity, and mixed `-Xlambdas`/`-Xsam-conversions` modes select only the matching closure kind.
fn lambda_impl_uses_class_strategy(ir: &IrFile, fid: u32, modes: LambdaModes) -> bool {
    ir.exprs.iter().any(|expression| {
        let IrExpr::Lambda {
            impl_fn,
            arity,
            sam,
            ..
        } = expression
        else {
            return false;
        };
        *impl_fn == fid
            && (sam.as_ref().is_some_and(|target| target.function_adapter)
                || modes.for_sam(sam.is_some()) == LambdaMode::Class
                || (sam.is_none() && is_high_arity_function(*arity)))
    })
}

/// `ACC_VARARGS` for a primary constructor whose last physical parameter is its `vararg`.
pub(super) fn primary_constructor_varargs(class: &crate::ir::IrClass) -> u16 {
    if class
        .ctor_args
        .last()
        .is_some_and(|argument| argument.is_vararg)
    {
        ACC_VARARGS
    } else {
        0
    }
}

/// `ACC_VARARGS` for a secondary constructor whose last declared parameter is its `vararg`. An
/// owner's prefix (an enum's name and ordinal, an outer instance) leads the physical list.
pub(super) fn secondary_constructor_varargs(constructor: &crate::ir::IrSecondaryCtor) -> u16 {
    let last = constructor.named_params.len().checked_sub(1);
    if constructor.vararg_index.is_some() && constructor.vararg_index == last {
        ACC_VARARGS
    } else {
        0
    }
}
