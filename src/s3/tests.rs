use super::*;
use crate::keyfile::KeyFile;

fn config() -> Config {
    Config {
        bucket: "sekeeper-example".into(),
        ..Config::default()
    }
}

#[test]
fn config_rejects_endpoint_and_path_injection() {
    let good = config();
    good.validate().unwrap();
    for bucket in [
        "other.example/path",
        "bucket\nheader",
        "-bucket",
        "bucket..test",
        "BUCKET",
    ] {
        let mut bad = good.clone();
        bad.bucket = bucket.into();
        assert!(bad.validate().is_err());
    }
    for prefix in [
        "../",
        "sekeeper/../",
        "/sekeeper/",
        "sekeeper/?x=",
        "sekeeper/\n",
    ] {
        let mut bad = good.clone();
        bad.prefix = prefix.into();
        assert!(bad.validate().is_err());
    }
    for region in [
        "eu-central-1.evil.test",
        "eu-central-1/path",
        "eu-central-1\nheader",
    ] {
        let mut bad = good.clone();
        bad.region = region.into();
        assert!(bad.validate().is_err());
    }
    let json = serde_json::to_string(&good).unwrap();
    assert!(!json.contains("secret_key"));
    assert!(!json.contains("session_token"));
}

#[test]
fn binary_response_is_preserved_and_http_failures_remain_errors() {
    let body = vec![0, 255, b'\n', b'2', b'0', b'0', 0];
    let mut response = body.clone();
    response.extend_from_slice(b"\n200");
    let (status, parsed) = parse_response(response).unwrap();
    assert_eq!(status, 200);
    assert_eq!(parsed, body);
    assert!(parse_response(b"truncated".to_vec()).is_err());
    for status in [301, 400, 403, 404, 409, 412, 500] {
        assert!(check_status(status).is_err());
    }
}

#[test]
fn listing_supports_namespaces_empty_pages_and_escaped_continuation_tokens() {
    let xml = br#"<?xml version="1.0"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><IsTruncated>true</IsTruncated><Contents><Key>sekeeper/personale/versions/0123456789abcdef0123456789abcdef.sek</Key><Size>123</Size></Contents><NextContinuationToken>a+b/c=&amp;x</NextContinuationToken></ListBucketResult>"#;
    let listing = parse_listing(xml).unwrap();
    assert!(listing.is_truncated);
    assert_eq!(listing.contents[0].size, 123);
    assert_eq!(listing.next_token.as_deref(), Some("a+b/c=&x"));
    assert_eq!(
        encode(listing.next_token.as_deref().unwrap(), false),
        "a%2Bb%2Fc%3D%26x"
    );
    assert!(
        parse_listing(b"<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>")
            .unwrap()
            .contents
            .is_empty()
    );
    assert!(parse_listing(b"not XML").is_err());
}

#[test]
fn credentials_reject_control_characters_and_config_escaping_is_literal() {
    let mut credentials = Credentials {
        access_key: "AKIAEXAMPLEONLY123456".into(),
        secret_key: "public-test-secret".into(),
        session_token: "temporary+/=token".into(),
    };
    credentials.validate().unwrap();
    credentials.secret_key.push('\n');
    assert!(credentials.validate().is_err());
    assert_eq!(escape("a\"b\\c").as_str(), "a\\\"b\\\\c");
}

#[test]
fn cache_identity_is_per_destination_not_per_device_or_credentials() {
    let first = config();
    let mut second = first.clone();
    second.device = "Altro PC".into();
    let path = Path::new("/tmp/example-vault");
    assert_eq!(first.cache(path), second.cache(path));
    second.prefix = "sekeeper/altro/".into();
    assert_ne!(first.cache(path), second.cache(path));
}

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("sekeeper-s3-test-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn configuration_distinguishes_failures_before_and_after_replacement() {
    let directory = TestDirectory::new();
    let vault = directory.0.join("vault");
    let first = config();
    assert!(matches!(
        first.save(&vault).unwrap(),
        storage::SaveOutcome::Durable
    ));
    let mut changed = first.clone();
    changed.prefix = "sekeeper/altro/".into();
    storage::inject_failure(&Config::path(&vault), storage::FailureStage::BeforeCommit);
    assert!(changed.save(&vault).is_err());
    assert_eq!(Config::load(&vault).unwrap().unwrap().prefix, first.prefix);
    storage::inject_failure(&Config::path(&vault), storage::FailureStage::AfterCommit);
    assert!(matches!(
        changed.save(&vault).unwrap(),
        storage::SaveOutcome::DurabilityUncertain(_)
    ));
    assert_eq!(
        Config::load(&vault).unwrap().unwrap().prefix,
        changed.prefix
    );
}

#[test]
fn curl_receives_secrets_only_on_stdin_and_uploads_are_conditional() {
    use std::os::unix::fs::PermissionsExt;
    let directory = TestDirectory::new();
    let program = directory.0.join("fake-curl");
    let args = directory.0.join("args");
    let stdin = directory.0.join("stdin");
    let uploaded = directory.0.join("uploaded");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat > '{}'\nwhile [ \"$#\" -gt 0 ]; do if [ \"$1\" = --upload-file ]; then shift; cat \"$1\" > '{}'; fi; shift; done\nprintf 'response\\n200'\n",
        args.display(),
        stdin.display(),
        uploaded.display()
    );
    let outcome = storage::write_atomic(&program, script.as_bytes(), true).unwrap();
    assert!(matches!(outcome, storage::SaveOutcome::Durable));
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let client = S3 {
        config: config(),
        credentials: Credentials {
            access_key: "AKIAEXAMPLEONLY123456".into(),
            secret_key: "TEST-SECRET-ONLY".into(),
            session_token: "TEST-TOKEN-ONLY".into(),
        },
        deadline: Instant::now() + Duration::from_secs(10),
    };
    let (status, bytes) = client
        .request_with(
            &program,
            "sekeeper/test.sek",
            "",
            Some(b"encrypted test body"),
        )
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes, b"response");
    assert_eq!(std::fs::read(uploaded).unwrap(), b"encrypted test body");
    let args = std::fs::read_to_string(args).unwrap();
    let stdin = std::fs::read_to_string(stdin).unwrap();
    assert!(args.starts_with("--disable\n"));
    assert!(args.contains("If-None-Match: *"));
    assert!(args.contains("aws:amz:eu-central-1:s3"));
    assert!(args.contains("--proto\n=https\n"));
    assert!(args.contains("--upload-file\n/proc/self/fd/"));
    for secret in [
        "AKIAEXAMPLEONLY123456",
        "TEST-SECRET-ONLY",
        "TEST-TOKEN-ONLY",
    ] {
        assert!(!args.contains(secret));
        assert!(stdin.contains(secret));
    }
}

#[test]
#[ignore = "richiede credenziali esplicite SEKEEPER_S3_TEST_* e carica copie fittizie su AWS"]
fn aws_s3_live_roundtrip_and_conditional_upload() {
    use crate::vault::Vault;
    let directory = TestDirectory::new();
    let required = |name| {
        std::env::var(name)
            .expect("configurare le variabili SEKEEPER_S3_TEST_* indicate nel README")
    };
    let config = Config {
        bucket: required("SEKEEPER_S3_TEST_BUCKET"),
        region: required("SEKEEPER_S3_TEST_REGION"),
        prefix: format!("sekeeper/integration-test-{:032x}/", rand::random::<u128>()),
        ..Config::default()
    };
    config.validate().unwrap();
    let credentials = Credentials {
        access_key: required("SEKEEPER_S3_TEST_ACCESS_KEY_ID"),
        secret_key: required("SEKEEPER_S3_TEST_SECRET_ACCESS_KEY"),
        session_token: std::env::var("SEKEEPER_S3_TEST_SESSION_TOKEN").unwrap_or_default(),
    };
    credentials.validate().unwrap();
    let mut remote = S3 {
        config,
        credentials,
        deadline: Instant::now() + Duration::from_secs(60),
    };
    let (mut vault, outcome) = Vault::create(
        &directory.0.join("vault"),
        "public integration test password",
        &KeyFile::test_key(),
    )
    .unwrap();
    assert!(matches!(outcome, storage::SaveOutcome::Durable));
    let outcome = vault.enable_sync("Test di integrazione").unwrap();
    assert!(matches!(outcome, storage::SaveOutcome::Durable));
    let id = vault.data.revision.as_ref().unwrap().id.clone();
    remote.put(&id, vault.encrypted()).unwrap();
    assert!(remote.list().unwrap().contains(&id));
    assert_eq!(remote.get(&id).unwrap(), vault.encrypted());
    remote.put(&id, vault.encrypted()).unwrap();
    assert!(remote.put(&id, b"different test data").is_err());
    assert_eq!(remote.get(&id).unwrap(), vault.encrypted());
}
