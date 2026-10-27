//! Real C: temporary -> D: workspace transfer. Never substitutes a same-volume test.
#[cfg(windows)]
#[test]
#[ignore = "Requires explicit workspace ARKCUT_DATA_DIR on a different volume from the Windows temporary directory"]
fn cross_volume_owned_copy_and_cancel() {
    use arkcut_core::{export_record, storage};
    use serde_json::json;
    use std::{fs, path::PathBuf};
    let id = uuid::Uuid::new_v4().to_string();
    let src_root = std::env::temp_dir().join(format!("arkcut-cross-volume-{id}"));
    let data_root =
        PathBuf::from(std::env::var_os("ARKCUT_DATA_DIR").expect("Set a workspace test directory"));
    let dst_root = data_root.join(format!("cross-volume-{id}"));
    fs::create_dir_all(&src_root).unwrap();
    fs::create_dir_all(&dst_root).unwrap();
    let source = storage::temp_file(&src_root, "cross").unwrap();
    let bytes: Vec<_> = (0..3 * 1024 * 1024 + 7).map(|i| (i % 251) as u8).collect();
    fs::write(source.path(), &bytes).unwrap();
    let target = dst_root.join("cached.bin");
    let before = export_record::digest_checked(source.path(), || Ok(())).unwrap();
    let mut checks = 0;
    let error = storage::transfer_cache_checked(&source, &target, || {
        checks += 1;
        if checks == 4 {
            Err("CANCELLED".into())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error, "CANCELLED");
    assert!(!target.exists());
    assert_eq!(fs::read(source.path()).unwrap(), bytes);
    let (size, sha, moved) = storage::transfer_cache_checked(&source, &target, || Ok(())).unwrap();
    assert!(!moved, "Actual cross-volume branch was not exercised");
    assert_eq!((size, sha.clone()), before);
    assert_eq!(fs::read(&target).unwrap(), bytes);
    assert_eq!(fs::read(source.path()).unwrap(), bytes);
    let evidence = json!({"status":"passed","sourceDirectory":src_root,"targetDirectory":dst_root,"crossVolumeFallbackExercised":!moved,"bytes":size,"sha256":sha,"sourceRetainedUntilOwnedTempDrop":true,"cancellationBeforePublication":true});
    fs::write(
        data_root.join("cross-volume-evidence.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    drop(source);
    // Each directory was created uniquely by this test. Remove individual known
    // test/lease/lock files only, then the empty directory; no recursive deletion.
    assert!(fs::canonicalize(&src_root)
        .unwrap()
        .starts_with(fs::canonicalize(std::env::temp_dir()).unwrap()));
    assert!(fs::canonicalize(&dst_root)
        .unwrap()
        .starts_with(fs::canonicalize(&data_root).unwrap()));
    for dir in [&src_root, &dst_root] {
        for item in fs::read_dir(dir).unwrap() {
            let item = item.unwrap();
            assert!(item.file_type().unwrap().is_file());
            let name = item.file_name().to_string_lossy().to_string();
            assert!(name.starts_with(".arkcut-") || name == "cached.bin");
            fs::remove_file(item.path()).unwrap();
        }
        fs::remove_dir(dir).unwrap();
    }
}
