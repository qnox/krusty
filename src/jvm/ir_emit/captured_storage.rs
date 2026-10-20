//! JVM storage of a class's compiler-supplied constructor prefix.

use crate::ir::IrClass;

pub(super) mod captured_assignment;

/// How many leading fields store the constructor prefix. A local class keeps a capture that only
/// its constructor code reads in the parameter, so it can have fewer prefix fields than prefix
/// parameters; so can an anonymous object, whose forwarded super-constructor arguments are
/// parameters only. A prefix longer than the constructor's arguments is malformed IR.
pub(super) fn constructor_prefix_fields(class: &IrClass) -> usize {
    let prefix = usize::try_from(class.constructor_prefix_count)
        .expect("constructor prefix count does not fit usize");
    class
        .ctor_args
        .get(..prefix)
        .expect("constructor prefix exceeds its argument layout")
        .iter()
        .filter(|argument| argument.is_field)
        .count()
}

/// Whether instance field `field_index` stores a constructor-prefix value: the enclosing instance
/// of an inner class, or a value a local class or anonymous object captures.
///
/// kotlinc realizes that storage as package-visible `final synthetic` (`JvmVisibilityPolicy`
/// `forCapturedField`), so a class nested in the capturing class reads it directly. A private field
/// would need a synthetic accessor and fail to link without one.
pub(super) fn stores_constructor_prefix(class: &IrClass, field_index: usize) -> bool {
    let field_index = u32::try_from(field_index).expect("too many JVM fields");
    let prefix_count = usize::try_from(class.constructor_prefix_count)
        .expect("constructor prefix count does not fit usize");
    class
        .ctor_args
        .get(..prefix_count)
        .expect("constructor prefix exceeds its argument layout")
        .iter()
        .any(|argument| argument.is_field && argument.field_index == Some(field_index))
}

#[cfg(test)]
mod tests {
    use super::constructor_prefix_fields;
    use crate::ir::{IrClass, IrCtorArg, IrCtorParameterProvenance};
    use crate::types::{ContextParameterKind, Ty};

    fn capture(field_index: Option<u32>) -> IrCtorArg {
        IrCtorArg {
            name: None,
            context_kind: ContextParameterKind::None,
            ty: Ty::Int,
            declared_ty: None,
            is_field: field_index.is_some(),
            field_index,
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: IrCtorParameterProvenance::Capture,
            capture_identity: None,
        }
    }

    #[test]
    fn only_stored_prefix_values_have_fields() {
        let mut class = IrClass::synthetic(crate::types::type_name("Local"));
        class.ctor_args = vec![capture(None), capture(Some(0))];
        class.constructor_prefix_count = 2;
        assert_eq!(constructor_prefix_fields(&class), 1);
    }

    #[test]
    #[should_panic(expected = "constructor prefix exceeds its argument layout")]
    fn a_prefix_longer_than_the_arguments_is_malformed() {
        let mut class = IrClass::synthetic(crate::types::type_name("Local"));
        class.ctor_args = vec![capture(Some(0))];
        class.constructor_prefix_count = 2;
        constructor_prefix_fields(&class);
    }
}
