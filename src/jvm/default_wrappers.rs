//! Which declarations the JVM writes a default-argument wrapper for, and the defaults each fills:
//! kotlinc's `name$default(…, mask, marker)` method and `<init>(…, mask, DefaultConstructorMarker)`
//! constructor. Emission writes exactly these wrappers, and an analysis of the code the class files
//! hold reads the same defaults from here.

use crate::fx_hash::HashSet;
use crate::ir::{ClassId, EmittedDeclaration, ExprId, FunId, IrClass, IrFile, IrSecondaryCtor};

/// The defaults the facade's `$default` wrapper of top-level function `fid` fills, when it writes
/// one: not for a default the wrapper's frame cannot evaluate (`toplevel_default_stub_safe`).
pub(crate) fn top_level_function(ir: &IrFile, fid: FunId) -> Option<&[Option<ExprId>]> {
    crate::ir::toplevel_default_stub_safe(ir, fid)
        .then(|| ir.param_defaults(fid))
        .flatten()
        .map(Vec::as_slice)
}

/// The member whose `$default` wrapper follows method `fid` in its class, and the defaults that
/// wrapper fills: its own declared defaults, or, after the `$suspendImpl` a member's body moved to,
/// that member's (`default_stub_after`).
pub(crate) fn member_function(ir: &IrFile, fid: FunId) -> Option<(FunId, &[Option<ExprId>])> {
    let declaration = crate::jvm::suspend_impls::default_stub_after(ir, fid)?;
    let defaults = ir.declared_param_defaults(declaration)?;
    Some((declaration, defaults.as_slice()))
}

/// The defaults the wrapper of `class`'s primary constructor fills, when it writes one.
pub(crate) fn primary_constructor<'a>(
    ir: &'a IrFile,
    class: &IrClass,
) -> Option<&'a [Option<ExprId>]> {
    ir.class_ctor_defaults_name(class.fq_name_id())
        .filter(|defaults| defaults.iter().any(Option::is_some))
        .map(Vec::as_slice)
}

/// The defaults the wrapper of secondary constructor `constructor` fills, when it writes one.
pub(crate) fn secondary_constructor(constructor: &IrSecondaryCtor) -> Option<&[Option<ExprId>]> {
    constructor
        .defaults
        .iter()
        .any(Option::is_some)
        .then_some(constructor.defaults.as_slice())
}

/// Every default a written wrapper evaluates, with the declaration that wrapper belongs to: the
/// roots the class files hold beyond the declarations' own bodies. A default no wrapper fills is
/// not here.
pub(crate) fn evaluated_defaults(ir: &IrFile) -> Vec<(EmittedDeclaration, ExprId)> {
    let mut roots = Vec::new();
    let mut members = HashSet::default();
    for (class_id, class) in ir.classes.iter().enumerate() {
        let class_id = ClassId::try_from(class_id).expect("too many classes for a class id");
        members.extend(class.methods.iter().copied());
        for &method in &class.methods {
            if let Some((declaration, defaults)) = member_function(ir, method) {
                let declaration = EmittedDeclaration::Function(declaration);
                roots.extend(defaults.iter().flatten().map(|&root| (declaration, root)));
            }
        }
        if let Some(defaults) = primary_constructor(ir, class) {
            let declaration = EmittedDeclaration::PrimaryConstructor(class_id);
            roots.extend(defaults.iter().flatten().map(|&root| (declaration, root)));
        }
        for (ordinal, constructor) in class.secondary_ctors.iter().enumerate() {
            if let Some(defaults) = secondary_constructor(constructor) {
                let declaration = EmittedDeclaration::SecondaryConstructor {
                    class: class_id,
                    ordinal,
                };
                roots.extend(defaults.iter().flatten().map(|&root| (declaration, root)));
            }
        }
    }
    for (function, declaration) in ir.functions.iter().enumerate() {
        let function = FunId::try_from(function).expect("too many functions for a function id");
        let on_facade = !members.contains(&function)
            && declaration.dispatch_receiver.is_none()
            && declaration.body.is_some()
            && !ir.inline_only_fns.contains(&function);
        if let Some(defaults) = on_facade
            .then(|| top_level_function(ir, function))
            .flatten()
        {
            let declaration = EmittedDeclaration::Function(function);
            roots.extend(defaults.iter().flatten().map(|&root| (declaration, root)));
        }
    }
    roots
}
