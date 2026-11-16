use crate::klib_libraries::{KlibDeclarationBodies, KlibLibraries};
use crate::libraries::ExternalCallableRealization;

pub(super) fn checked_template_hierarchy(
    libraries: &KlibLibraries,
    selected: &ExternalCallableRealization,
) -> crate::ir::CheckedKlibTypeFacts {
    let callable = selected
        .klib_body_callable()
        .expect("selected template has a KLIB body callable");
    let bodies: &KlibDeclarationBodies = libraries.declaration_bodies();
    crate::resolve::for_checked_klib_bodies(libraries, bodies, [callable])
        .expect("selected template body graph has checked facts")
        .type_hierarchy()
        .expect("the checker publishes a hierarchy snapshot")
        .clone()
}
