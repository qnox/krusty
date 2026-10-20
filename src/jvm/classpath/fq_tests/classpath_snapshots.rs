use super::*;

#[test]
fn classpath_snapshot_detects_a_nested_class_overwrite() {
    use std::io::Write;

    let directory = test_temp_dir("nested-class-revision");
    let package = directory.join("existing/package");
    std::fs::create_dir_all(&package).expect("create existing package");
    let class_file = package.join("Changed.class");
    let bytes =
        crate::jvm::classfile::ClassWriter::new("existing/package/Changed", "java/lang/Object")
            .finish();
    std::fs::write(&class_file, bytes).expect("write initial class");
    let classpath = Classpath::new(vec![directory.clone()]);
    assert!(classpath.snapshot_is_current());

    std::fs::OpenOptions::new()
        .append(true)
        .open(&class_file)
        .expect("open nested class")
        .write_all(&[0])
        .expect("overwrite nested class");

    assert!(!classpath.snapshot_is_current());
    drop(classpath);
    std::fs::remove_dir_all(directory).expect("remove class directory");
}

#[cfg(unix)]
#[test]
fn classpath_snapshot_follows_symlinks_without_cycles() {
    use std::io::Write;
    use std::os::unix::fs::symlink;

    let directory = test_temp_dir("symlink-class-revision");
    let targets = test_temp_dir("symlink-class-targets");
    let linked_file = targets.join("Linked.class");
    let linked_bytes =
        crate::jvm::classfile::ClassWriter::new("Linked", "java/lang/Object").finish();
    std::fs::write(&linked_file, linked_bytes).expect("write linked class");
    symlink(&linked_file, directory.join("Linked.class")).expect("link class file");

    let linked_directory = targets.join("package");
    std::fs::create_dir(&linked_directory).expect("create linked package");
    let nested_file = linked_directory.join("Nested.class");
    let nested_bytes =
        crate::jvm::classfile::ClassWriter::new("package/Nested", "java/lang/Object").finish();
    std::fs::write(&nested_file, nested_bytes).expect("write nested linked class");
    symlink(&linked_directory, directory.join("package")).expect("link package directory");
    symlink(&directory, directory.join("loop")).expect("link directory cycle");

    let file_snapshot = Classpath::new(vec![directory.clone()]);
    file_snapshot.prepare_for_source_analysis();
    assert!(file_snapshot.snapshot_is_current());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&linked_file)
        .expect("open linked class target")
        .write_all(b"-changed")
        .expect("change linked class target");
    assert!(!file_snapshot.snapshot_is_current());

    let directory_snapshot = Classpath::new(vec![directory.clone()]);
    assert!(directory_snapshot.snapshot_is_current());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&nested_file)
        .expect("open linked directory target")
        .write_all(b"-changed")
        .expect("change linked directory target");
    assert!(!directory_snapshot.snapshot_is_current());

    drop(directory_snapshot);
    drop(file_snapshot);
    std::fs::remove_dir_all(directory).expect("remove symlink directory");
    std::fs::remove_dir_all(targets).expect("remove symlink targets");
}

#[cfg(unix)]
#[test]
fn classpath_catalogs_each_symlink_alias() {
    use std::os::unix::fs::symlink;

    let directory = test_temp_dir("symlink-aliases");
    let target = test_temp_dir("symlink-alias-target");
    let left_package = target.join("left");
    let right_package = target.join("right");
    std::fs::create_dir(&left_package).expect("create left package");
    std::fs::create_dir(&right_package).expect("create right package");
    let through_left =
        crate::jvm::classfile::ClassWriter::new("left/right/Shared", "java/lang/Object").finish();
    let through_right =
        crate::jvm::classfile::ClassWriter::new("right/left/Shared", "java/lang/Object").finish();
    std::fs::write(right_package.join("Shared.class"), through_left)
        .expect("write class reached through left alias");
    std::fs::write(left_package.join("Shared.class"), through_right)
        .expect("write class reached through right alias");
    symlink(&target, directory.join("left")).expect("create left alias");
    symlink(&target, directory.join("right")).expect("create right alias");

    let classpath = Classpath::new(vec![directory.clone()]);
    assert!(classpath.find("left/right/Shared").is_some());
    assert!(classpath.find("right/left/Shared").is_some());

    drop(classpath);
    std::fs::remove_dir_all(directory).expect("remove alias directory");
    std::fs::remove_dir_all(target).expect("remove alias target");
}

#[cfg(unix)]
#[test]
fn classpath_snapshot_detects_symlink_retargeting() {
    use std::os::unix::fs::symlink;

    let directory = test_temp_dir("symlink-retarget");
    let targets = test_temp_dir("symlink-retarget-targets");
    let first = targets.join("first.class");
    let second = targets.join("second.class");
    let bytes = crate::jvm::classfile::ClassWriter::new("Linked", "java/lang/Object").finish();
    std::fs::write(&first, bytes).expect("write first target");
    std::fs::hard_link(&first, &second).expect("create equal-metadata target");
    let linked = directory.join("Linked.class");
    symlink(&first, &linked).expect("create initial symlink");

    let classpath = Classpath::new(vec![directory.clone()]);
    assert!(classpath.snapshot_is_current());
    std::fs::remove_file(&linked).expect("remove initial symlink");
    symlink(&second, &linked).expect("retarget symlink");
    assert!(!classpath.snapshot_is_current());

    drop(classpath);
    std::fs::remove_dir_all(directory).expect("remove symlink directory");
    std::fs::remove_dir_all(targets).expect("remove symlink targets");
}

#[cfg(unix)]
#[test]
fn broken_symlink_does_not_hide_a_class_change() {
    use std::io::Write;
    use std::os::unix::fs::symlink;

    let directory = test_temp_dir("broken-symlink-revision");
    symlink(
        directory.join("permanently-missing"),
        directory.join("broken"),
    )
    .expect("create broken symlink");
    let package = directory.join("existing");
    std::fs::create_dir(&package).expect("create package");
    let class_file = package.join("Changed.class");
    let bytes =
        crate::jvm::classfile::ClassWriter::new("existing/Changed", "java/lang/Object").finish();
    std::fs::write(&class_file, bytes).expect("write class");

    let classpath = Classpath::new(vec![directory.clone()]);
    assert!(classpath.snapshot_is_current());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&class_file)
        .expect("open class")
        .write_all(&[0])
        .expect("change class");
    assert!(!classpath.snapshot_is_current());

    drop(classpath);
    std::fs::remove_dir_all(directory).expect("remove class directory");
}
