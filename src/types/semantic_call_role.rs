/// Semantic role attached by a declaration provider after exact callable selection. A target may
/// consume it when the selected language member has no ordinary dispatch on a chosen
/// representation. The role belongs to the checked cross-phase type contract; it is not provider
/// or target metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticCallRole {
    KotlinAnyEquals,
    KotlinAnyHashCode,
    KotlinAnyToString,
}
