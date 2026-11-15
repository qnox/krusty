use super::*;
use crate::types::AnnotationRetention;

#[test]
fn source_policy_evidence_never_uses_dependency_defaults() {
    let classifier = type_name("marks/Policy");
    let source = CheckedRetentionEvidence::Source(classifier);
    let dependency = CheckedRetentionEvidence::Dependency(classifier, AnnotationRetention::Default);
    let mut policies = HashMap::default();
    assert_eq!(source.finalized(&policies), None);
    assert_eq!(
        dependency.finalized(&policies),
        Some(AnnotationRetention::Default)
    );
    policies.insert(classifier, AnnotationRetention::Binary);
    assert_eq!(
        source.finalized(&policies),
        Some(AnnotationRetention::Binary)
    );
    policies.insert(classifier, AnnotationRetention::Source);
    assert_eq!(
        source.finalized(&policies),
        Some(AnnotationRetention::Source)
    );
}
