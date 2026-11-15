//! The dotted Kotlin qualified name of a classifier identity: a boundary spelling for text a
//! compiler or a program prints, never a lookup key.

use super::TypeName;

impl TypeName {
    /// The name kotlinc prints for this classifier (`ClassId.asFqNameString`): the package and
    /// every enclosing class, dot-separated. It is also what `KClass.qualifiedName` answers for a
    /// class that has one, and the spelling `-opt-in` takes.
    pub fn kotlin_qualified_name(self) -> String {
        let mut nested = Vec::new();
        let mut top_level = self;
        while let Some(owner) = top_level.nested_owner() {
            nested.push(top_level.nested_segment_ref());
            top_level = owner;
        }
        // Boundary conversion: the printed name.
        let mut name = top_level.render().replace('/', ".");
        for segment in nested.iter().rev() {
            name.push('.');
            name.push_str(segment);
        }
        name
    }
}
