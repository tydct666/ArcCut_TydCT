use arkcut_core::update::{platform, protocol::*, transaction};
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signer, SigningKey};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
fn config() -> Config {
    let key = SigningKey::from_bytes(&[71; 32]);
    Config {
        endpoint: "https://example.test/index.json".into(),
        asset_prefix: "https://example.test/releases/".into(),
        redirect_hosts: vec!["example.test".into()],
        keys: vec![Key {
            id: "test-only".into(),
            key: STANDARD.encode(key.verifying_key().as_bytes()),
        }],
        channel: "stable".into(),
    }
}
fn signed(r: &Release) -> Envelope {
    let key = SigningKey::from_bytes(&[71; 32]);
    let payload = serde_json::to_vec(r).unwrap();
    Envelope {
        key_id: "test-only".into(),
        signature: STANDARD.encode(key.sign(&payload).to_bytes()),
        payload: STANDARD.encode(payload),
    }
}
fn temp() -> PathBuf {
    let p = std::env::temp_dir().join(format!("arkcut-updater-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&p).unwrap();
    p
}
fn tree(p: &Path, version: &str) -> Vec<OwnedFile> {
    fs::create_dir_all(p).unwrap();
    for name in [
        "ArkCut.exe",
        "ArkCutUpdater.exe",
        "WebView2/msedgewebview2.exe",
        "media/ffmpeg.exe",
        "media/ffprobe.exe",
    ] {
        let f = p.join(name);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, format!("{version}:{name}")).unwrap();
    }
    let files: Vec<_> = [
        "ArkCut.exe",
        "ArkCutUpdater.exe",
        "WebView2/msedgewebview2.exe",
        "media/ffmpeg.exe",
        "media/ffprobe.exe",
    ]
    .iter()
    .map(|n| OwnedFile {
        path: n.to_string(),
        bytes: fs::metadata(p.join(n)).unwrap().len(),
        sha256: hash(&p.join(n)).unwrap(),
    })
    .collect();
    platform::json(
        &p.join("manifest.json"),
        &serde_json::json!({"product":"ArkCut","version":version,"files":files}),
    )
    .unwrap();
    transaction::old_files(p).unwrap()
}
fn fixture(p: &Path) -> (PathBuf, Release, Envelope) {
    fixture_versions(p, "0.17.0", "0.17.1")
}
fn fixture_versions(p: &Path, old_version: &str, new_version: &str) -> (PathBuf, Release, Envelope) {
    let root = p.join("中文 空格 app");
    tree(&root, old_version);
    fs::create_dir_all(root.join("项目存档")).unwrap();
    fs::write(root.join("项目存档/user.json"), b"archive-original").unwrap();
    fs::write(root.join("recording.mp4"), b"original-media").unwrap();
    let new = p.join("new");
    let files = tree(&new, new_version);
    let dir = transaction::control(&root).unwrap();
    let archive = dir.join("archive.zip");
    let mut z = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    for f in &files {
        z.start_file(
            format!("ArkCut/{}", f.path),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.write_all(&fs::read(new.join(&f.path)).unwrap()).unwrap();
    }
    z.finish().unwrap();
    let r = Release {
        schema: 1,
        product: "ArkCut".into(),
        version: new_version.into(),
        platform: "windows-x86_64".into(),
        channel: "stable".into(),
        min_updater: old_version.into(),
        data_schema: 1,
        notes: "test".into(),
        source_commit: "0".repeat(40),
        asset: Asset {
            url: "https://example.test/releases/app.zip".into(),
            bytes: fs::metadata(&archive).unwrap().len(),
            sha256: hash(&archive).unwrap(),
            root: "ArkCut".into(),
        },
        unpacked_bytes: files.iter().map(|f| f.bytes).sum(),
        components: BTreeMap::from([
            ("ffmpeg".into(), "test".into()),
            ("webview2".into(), "test".into()),
        ]),
        files,
    };
    let e = signed(&r);
    platform::json(&dir.join("offer.json"), &e).unwrap();
    (root, r, e)
}
fn unchanged(root: &Path) {
    assert_eq!(
        fs::read(root.join("项目存档/user.json")).unwrap(),
        b"archive-original"
    );
    assert_eq!(
        fs::read(root.join("recording.mp4")).unwrap(),
        b"original-media"
    );
}
#[test]
fn lightweight_protocol_rejects_bundled_runtimes(){
    let p=temp();let(_,mut r,_)=fixture(&p);r.components.insert("webview2".into(),"evergreen>=155".into());
    assert!(validate(&r,&config(),Some("0.17.0")).is_err());
    r.files.retain(|f|!f.path.starts_with("media/")&&!f.path.starts_with("WebView2/"));r.unpacked_bytes=r.files.iter().map(|f|f.bytes).sum();
    assert!(validate(&r,&config(),Some("0.17.0")).is_ok());transaction::remove_owned_tree(&p).unwrap();
}
#[test]
fn component_preflight_failure_does_not_replace_application(){
    let p=temp();let(root,mut r,_)=fixture(&p);let new=p.join("new");
    // An intentionally non-executable candidate proves that failed preflight happens before any switch.
    r.files.retain(|f|!f.path.starts_with("media/")&&!f.path.starts_with("WebView2/"));
    let listed:Vec<_>=r.files.iter().filter(|f|f.path!="manifest.json").cloned().collect();
    platform::json(&new.join("manifest.json"),&serde_json::json!({"product":"ArkCut","version":"0.17.1","files":listed})).unwrap();
    let f=r.files.iter_mut().find(|f|f.path=="manifest.json").unwrap();f.bytes=fs::metadata(new.join("manifest.json")).unwrap().len();f.sha256=hash(&new.join("manifest.json")).unwrap();
    let archive=transaction::control(&root).unwrap().join("archive.zip");let mut z=zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    for f in &r.files{z.start_file(format!("ArkCut/{}",f.path),zip::write::SimpleFileOptions::default()).unwrap();z.write_all(&fs::read(new.join(&f.path)).unwrap()).unwrap();}z.finish().unwrap();
    r.components.insert("webview2".into(),"evergreen>=155".into());r.asset.bytes=fs::metadata(&archive).unwrap().len();r.asset.sha256=hash(&archive).unwrap();r.unpacked_bytes=r.files.iter().map(|f|f.bytes).sum();
    let old=hash(&root.join("ArkCut.exe")).unwrap();let result=transaction::install(&root,&config(),signed(&r),&r,|_,_|panic!("health must not run"));
    assert!(result.unwrap_err().contains("当前版本未改变"));assert_eq!(hash(&root.join("ArkCut.exe")).unwrap(),old);assert!(transaction::journal(&root).unwrap().is_none());unchanged(&root);transaction::remove_owned_tree(&p).unwrap();
}
#[test]
fn path_restrictions() {
    for s in [
        "../ArkCut.exe",
        "C:/a",
        "/root/a",
        "media/a:stream",
        "media/CON.txt",
        "media/COM¹.txt",
        "media/LPT²",
        "media/a.",
        "media/a ",
        "media/../a",
        "media\\a",
        "项目存档/user.json",
        "settings.json",
    ] {
        assert!(owned_path(s).is_err(), "{s}")
    }
    assert!(owned_path("media/codec.dat").is_ok());
}

#[cfg(windows)]
#[test]
fn transient_record_reader_does_not_break_atomic_commit() {
    use std::os::windows::fs::OpenOptionsExt;
    let p=temp();let record=p.join("transaction.json");fs::write(&record,b"old").unwrap();
    let reader=fs::OpenOptions::new().read(true).share_mode(1).open(&record).unwrap();
    let destination=record.clone();let worker=std::thread::spawn(move||platform::atomic(&destination,b"new"));
    std::thread::sleep(std::time::Duration::from_millis(200));drop(reader);
    worker.join().unwrap().unwrap();assert_eq!(fs::read(record).unwrap(),b"new");fs::remove_dir_all(p).unwrap();
}

#[test]
fn manual_check_rechecks_a_cached_release_while_automatic_defers() {
    let p = temp();
    let current = semver::Version::parse(arkcut_core::update::VERSION).unwrap();
    let next = format!("{}.{}.{}", current.major, current.minor, current.patch + 1);
    let (root, _, _) = fixture_versions(&p, arkcut_core::update::VERSION, &next);
    let mut c=config();
    c.endpoint="https://127.0.0.1:1/index".into();c.redirect_hosts.push("127.0.0.1".into());
    let updater=arkcut_core::update::Updater::new(root,c);
    assert_eq!(updater.status().status,"ready");
    assert_eq!(updater.check(true).unwrap().status,"ready");
    assert_ne!(updater.check(false).unwrap().status,"ready");
    let start=std::time::Instant::now();while updater.status().status=="checking"{assert!(start.elapsed().as_secs()<20);std::thread::sleep(std::time::Duration::from_millis(20));}
    fs::remove_dir_all(p).unwrap();
}

#[cfg(not(feature = "update-test-hooks"))]
#[test]
fn production_ignores_test_configuration() {
    std::env::set_var("ARKCUT_UPDATE_TEST_CONFIG", "missing-untrusted-config.json");
    let embedded = Config::embedded().unwrap();
    std::env::remove_var("ARKCUT_UPDATE_TEST_CONFIG");
    assert!(embedded
        .endpoint
        .starts_with("https://github.com/tydct666/ArcCut_TydCT/"));
}

#[cfg(feature = "update-test-hooks")]
#[test]
fn low_space_worker() {
    if let Ok(p) = std::env::var("ARKCUT_UPDATE_LOW_SPACE_ROOT") {
        let p = PathBuf::from(p);
        let (root, r, e) = fixture(&p);
        assert!(transaction::install(&root, &config(), e, &r, |_, _| Ok(()))
            .unwrap_err()
            .contains("空间不足"));
        assert!(transaction::journal(&root).unwrap().is_none());
        unchanged(&root);
    }
}
#[cfg(feature = "update-test-hooks")]
#[test]
fn low_space_then_recover_without_replacing_files() {
    let p = temp();
    let exit = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("low_space_worker")
        .env("ARKCUT_UPDATE_LOW_SPACE_ROOT", &p)
        .env("ARKCUT_UPDATE_TEST_LOW_SPACE", "1")
        .output()
        .unwrap();
    assert!(
        exit.status.success(),
        "{}",
        String::from_utf8_lossy(&exit.stdout)
    );
    let root = p.join("中文 空格 app");
    let (e, r) = verify(
        &read_bounded(&root.join(".arkcut-update/offer.json"), MAX_INDEX).unwrap(),
        &config(),
        Some("0.17.0"),
    )
    .unwrap();
    transaction::install(&root, &config(), e, &r, |_, _| Ok(())).unwrap();
    unchanged(&root);
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn signature_version_platform_and_size() {
    let p = temp();
    let (_, r, e) = fixture(&p);
    let c = config();
    let bytes = serde_json::to_vec(&e).unwrap();
    assert!(verify(&bytes, &c, Some("0.17.0")).is_ok());
    assert!(verify(&bytes, &c, Some("0.17.1")).is_err());
    assert!(verify(&bytes, &c, Some("0.18.0")).is_err());
    let mut bad = e.clone();
    bad.payload = STANDARD.encode(b"tampered");
    assert!(verify(&serde_json::to_vec(&bad).unwrap(), &c, None).is_err());
    bad = e;
    bad.key_id = "unknown".into();
    assert!(verify(&serde_json::to_vec(&bad).unwrap(), &c, None).is_err());
    for change in 0..8 {
        let mut v = r.clone();
        match change {
            0 => v.platform = "linux-x64".into(),
            1 => v.version = "0.18.0-beta.1".into(),
            2 => v.asset.url = "https://evil.test/a.zip".into(),
            3 => v.files[0].path = "项目存档/a".into(),
            4 => v.asset.bytes = MAX_ARCHIVE + 1,
            5 => v.data_schema = 2,
            6 => v.files.push(v.files[0].clone()),
            _ => v.min_updater = "9.0.0".into(),
        };
        assert!(
            verify(
                &serde_json::to_vec(&signed(&v)).unwrap(),
                &c,
                Some("0.17.0")
            )
            .is_err(),
            "case {change}"
        );
    }
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn success_preserves_user_data_and_backup() {
    let p = temp();
    let (root, r, e) = fixture(&p);
    transaction::install(&root, &config(), e, &r, |_, _| Ok(())).unwrap();
    assert_eq!(
        transaction::journal(&root).unwrap().unwrap().state,
        "committed"
    );
    assert!(fs::read_to_string(root.join("ArkCut.exe"))
        .unwrap()
        .starts_with("0.17.1"));
    unchanged(&root);
    assert!(root.join(".arkcut-update/backup/ArkCut.exe").exists());
    assert!(!root.join(".arkcut-update/payload").exists());
    assert!(!root.join(".arkcut-update/archive.zip").exists());
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn failed_health_rolls_back_and_retry_works() {
    let p = temp();
    let (root, r, e) = fixture(&p);
    let old = transaction::old_files(&root).unwrap();
    assert!(
        transaction::install(&root, &config(), e.clone(), &r, |_, _| Err(
            "test health failure".into()
        ))
        .is_err()
    );
    for f in &old {
        assert!(matches(&root.join(&f.path), f));
    }
    unchanged(&root);
    assert_eq!(
        transaction::journal(&root).unwrap().unwrap().state,
        "rolledBack"
    );
    transaction::install(&root, &config(), e, &r, |_, _| Ok(())).unwrap();
    unchanged(&root);
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn corrupt_archive_and_changed_program_fail_before_replacement() {
    let p = temp();
    let (root, r, e) = fixture(&p);
    fs::write(root.join("ArkCut.exe"), b"modified user file").unwrap();
    assert!(transaction::install(&root, &config(), e.clone(), &r, |_, _| Ok(())).is_err());
    assert!(transaction::journal(&root).unwrap().is_none());
    assert_eq!(
        fs::read(root.join("ArkCut.exe")).unwrap(),
        b"modified user file"
    );
    assert!(transaction::verify_archive(&root.join("ArkCut.exe"), &r).is_err());
    unchanged(&root);
    fs::remove_dir_all(p).unwrap();
}
#[cfg(windows)]
#[test]
fn occupied_file_rolls_back() {
    use std::os::windows::fs::OpenOptionsExt;
    let p = temp();
    let (root, r, e) = fixture(&p);
    let old = transaction::old_files(&root).unwrap();
    let guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(root.join("media/ffmpeg.exe"))
        .unwrap();
    assert!(transaction::install(&root, &config(), e, &r, |_, _| Ok(())).is_err());
    drop(guard);
    transaction::recover(&root, &config()).unwrap();
    for f in &old {
        assert!(matches(&root.join(&f.path), f));
    }
    unchanged(&root);
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn zip_rejects_traversal_and_unsigned_entries() {
    for name in [
        "ArkCut/../evil",
        "ArkCut/media/CON.txt",
        "ArkCut/media/a",
        "wrong/ArkCut.exe",
    ] {
        let p = temp();
        let (root, mut r, _) = fixture(&p);
        let archive = root.join(".arkcut-update/archive.zip");
        let mut z = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
        z.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"evil").unwrap();
        z.finish().unwrap();
        r.asset.bytes = fs::metadata(&archive).unwrap().len();
        r.asset.sha256 = hash(&archive).unwrap();
        assert!(transaction::extract(&archive, &p.join("extract"), &r).is_err());
        unchanged(&root);
        fs::remove_dir_all(p).unwrap();
    }
}
#[cfg(feature = "update-test-hooks")]
#[test]
fn interrupted_worker() {
    if let Ok(p) = std::env::var("ARKCUT_UPDATE_WORKER_ROOT") {
        let root = PathBuf::from(p);
        let (e, r) = verify(
            &read_bounded(&root.join(".arkcut-update/offer.json"), MAX_INDEX).unwrap(),
            &config(),
            Some("0.17.0"),
        )
        .unwrap();
        transaction::install(&root, &config(), e, &r, |_, _| Ok(())).unwrap();
    }
}
#[cfg(feature = "update-test-hooks")]
#[test]
fn every_file_boundary_recovers_after_process_exit() {
    for phase in ["backup", "install"] {
        for index in 0..6 {
            let p = temp();
            let (root, _, _) = fixture(&p);
            let old = transaction::old_files(&root).unwrap();
            let exit = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("interrupted_worker")
                .env("ARKCUT_UPDATE_WORKER_ROOT", &root)
                .env("ARKCUT_UPDATE_TEST_INTERRUPT", format!("{phase}:{index}"))
                .output()
                .unwrap()
                .status;
            assert_eq!(exit.code(), Some(86));
            transaction::recover(&root, &config()).unwrap();
            transaction::recover(&root, &config()).unwrap();
            for f in &old {
                assert!(
                    matches(&root.join(&f.path), f),
                    "{phase}:{index}:{}",
                    f.path
                )
            }
            unchanged(&root);
            fs::remove_dir_all(p).unwrap();
        }
    }
}
