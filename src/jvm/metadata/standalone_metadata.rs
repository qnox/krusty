//! Metadata decode for a caller that does not already own a spelling catalog.

use super::super::member_spelling::SpellingPool;
use super::{decode_metadata_with, KotlinMeta, MetadataResult};

/// Decode a classfile's `@Metadata` into [`KotlinMeta`] — the ONE place the packed representation is
/// read. `k` is the header kind: a synthetic class (`k = 3`) carries a lambda payload rather than a
/// Class/Package message, while a multi-file facade (`k = 4`) lists its part class names in `d1`
/// verbatim. Neither kind contributes declarations to the classpath semantic model.
///
/// The ephemeral spelling pool lives only for this decode. Classpath parsing passes a shared pool to
/// [`decode_metadata_with`] so one entry reuses spellings across the classes it retains.
pub fn decode_metadata(
    d1: &[String],
    d2: &[String],
    k: Option<i32>,
    this_class: &str,
    package_name: Option<&str>,
    methods: &[super::super::classreader::MethodSig],
) -> MetadataResult<KotlinMeta> {
    decode_metadata_with(
        d1,
        d2,
        k,
        this_class,
        package_name,
        methods,
        &SpellingPool::new(),
    )
}
