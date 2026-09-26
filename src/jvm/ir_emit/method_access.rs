//! JVM access flags of a declared function's method, following kotlinc's `calculateMethodFlags`.

use crate::ir::IrFile;
use crate::jvm::classfile::{
    ACC_BRIDGE, ACC_FINAL, ACC_PRIVATE, ACC_PROTECTED, ACC_PUBLIC, ACC_STATIC, ACC_SYNTHETIC,
    ACC_VARARGS,
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
        // `ACC_FINAL` follows the member's own Kotlin modality, whatever the class's: an `open`,
        // `abstract` or non-`final` `override` member stays overridable even in a final class, and
        // any other member is final even in an open one. A private member is final too. An
        // interface method is never final (the JVM rejects it: `illegal modifiers 0x12`).
        let fin = (private || !ir.open_methods.contains(&fid)) && !owner_is_iface;
        // A `private set` setter is `private final` (kotlinc); else `public`.
        let vis = if private { ACC_PRIVATE } else { ACC_PUBLIC };
        vis | if fin { ACC_FINAL } else { 0 }
    } else {
        // A `static` method is `<vis> static final` (kotlinc) — EXCEPT on an interface, where a `final`
        // static method is illegal (`ClassFormatError`), or a value class's `constructor-impl`/
        // `<name>-impl` delegate members, which kotlinc emits `public static` (non-`final`) and marks via
        // `open_methods`. `box-impl`/`equals-impl0` stay `public static final` (not opened). Visibility
        // derives from the member's own (a private declaration — or a lambda impl, which kotlinc always
        // emits private — is `ACC_PRIVATE`).
        // A class member's `$suspendImpl` is package-private, as kotlinc writes it.
        let class_suspend_impl = !owner_is_iface && ir.jvm_suspend_impl_bodies.contains_key(&fid);
        let vis = if class_suspend_impl {
            0
        } else if private {
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
        if owner_is_iface || class_suspend_impl || ir.open_methods.contains(&fid) {
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

/// The access word of a class's primary `<init>`.
///
/// An `object`'s constructor is private; a `@JvmInline value class`'s is private and synthetic
/// (instances are created via `constructor-impl`/`box-impl`, never `new`); a class whose primary
/// constructor takes a value-class-typed parameter is private (kotlinc routes construction through a
/// synthetic `(…args, DefaultConstructorMarker)` accessor); a SEALED class's is private too, since
/// subclasses construct through that public synthetic accessor. A `vararg` last parameter adds
/// `ACC_VARARGS`.
pub(super) fn primary_constructor_access(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    is_continuation: bool,
    value_param_ctor: bool,
) -> u16 {
    let access = if is_continuation || class.is_anonymous_object {
        // A continuation class's ctor is package-private (constructed only by its own file);
        // kotlinc gives an ANONYMOUS class's ctor the same access (flags 0x0000). This remains
        // true when a capture has value-class type: the enclosing class directly constructs the
        // anonymous class, so treating that semantic capture like a declared value-class
        // parameter would make the only reachable constructor private.
        0
    } else if class.is_value {
        ACC_PRIVATE | ACC_SYNTHETIC
    } else if class.is_singleton() || value_param_ctor || class.is_sealed {
        ACC_PRIVATE
    } else {
        // A DECLARED protected constructor reaches the JVM method too (kotlinc emits `<init>`
        // protected), and a declared PRIVATE one is ACC_PRIVATE: another class calls it
        // through its `constructor_accessors` accessor.
        match ir.ctor_visibilities.get(&class.fq_name_id()) {
            Some(crate::types::Visibility::Protected) => ACC_PROTECTED,
            Some(crate::types::Visibility::Private) => ACC_PRIVATE,
            _ => ACC_PUBLIC,
        }
    };
    access | primary_constructor_varargs(class)
}
