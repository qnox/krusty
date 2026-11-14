//! The library interfaces builtin values are instances of.
//!
//! A string or a boxed primitive is an instance of the library interfaces above its builtin class
//! (`String` is a `CharSequence` and a `Comparable<String>`, `Int` a `Comparable<Int>`). An `is`
//! against such an interface must answer true for these values, which no class of the program
//! records. Which builtin classes are below an interface comes from the frontend's supertype
//! traversal (`krusty::backend::BackendClassifierSource::builtin_subtypes`); here each of their
//! representations is recorded an instance of the interface: a string's struct, which has no
//! interface table, and each box's class.
//!
//! An interface's builtin instances are recorded the first time something tests for it, and a box
//! the first time a value of its type is boxed; whichever comes second records the box.

use krusty::types::{Ty, TypeName};

use super::boxes::BoxForm;
use super::runtime::Runtime;

/// The library interfaces something tests for, with the builtin types below each.
#[derive(Default)]
pub(super) struct BuiltinInstances {
    interfaces: Vec<(TypeName, Vec<Ty>)>,
}

impl Runtime {
    /// Record that the builtin values of `subtypes` are instances of `interface`, which something
    /// tests for: a string's now, and each box's now or when the box is declared.
    pub(super) fn builtin_instances(&mut self, interface: TypeName, subtypes: Vec<Ty>) {
        if self
            .builtin_instance_tests
            .interfaces
            .iter()
            .any(|(known, _)| *known == interface)
        {
            return;
        }
        if subtypes.contains(&Ty::String) {
            self.objects
                .interfaces
                .representation_instance_of(self.string, interface);
        }
        for form in self.box_forms() {
            if subtypes.contains(&form.boxed) {
                self.objects.interfaces.instance_of(form.class, interface);
            }
        }
        self.builtin_instance_tests
            .interfaces
            .push((interface, subtypes));
    }

    /// Record the new box `form` an instance of every tested interface above its type.
    pub(super) fn box_instances(&mut self, form: BoxForm) {
        for (interface, subtypes) in &self.builtin_instance_tests.interfaces {
            if subtypes.contains(&form.boxed) {
                self.objects.interfaces.instance_of(form.class, *interface);
            }
        }
    }
}
