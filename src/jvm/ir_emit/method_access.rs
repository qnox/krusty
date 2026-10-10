//! JVM access flags of a declared function's method, following kotlinc's `calculateMethodFlags`.

use crate::ir::IrFile;
use crate::jvm::classfile::{
    ACC_ABSTRACT, ACC_BRIDGE, ACC_FINAL, ACC_PRIVATE, ACC_PROTECTED, ACC_PUBLIC, ACC_STATIC,
    ACC_SYNTHETIC, ACC_VARARGS,
};

use super::{IrExpr, LambdaMode, LambdaModes};

fn jvm_visibility(visibility: crate::types::Visibility) -> u16 {
    match visibility {
        crate::types::Visibility::Private => ACC_PRIVATE,
        crate::types::Visibility::Protected => ACC_PROTECTED,
        crate::types::Visibility::Internal | crate::types::Visibility::Public => ACC_PUBLIC,
        crate::types::Visibility::PackagePrivate => 0,
    }
}

/// The access word of a body-less (abstract) method declaration, following kotlinc's
/// `calculateMethodFlags`: the member's own visibility and `ACC_ABSTRACT`, never `ACC_FINAL`.
/// Interface members record `public`, so their flags are unchanged.
pub(super) fn abstract_method_access(ir: &IrFile, fid: u32) -> u16 {
    jvm_visibility(ir.method_visibility(fid)) | ACC_ABSTRACT | varargs_access(ir, fid)
}

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
    let visibility = ir.method_visibility(fid);
    let private = visibility.is_private();
    let owner_is_iface = ir
        .classes
        .iter()
        .any(|o| o.is_interface && o.fq_name_matches(owner));
    let access = if holder_static {
        // STATIC, with the member's own visibility: a private interface member's body is a PRIVATE
        // static on the holder, as kotlinc emits it.
        jvm_visibility(visibility) | ACC_STATIC
    } else if instance {
        // `ACC_FINAL` follows the member's own Kotlin modality, whatever the class's: an `open`,
        // `abstract` or non-`final` `override` member stays overridable even in a final class, and
        // any other member is final even in an open one. A private member is final by default
        // too, but not when a compiler plugin (all-open) made `open` its default modality, which
        // kotlinc emits without `ACC_FINAL`. An interface method is never final (the JVM rejects
        // it: `illegal modifiers 0x12`).
        let fin = !ir.open_methods.contains(&fid) && !owner_is_iface;
        let vis = jvm_visibility(visibility);
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
            jvm_visibility(visibility)
        };
        if owner_is_iface || class_suspend_impl || ir.open_methods.contains(&fid) {
            vis | ACC_STATIC
        } else {
            vis | ACC_STATIC | ACC_FINAL
        }
    };
    // A value class's `box-impl`/`unbox-impl` are compiler-manufactured box adapters — kotlinc marks
    // them `ACC_SYNTHETIC`, and so it does a reifiable function, which Java cannot call.
    let synthetic = if ir.synthetic_methods.contains(&fid) || is_reifiable(ir, fid) {
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

/// kotlinc's `isReifiable`: the function declares a `reified` type parameter. Its erased body only
/// works once a call site substitutes the type, so kotlinc emits the method `ACC_SYNTHETIC` and
/// annotates none of its nullability.
pub(super) fn is_reifiable(ir: &IrFile, fid: u32) -> bool {
    ir.signatures.get(&fid).is_some_and(|signature| {
        signature
            .type_params
            .iter()
            .any(|parameter| parameter.reified)
    })
}

/// kotlinc sets `ACC_VARARGS` exactly when the method's LAST physical parameter is the declared
/// `vararg`, so Java may call it in element form. Captured values and receivers lead the physical
/// list and do not move it; a suspend function's trailing continuation does.
pub(super) fn varargs_access(ir: &IrFile, fid: u32) -> u16 {
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
pub(super) fn class_realized_lambda_method(
    ir: &IrFile,
    fid: u32,
    modes: LambdaModes,
) -> Option<&str> {
    let runtime_reified = ir.runtime_reified_lambda_implementations.contains(&fid)
        || ir.inline_anonymous_lambdas.contains(&fid);
    ir.exprs.iter().find_map(|expression| {
        let IrExpr::Lambda {
            impl_fn,
            arity,
            captures,
            sam,
            ..
        } = expression
        else {
            return None;
        };
        let bounded_erasure = sam.as_ref().is_some_and(|target| {
            super::lambda_class::bounded_erasure_needs_class(ir, *impl_fn, target, captures.len())
        });
        (*impl_fn == fid
            && (runtime_reified
                || bounded_erasure
                || modes.for_lambda(sam.as_ref(), *arity) == LambdaMode::Class))
            .then_some(
                sam.as_ref()
                    .map_or("invoke", |target| target.method.as_str()),
            )
    })
}

pub(super) fn lambda_impl_uses_class_strategy(ir: &IrFile, fid: u32, modes: LambdaModes) -> bool {
    class_realized_lambda_method(ir, fid, modes).is_some()
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
/// subclasses construct through that public synthetic accessor. An anonymous object's constructor is
/// package-private, except one declared in an `inline` function: inlining copies its `new` into
/// every call site, including a caller in another package, so that constructor is public. A
/// continuation class's constructor is package-private. A `vararg` last parameter adds
/// `ACC_VARARGS`.
pub(super) fn primary_constructor_access(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    is_continuation: bool,
    value_param_ctor: bool,
) -> u16 {
    let access = if class.is_anonymous_object && anonymous_object_in_inline_function(ir, class) {
        ACC_PUBLIC
    } else if is_continuation || class.is_anonymous_object {
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

/// An anonymous object declared directly in an `inline` function. Inlining copies its `new` into
/// each call site, which may live in another package, so the constructor has to be public. The
/// class itself stays in the function's package.
fn anonymous_object_in_inline_function(ir: &IrFile, class: &crate::ir::IrClass) -> bool {
    let Some(crate::ir::IrEnclosure::Function(function)) = class.enclosure.as_ref() else {
        return false;
    };
    ir.inline_fns.contains(function)
}
