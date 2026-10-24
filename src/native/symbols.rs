//! The symbol each class and function of a file is emitted under.
//!
//! Every symbol a program defines shares one namespace with the runtime's, so a name is handed out
//! only once it is free of both: the runtime's own symbols are reserved first, and a repeat gets a
//! numbered suffix whose own spelling is checked in turn.

use crate::fx_hash::HashSet;
use crate::types::TyKind;

use crate::ir::IrFile;

/// A symbol-safe spelling of a Kotlin name: every non-alphanumeric character becomes `_`.
/// Symbol both files of a module use for one top-level function.
///
/// The id is the checked callable identity, so the file that defines the function and the file
/// that calls it name the same symbol without seeing each other's lowering. A suffix would break
/// that: the caller has no way to know a collision the defining file resolved locally.
pub(super) fn module_function_symbol(
    module_name: &str,
    callable: crate::fir::CallableId,
) -> String {
    format!(
        "kt_mod_{}_{}",
        module_namespace(module_name),
        callable.raw()
    )
}

/// Injective target spelling of a source-module identity.
pub(super) fn module_namespace(module_name: &str) -> String {
    let mut namespace = String::new();
    mangle_name(&mut namespace, module_name);
    namespace
}

/// Symbol of the getter entry point of a property one file of a module declares and another uses.
///
/// Same rule as [`module_function_symbol`]: the id is the checked property identity, so the file
/// that declares the property and the file that reads it name one symbol without seeing each
/// other's lowering. A `var`'s setter is [`module_property_setter_symbol`].
pub(super) fn module_property_getter_symbol(
    module_name: &str,
    property: crate::fir::PropertyId,
) -> String {
    format!(
        "kt_modprop_get_{}_{}",
        module_namespace(module_name),
        property.raw()
    )
}

/// Symbol of the setter entry point of a module `var`; see [`module_property_getter_symbol`].
pub(super) fn module_property_setter_symbol(
    module_name: &str,
    property: crate::fir::PropertyId,
) -> String {
    format!(
        "kt_modprop_set_{}_{}",
        module_namespace(module_name),
        property.raw()
    )
}

/// Symbol of the allocating entry point of a module constructor.
///
/// The symbol spells the source module plus the constructor's stable identity: the qualified name
/// of the class it constructs and its complete parameter list, in an injective encoding. It
/// depends neither on interning order nor on where the constructor sits among its class's
/// constructors, so files of one module agree while equal internal classes in unrelated modules
/// cannot collide. `None` for a parameter type that a checked signature never carries.
pub(super) fn module_constructor_symbol(
    module_name: &str,
    constructor: &crate::ir::IrModuleConstructor,
) -> Option<String> {
    let mut symbol = format!("kt_modctor_{}_", module_namespace(module_name));
    mangle_name(&mut symbol, &constructor.owner.render());
    symbol.push_str("__");
    if let Some(outer) = constructor.outer {
        symbol.push('X');
        mangle_ty(&mut symbol, crate::types::Ty::obj_name(outer))?;
    }
    for parameter in &constructor.parameters {
        mangle_ty(&mut symbol, *parameter)?;
    }
    Some(symbol)
}

/// Symbols of the entry points that box a value of a module value class and read one back, which
/// the file declaring the class defines for every other file of the module. Named from the exact
/// source module and class's qualified name, which both files hold, and not from anything either
/// file interned.
pub(super) fn module_value_box_symbols(
    module_name: &str,
    classifier: crate::types::TypeName,
) -> (String, String) {
    let mut name = String::new();
    mangle_name(&mut name, &classifier.render());
    let namespace = module_namespace(module_name);
    (
        format!("kt_modvbox_{namespace}_{name}"),
        format!("kt_modvunbox_{namespace}_{name}"),
    )
}

/// Symbol of the runtime type descriptor of a named source class, which the declaring file
/// defines and every other file of the module names in a type check, a cast or a `catch`. Named
/// from the exact source module and the class's qualified name, which every file holds.
pub(super) fn module_descriptor_symbol(
    module_name: &str,
    classifier: crate::types::TypeName,
) -> String {
    let mut symbol = format!("kt_modtype_{}_", module_namespace(module_name));
    mangle_name(&mut symbol, &classifier.render());
    symbol
}

/// Symbol of the getter of an `object` declaration's instance, which the declaring file defines
/// and every other file of the module calls to reach the object. Named from the exact source
/// module and the object's qualified name, which every file holds.
pub(super) fn module_singleton_symbol(
    module_name: &str,
    classifier: crate::types::TypeName,
) -> String {
    let mut symbol = format!("kt_modobj_{}_", module_namespace(module_name));
    mangle_name(&mut symbol, &classifier.render());
    symbol
}

/// Letters and digits stand for themselves; every other character, `_` included, is `_<hex>_`.
fn mangle_name(out: &mut String, text: &str) {
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character);
        } else {
            out.push_str(&format!("_{:x}_", character as u32));
        }
    }
}

/// One type in a prefix code: each form opens with its own tag, and a name or a list is length-
/// or terminator-delimited, so two different parameter lists never spell alike.
fn mangle_ty(out: &mut String, ty: crate::types::Ty) -> Option<()> {
    use crate::types::Ty;
    let nested = |out: &mut String, tag: char, inner: Ty| {
        out.push(tag);
        mangle_ty(out, inner)
    };
    match ty.kind() {
        TyKind::Unit => out.push('U'),
        TyKind::Null => out.push('Z'),
        TyKind::Nothing => out.push('W'),
        TyKind::Obj(name, arguments) => {
            let mut spelled = String::new();
            mangle_name(&mut spelled, &name.render());
            out.push_str(&format!("L{}_{spelled}", spelled.len()));
            for argument in arguments {
                mangle_ty(out, *argument)?;
            }
            out.push('E');
        }
        TyKind::TyParam(name, _) => {
            let mut spelled = String::new();
            mangle_name(&mut spelled, name.as_str());
            out.push_str(&format!("P{}_{spelled}", spelled.len()));
        }
        TyKind::Fun(signature) => {
            out.push_str(&format!(
                "F{}_{}_{}_",
                signature.context_count,
                u8::from(signature.has_receiver),
                u8::from(signature.suspend),
            ));
            for parameter in &signature.params {
                mangle_ty(out, *parameter)?;
            }
            out.push('R');
            mangle_ty(out, signature.ret)?;
        }
        TyKind::Intersection(components) => {
            out.push('I');
            for component in components {
                mangle_ty(out, *component)?;
            }
            out.push('E');
        }
        TyKind::Nullable(inner) => nested(out, 'N', inner)?,
        TyKind::PlatformNullable(inner) => nested(out, 'Q', inner)?,
        TyKind::InProjection(inner) => nested(out, 'M', inner)?,
        TyKind::OutProjection(inner) => nested(out, 'O', inner)?,
        TyKind::StarProjection(bound) => nested(out, 'S', bound)?,
        TyKind::RawProjection(erased) => nested(out, 'J', erased)?,
        TyKind::DefinitelyNotNull(inner) => nested(out, 'D', inner)?,
        TyKind::Error | TyKind::Pending => return None,
    }
    Some(())
}

/// Symbol of the entry another file calls when it leaves arguments of a top-level function out:
/// the function's whole parameter frame followed by a mask of the omitted ordinals. See
/// `codegen::lower::defaults`.
pub(super) fn module_default_symbol(module_name: &str, callable: crate::fir::CallableId) -> String {
    format!(
        "kt_mod_{}_{}__defaults",
        module_namespace(module_name),
        callable.raw()
    )
}

/// Symbol of a file's once-only top-level initializer. Both the file and any caller in the module
/// derive it from the source-file identity, the same way [`module_function_symbol`] is derived.
pub(super) fn file_init_symbol(module_name: &str, source: crate::fir::SourceFileId) -> String {
    format!(
        "kt_fileinit_{}_{}",
        module_namespace(module_name),
        source.raw()
    )
}

pub(super) fn c_identifier(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    out
}

/// Kotlin overloads share a name and C has no overloading, so a repeat gets a suffix. The suffixed
/// spelling is itself checked for collisions: a file declaring both `greet` and `greet__1` would
/// otherwise hand two functions the same symbol, and the linker would silently pick one of them.
/// Classes draw from the same pool as functions because their descriptors and constructors are
/// ordinary C objects too: a class `X` and a top-level function `type_X` must not both become
/// `kt_type_X`.
pub(super) struct Symbols {
    /// The module-qualified, unique base name of each class, from which
    /// `struct kt_class_<base>`, `kt_type_<base>`, `kt_<base>__init` and the struct members
    /// `<base>_f<i>` derive.
    pub classes: Vec<String>,
    /// C symbol per IR function index.
    pub functions: Vec<String>,
    /// Per IR function index, whether its symbol is the module symbol another file of the module
    /// calls it by.
    pub cross_file: Vec<bool>,
}

fn class_symbol_family(base: &str) -> [String; 6] {
    [
        format!("kt_type_{base}"),
        format!("kt_vtable_{base}"),
        format!("kt_refs_{base}"),
        format!("kt_{base}__init"),
        format!("kt_singleton_{base}"),
        format!("kt_singleton_{base}_get"),
    ]
}

/// Collision-free base of every symbol owned by a source class.
///
/// Classifiers may have the same qualified spelling in independent modules, especially for
/// `internal` declarations. Their descriptors, vtables, constructors and exported member entry
/// points therefore share this exact source-module namespace rather than a process-global FQN.
fn module_class_symbol_base(module_name: &str, fq_name: &str) -> String {
    let mut base = format!("mod_{}_", module_namespace(module_name));
    mangle_name(&mut base, fq_name);
    base
}

pub(super) fn symbols(ir: &IrFile, module_name: &str, reserved: HashSet<String>) -> Symbols {
    // The runtime's own symbols are taken before any of the program's are handed out. A Kotlin
    // `fun cast(value: Any)` would otherwise be named `kt_cast`, which the runtime already defines,
    // and the link fails with a duplicate symbol that says nothing about the Kotlin name behind it.
    let mut taken: HashSet<String> = reserved;
    // Reserved before `unique` is built: that closure borrows `taken`, so these inserts cannot
    // follow it. Every function another file can name uses the callable id: a top-level function,
    // and a member, which another file calls directly when dispatch cannot select a different body.
    let mut functions = vec![String::new(); ir.functions.len()];
    let mut cross_file = vec![false; ir.functions.len()];
    for (callable, function) in &ir.checked_callable_functions {
        let index = *function as usize;
        let Some(declared) = ir.functions.get(index) else {
            continue;
        };
        // A member of a local or anonymous class is reachable from no other file. Its callable
        // identity can also be realized in more than one file, when an inline function that
        // declares the class is expanded into a caller, so it keeps a per-file name.
        if let Some(owner) = declared.dispatch_receiver {
            let named = ir
                .class_id_by_name(owner)
                .is_some_and(|class| !ir.local_class_name_provenance.contains_key(&class));
            if !named {
                continue;
            }
        }
        let symbol = module_function_symbol(module_name, *callable);
        taken.insert(symbol.clone());
        let default_identity = match ir.inherited_param_default_provider(*function) {
            Some(provider) => crate::ir::module_override_callable(provider),
            None => Some(*callable),
        };
        if let Some(default_identity) = default_identity {
            taken.insert(module_default_symbol(module_name, default_identity));
        }
        functions[index] = symbol;
        cross_file[index] = true;
    }
    // A property's entry points are synthesized from its identity rather than being entries in
    // `functions`. Reserving them keeps a function from being handed the same spelling.
    for property in ir
        .checked_properties
        .keys()
        .chain(ir.referenced_module_properties.keys())
    {
        taken.insert(module_property_getter_symbol(module_name, *property));
        taken.insert(module_property_setter_symbol(module_name, *property));
    }
    taken.extend(
        ir.module_constructions
            .records
            .values()
            .filter_map(|constructor| module_constructor_symbol(module_name, constructor)),
    );
    let mut unique = |base: String, family: &dyn Fn(&str) -> Vec<String>| -> String {
        let mut candidate = base.clone();
        let mut ordinal = 0;
        loop {
            let names = family(&candidate);
            if names.iter().all(|name| !taken.contains(name)) {
                taken.extend(names);
                return candidate;
            }
            ordinal += 1;
            candidate = format!("{base}__{ordinal}");
        }
    };

    // Classes first: a class reserves a whole family of names, and a function only one.
    let classes: Vec<String> = ir
        .classes
        .iter()
        .map(|class| {
            unique(
                module_class_symbol_base(module_name, &class.fq_name()),
                &|base| class_symbol_family(base).to_vec(),
            )
        })
        .collect();

    let namespace = module_namespace(module_name);
    let package = ir
        .package
        .as_deref()
        .map(c_identifier)
        .filter(|package| !package.is_empty());
    // Methods are named by their class, then everything else by the package. A slot filled above
    // is a cross-file symbol and stays.
    for (class, base) in ir.classes.iter().zip(&classes) {
        for &fid in &class.methods {
            if !functions[fid as usize].is_empty() {
                continue;
            }
            let function = &ir.functions[fid as usize];
            let method = format!("kt_{base}_{}", c_identifier(&function.name));
            functions[fid as usize] = unique(method, &|name| vec![name.to_string()]);
        }
    }
    for (index, function) in ir.functions.iter().enumerate() {
        if !functions[index].is_empty() {
            continue;
        }
        // Local and compiler-generated functions have no checked callable identity. They still
        // belong to one source module: independently analyzed dependencies restart their local
        // function ids and may otherwise export identical strong symbols at the final link.
        let base = match &package {
            Some(package) => format!(
                "kt_modlocal_{namespace}_{package}_{}",
                c_identifier(&function.name)
            ),
            None => format!("kt_modlocal_{namespace}_{}", c_identifier(&function.name)),
        };
        functions[index] = unique(base, &|name| vec![name.to_string()]);
    }
    Symbols {
        classes,
        functions,
        cross_file,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrFunction;
    use crate::types::Ty;

    fn fallback_function(name: &str) -> IrFile {
        let mut ir = IrFile::default();
        ir.package = Some("sample".to_string());
        ir.functions.push(IrFunction {
            name: name.to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir
    }

    #[test]
    fn fallback_functions_are_namespaced_by_source_module() {
        let first = symbols(
            &fallback_function("local"),
            "first-module",
            HashSet::default(),
        );
        let second = symbols(
            &fallback_function("local"),
            "second-module",
            HashSet::default(),
        );

        assert_eq!(
            first.functions,
            ["kt_modlocal_first_2d_module_sample_local"]
        );
        assert_eq!(
            second.functions,
            ["kt_modlocal_second_2d_module_sample_local"]
        );
    }
}
