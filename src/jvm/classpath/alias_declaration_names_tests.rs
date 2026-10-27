use super::test_support::test_temp_dir;
use super::*;
use crate::libraries::AliasExpansion;
use crate::types::ClassifierDeclarationName;

#[test]
fn alias_declaration_names_survive_classpath_package_and_classifier_indexes() {
    for (classifier, class_name) in [(false, false), (true, true), (true, false)] {
        let directory = test_temp_dir("alias-declaration-names");
        let internal = if classifier {
            "alias_cp_names/Host$Literal"
        } else {
            "alias_cp_names/TopKt"
        };
        let mut message = Vec::new();
        if class_name {
            message.extend_from_slice(&[0x18, 0x02]);
        }
        message.extend_from_slice(&[if classifier { 0x5a } else { 0x2a }, 0x06]);
        // Public alias, simple-name index 0, expanded-class index 1.
        message.extend_from_slice(&[0x10, 0x00, 0x32, 0x02, 0x30, 0x01]);
        let mut payload = vec![0]; // Empty metadata string-table message.
        payload.extend_from_slice(&message);
        let d1 = crate::metadata::encoding::bytes_to_strings(&payload);
        let d2 = vec![
            "Alias$Name".into(),
            "alias_cp_target/Real".into(),
            "alias_cp_names/Host$Literal".into(),
        ];
        let mut writer = crate::jvm::classfile::ClassWriter::new(internal, "java/lang/Object");
        writer.set_kotlin_metadata(
            if classifier { 1 } else { 2 },
            crate::jvm::classfile::MetadataStamp {
                version: [2, 2, 0],
                pre_release: false,
            },
            48,
            &d1,
            &d2,
        );
        let path = directory.join(format!("{internal}.class"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, writer.finish()).unwrap();
        let classpath = Classpath::new(vec![directory.clone()]);
        let owner = type_name(internal);
        let identity = if classifier {
            classpath
                .classifier_type_alias_identity(owner, "Alias$Name")
                .expect("the classifier declares the alias")
        } else {
            type_name("alias_cp_names/Alias$Name")
        };
        let declaration_name = if !classifier {
            Some(ClassifierDeclarationName::TopLevel {
                package: type_name("alias_cp_names"),
                name: "Alias$Name".into(),
            })
        } else if class_name {
            Some(ClassifierDeclarationName::Nested {
                owner,
                name: "Alias$Name".into(),
            })
        } else {
            None
        };
        let target = type_name("alias_cp_target/Real");
        assert_eq!(
            classpath.type_alias_expansion(identity),
            Some(AliasExpansion {
                identity,
                declaration_name: declaration_name.map(std::sync::Arc::new),
                target,
                formals: Vec::new(),
                expansion: Ty::obj_name(target),
                expansion_spelling: Default::default(),
            })
        );
        assert_eq!(classpath.type_alias_target_name(identity), Some(target));
        drop(classpath);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
