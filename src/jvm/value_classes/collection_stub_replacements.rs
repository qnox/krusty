//! kotlinc's value-class replacement of a read-only collection stub.
//!
//! `CollectionStubMethodLowering` runs before `JvmInlineClassLowering`, so a throwing stub whose
//! signature mentions a value class is replaced like any other member
//! (`MemoizedInlineClassReplacements.getReplacementFunction`):
//!
//! * in a value class every stub (`IR_BUILTINS_STUB`) takes a method replacement, except the
//!   `removeAt` stub `remove(Int)`, which keeps its name;
//! * elsewhere a stub takes one when a parameter requires mangling or its result is a value class.
//!
//! A replacement is named by the mangled signature (`add-IQRRRT4`, `remove-gaYaXmE` for a value
//! class result), and every replaced or kept stub declares each value-class position by the
//! carrier the JVM type mapper writes for it: `Z(val x: Int)` is `int`, a nullable `S(val s:
//! String)` is `String`, and a nullable `Z` stays the box.

use crate::ir::IrFile;
use crate::jvm::bridge_adaptations::ValueClassAdapter;
use crate::jvm::method_descriptors::{jvm_declared_ty, jvm_tys};
use crate::types::Ty;

/// A stub's JVM name, physical parameters and physical result.
#[derive(Clone, Debug)]
pub(crate) struct StubRealization {
    pub(crate) name: String,
    pub(crate) parameters: Vec<Ty>,
    pub(crate) result: Ty,
    /// The value class whose carrier the stub returns where its declaration returns the box: a
    /// bridge that returns an erased reference boxes the stub's result through `box-impl`.
    pub(crate) result_box: Option<ValueClassAdapter>,
}

/// The realization of a stub named `name` with the semantic `parameters` and `result` (the result
/// as the stub declares it, a boxed scalar's wrapper included). `in_value_class` is whether the
/// class that receives the stub is a value class; `removes_at` is whether the stub is kotlinc's
/// `removeAt` stub `remove(Int)`.
pub(crate) fn collection_stub_realization(
    ir: &IrFile,
    in_value_class: bool,
    removes_at: bool,
    name: &str,
    parameters: &[Ty],
    result: Ty,
) -> StubRealization {
    let name = if in_value_class && removes_at {
        name.to_string()
    } else {
        // A member is never in a file class, so its value-class result takes part in the hash.
        super::vc_mangle(name, parameters, &result, ir, false, false)
    };
    let under = super::representation::known_underlyings(ir);
    let erased: Vec<Ty> = parameters
        .iter()
        .map(|parameter| super::erase(parameter, &under))
        .collect();
    let erased_result = super::erase(&result, &under);
    // A nullable value class whose carrier is a reference holds its null in the carrier.
    let result_box = super::member_names::value_class_bound_occurrence(result, &under)
        .non_null()
        .obj_internal()
        .filter(|classifier| under.contains_key(classifier) && erased_result != result)
        .map(|classifier| ValueClassAdapter::new(classifier, result.is_nullable()));
    StubRealization {
        name,
        parameters: jvm_tys(&erased),
        result: jvm_declared_ty(&erased_result),
        result_box,
    }
}
