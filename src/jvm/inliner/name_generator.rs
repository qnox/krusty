//! kotlinc's `NameGenerator`: the names of the classes an inline call regenerates.
//!
//! Every class being written owns one generator per function name, spelled
//! `<class>$<function>$$inlined` (`$special` for a constructor or another special name). An inline
//! call in that function takes the generator's child for the callee's name
//! (`…$$inlined$<callee>`), shared by every call of a callee of that name in the function, and each
//! anonymous object the call regenerates takes the next number under it (`…$<callee>$1`); the
//! objects that object regenerates in turn are numbered under its own name.

use std::collections::HashMap;

/// kotlinc's `SPECIAL_TRANSFORMATION_NAME`.
const SPECIAL_TRANSFORMATION_NAME: &str = "$special";
/// kotlinc's `INLINE_CALL_TRANSFORMATION_SUFFIX`.
const INLINE_CALL_TRANSFORMATION_SUFFIX: &str = "$$inlined";

/// One level of regenerated names.
#[derive(Clone, Debug)]
pub(crate) struct NameGenerator {
    class: String,
    next_lambda_index: u32,
    children: HashMap<String, NameGenerator>,
}

impl NameGenerator {
    fn new(class: String) -> NameGenerator {
        NameGenerator {
            class,
            next_lambda_index: 1,
            children: HashMap::new(),
        }
    }

    /// The internal name this generator numbers under (`generatorClass`).
    pub(crate) fn class(&self) -> &str {
        &self.class
    }

    /// `subGenerator(inliningMethod)`: the generator for inline calls of `method`, created on first
    /// use.
    pub(crate) fn for_inlined_method(&mut self, method: &str) -> &mut NameGenerator {
        let class = format!("{}${method}", self.class);
        self.children
            .entry(method.to_string())
            .or_insert_with(|| NameGenerator::new(class))
    }

    /// `subGenerator(true, null)`: the generator of the next regenerated anonymous object, named
    /// `<class>$<n>`.
    pub(crate) fn next_object(&mut self) -> &mut NameGenerator {
        let class = format!("{}${}", self.class, self.next_lambda_index);
        self.next_lambda_index += 1;
        self.child(class)
    }

    fn child(&mut self, class: String) -> &mut NameGenerator {
        debug_assert!(
            !self.children.contains_key(&class),
            "a regenerated class name is unique"
        );
        self.children
            .entry(class.clone())
            .or_insert_with(|| NameGenerator::new(class))
    }
}

/// The generators of one class being written (`ClassCodegen.regeneratedObjectNameGenerators`).
#[derive(Clone, Debug, Default)]
pub(crate) struct ClassNameGenerators {
    by_function: HashMap<String, NameGenerator>,
}

impl ClassNameGenerators {
    /// `getRegeneratedObjectNameGenerator`: the generator of the function named `function` (its
    /// Kotlin name, `None` for a special name such as a constructor) in the class `class`.
    pub(crate) fn for_function(
        &mut self,
        class: &str,
        function: Option<&str>,
    ) -> &mut NameGenerator {
        let name = match function {
            Some(function) => format!("${function}"),
            None => SPECIAL_TRANSFORMATION_NAME.to_string(),
        };
        self.by_function.entry(name.clone()).or_insert_with(|| {
            NameGenerator::new(format!("{class}{name}{INLINE_CALL_TRANSFORMATION_SUFFIX}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ClassNameGenerators;

    #[test]
    fn objects_are_numbered_per_callee_name_within_a_function() {
        let mut generators = ClassNameGenerators::default();
        let plain = generators.for_function("MainKt", Some("plain"));
        let greeter = plain.for_inlined_method("greeter");
        assert_eq!(
            greeter.next_object().class(),
            "MainKt$plain$$inlined$greeter$1"
        );
        assert_eq!(
            greeter.next_object().class(),
            "MainKt$plain$$inlined$greeter$2"
        );
        let again = generators
            .for_function("MainKt", Some("plain"))
            .for_inlined_method("greeter");
        assert_eq!(
            again.next_object().class(),
            "MainKt$plain$$inlined$greeter$3"
        );
        let other = generators
            .for_function("MainKt", Some("plain"))
            .for_inlined_method("caller");
        assert_eq!(
            other.next_object().class(),
            "MainKt$plain$$inlined$caller$1"
        );
    }

    #[test]
    fn a_special_function_and_nested_objects_have_their_own_names() {
        let mut generators = ClassNameGenerators::default();
        let init = generators
            .for_function("A", None)
            .for_inlined_method("make");
        let outer = init.next_object();
        assert_eq!(outer.class(), "A$special$$inlined$make$1");
        assert_eq!(outer.next_object().class(), "A$special$$inlined$make$1$1");
    }
}
