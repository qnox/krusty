//! Type references that have no declaration header to own them.

use super::*;

impl HeaderInventoryBuilder {
    pub(super) fn add_file_detached_types(&mut self, file: &File, source: SourceFileId) {
        for reference in &file.detached_type_refs {
            let ty = self.syntax.add_type(reference, &mut self.lookup_names);
            self.detached_types.push((source, ty));
        }
        // A file annotation is not declaration-owned, but its classifier still follows ordinary
        // file-scope/import resolution. Retain the reference, not the suppression argument text.
        for (annotation, _) in &file.file_annotations {
            let annotation = TypeRef::from_annotation(annotation);
            let ty = self.syntax.add_type(&annotation, &mut self.lookup_names);
            self.detached_types.push((source, ty));
        }
    }
}
