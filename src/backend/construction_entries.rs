//! Another module's construction of a class of this file.
//!
//! Constructing an object needs its layout, which only the declaring file's target code knows, so
//! a constructor is not called across modules the way a function is. Each constructor the file's
//! KLIB publishes instead gets an entry, an ordinary function of the file taking the constructor's
//! parameters and answering the new object. The entry is exported under the constructor's own
//! identity, so another module's construction is an ordinary call to it.
//!
//! An object declaration's instance is the same: only its file knows how it is laid out and
//! initialized, so each object the KLIB publishes gets an entry answering it, which a target
//! publishes by the object's classifier.

use super::DependencyExportSignatures;
use crate::ir::{ExprId, FunId, IrExpr, IrFile, IrFunction};
use crate::types::Ty;

/// Build the entry of every constructor `exports` publishes from `ir` and export it as an ordinary
/// callable under that constructor's identity.
pub fn realize(ir: &mut IrFile, exports: &mut DependencyExportSignatures) {
    let constructors = exports.constructors().to_vec();
    for (constructor, identity) in constructors {
        if let Some(entry) = build(ir, constructor) {
            ir.construction_entries.insert(entry, constructor.class);
            exports.record_callable(entry, identity);
        }
    }
    let singletons = exports.singletons().to_vec();
    for class in singletons {
        let classifier = ir.classes[class as usize].fq_name;
        if let Some(entry) = instance(ir, classifier) {
            ir.construction_entries.insert(entry, class);
            exports.record_singleton_entry(classifier, entry);
        }
    }
}

/// Exercise the production export realization with an exact primary-constructor identity.
#[cfg(any(test, feature = "test-support"))]
pub fn realize_primary_constructor_for_test(
    ir: &mut IrFile,
    class: crate::ir::ClassId,
    identity: super::KlibDependencyCallable,
) {
    let mut exports = DependencyExportSignatures::default();
    exports.record_constructor(
        super::ExportedConstructor {
            class,
            secondary: None,
        },
        identity,
    );
    realize(ir, &mut exports);
}

/// The entry answering the instance of the object `classifier`.
fn instance(ir: &mut IrFile, classifier: crate::types::TypeName) -> Option<FunId> {
    let result = ir.declared_class_self_type(classifier)?;
    let value = ir.add_expr(IrExpr::SingletonValue { classifier });
    ir.set_logical_type(value, result);
    Some(entry(ir, "<instance>$get", Vec::new(), result, value))
}

/// A compiler-generated static function of the file returning `value`.
fn entry(ir: &mut IrFile, name: &str, parameters: Vec<Ty>, result: Ty, value: ExprId) -> FunId {
    let last = ir.add_expr(IrExpr::Return(Some(value)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![last],
        value: None,
    });
    let entry = ir.add_fun(IrFunction {
        name: name.to_owned(),
        params: parameters,
        ret: result,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    // Compiler-generated: no source declaration of its own, and no public name.
    ir.synthetic_methods.insert(entry);
    entry
}

/// The entry constructing `constructor`'s class, when the file lays out its object.
fn build(ir: &mut IrFile, constructor: super::ExportedConstructor) -> Option<FunId> {
    let class = &ir.classes[constructor.class as usize];
    let internal = class.fq_name;
    let result = ir.declared_class_self_type(internal)?;
    let (parameters, selected, declared) = match constructor.secondary {
        None => (
            class.ctor_args.iter().map(|argument| argument.ty).collect(),
            None,
            class
                .ctor_args
                .iter()
                .map(|argument| argument.declared_ty.unwrap_or(argument.ty))
                .collect::<Vec<_>>(),
        ),
        Some(ordinal) => {
            let secondary = class.secondary_ctors.get(ordinal)?;
            if !secondary.prefix_params.is_empty() {
                return None;
            }
            (
                secondary.params.clone(),
                Some(secondary.params.clone()),
                secondary.params.clone(),
            )
        }
    };
    let arguments = parameters
        .iter()
        .enumerate()
        .map(|(slot, &ty)| read(ir, u32::try_from(slot).expect("too many parameters"), ty))
        .collect();
    let construction = ir.add_expr(IrExpr::New {
        internal,
        args: arguments,
        ctor_params: selected,
        declared_params: Some(declared.into_boxed_slice()),
        ctor_desc: None,
        external_target: None,
        defaults: Box::default(),
        default_prefix_count: 0,
    });
    ir.set_logical_type(construction, result);
    Some(entry(ir, "<init>$new", parameters, result, construction))
}

fn read(ir: &mut IrFile, slot: u32, ty: Ty) -> ExprId {
    let read = ir.add_expr(IrExpr::GetValue(slot));
    ir.set_logical_type(read, ty);
    read
}
