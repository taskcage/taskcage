use serde_json::{Value, json};
use taskcaged::digest::Sha256Digest;
use taskcaged::product_manifest::{
    MAX_MANIFEST_BYTES, ManifestError, parse_bundle, parse_execution_profile, parse_runtime_package,
};

const PROFILE: &[u8] = include_bytes!("../../product-fixtures/v1/ffmpeg-transcode-profile.json");
const PACKAGE: &[u8] = include_bytes!("../../product-fixtures/v1/ffmpeg-runtime-package.json");
const BUNDLE: &[u8] = include_bytes!("../../product-fixtures/v1/ffmpeg-transcode-bundle.json");

const PROFILE_DIGEST: &str =
    "sha256:01d667dade05be47cbd6fc285aa4e13acde1961a2516b82b6b72c35591890199";
const PACKAGE_DIGEST: &str =
    "sha256:49c3a4b8e209375766448c957f06740fae824c12f002eda5f69e700d9e4425c6";
const BUNDLE_DIGEST: &str =
    "sha256:e11581dc8be885c4fed87fb9705200d4b2390fe85be2ff8af4ac49e01346f477";

#[test]
fn normative_fixtures_validate_and_match_pinned_digests() {
    let profile = parse_execution_profile(PROFILE).unwrap();
    let package = parse_runtime_package(PACKAGE).unwrap();
    let bundle = parse_bundle(BUNDLE, &package).unwrap();

    assert_eq!(profile.digest().to_string(), PROFILE_DIGEST);
    assert_eq!(package.digest().to_string(), PACKAGE_DIGEST);
    assert_eq!(bundle.digest().to_string(), BUNDLE_DIGEST);
    assert_eq!(
        package.digest().hex(),
        "49c3a4b8e209375766448c957f06740fae824c12f002eda5f69e700d9e4425c6"
    );
    assert_eq!(bundle.manifest().integrity.profile_digest, profile.digest());
    assert_eq!(bundle.manifest().runtime_package.digest, package.digest());
    assert!(!profile.canonical_json().contains(&b'\n'));
}

#[test]
fn canonical_digest_is_stable_across_whitespace_and_member_order() {
    let original = String::from_utf8(PROFILE.to_vec()).unwrap();
    let reordered = original.replace(
        "  \"schemaVersion\": \"taskcage.execution-profile/v0alpha1\",\n  \"id\": \"org.taskcage.ffmpeg.transcode\",\n",
        "  \"id\": \"org.taskcage.ffmpeg.transcode\",\n  \"schemaVersion\": \"taskcage.execution-profile/v0alpha1\",\n",
    );
    assert_ne!(original, reordered);

    let compact = serde_json::to_vec(&serde_json::from_slice::<Value>(PROFILE).unwrap()).unwrap();
    let expected = parse_execution_profile(PROFILE).unwrap().digest();
    assert_eq!(
        parse_execution_profile(reordered.as_bytes())
            .unwrap()
            .digest(),
        expected
    );
    assert_eq!(
        parse_execution_profile(&compact).unwrap().digest(),
        expected
    );
}

#[test]
fn rejects_duplicate_unknown_self_digest_and_floating_numbers() {
    let duplicate = String::from_utf8(PROFILE.to_vec()).unwrap().replace(
        "  \"id\": \"org.taskcage.ffmpeg.transcode\",",
        "  \"id\": \"org.taskcage.ffmpeg.transcode\",\n  \"id\": \"org.taskcage.other\",",
    );
    assert_json_error(
        parse_execution_profile(duplicate.as_bytes()),
        "duplicate JSON object key",
    );

    let mut unknown = profile_value();
    unknown["digest"] = json!(PROFILE_DIGEST);
    assert_json_error(parse_execution_profile(&encode(&unknown)), "unknown field");

    let floating = String::from_utf8(PROFILE.to_vec())
        .unwrap()
        .replace("536870912", "1.5");
    assert_json_error(
        parse_execution_profile(floating.as_bytes()),
        "floating-point",
    );

    let exponent = String::from_utf8(PROFILE.to_vec())
        .unwrap()
        .replace("120000", "1e5");
    assert_json_error(
        parse_execution_profile(exponent.as_bytes()),
        "floating-point",
    );
}

#[test]
fn rejects_noncanonical_names_semver_and_digests() {
    for invalid_name in [
        "Upper".to_owned(),
        "a.b".to_owned(),
        "_name".to_owned(),
        "a/b".to_owned(),
        "a".repeat(65),
    ] {
        let mut profile = profile_value();
        profile["inputSchema"]["scalars"]
            .as_object_mut()
            .unwrap()
            .insert(invalid_name, json!({"type": "boolean", "required": true}));
        assert_invalid(parse_execution_profile(&encode(&profile)));
    }

    for version in ["01.0.0", "1.0", "v1.0.0", "1.0.0+"] {
        let mut profile = profile_value();
        profile["version"] = json!(version);
        assert_invalid(parse_execution_profile(&encode(&profile)));
    }

    for digest in [
        "sha256:ABCDEF",
        "SHA-256:49c3a4b8e209375766448c957f06740fae824c12f002eda5f69e700d9e4425c6",
        "sha256:49c3a4b8e209375766448c957f06740fae824c12f002eda5f69e700d9e4425cg",
    ] {
        assert!(digest.parse::<Sha256Digest>().is_err());
    }
}

#[test]
fn rejects_unsafe_relative_paths_and_file_modes() {
    for path in [
        "/bin/ffmpeg",
        "../bin/ffmpeg",
        "bin/../ffmpeg",
        "bin//ffmpeg",
        "bin\\ffmpeg",
        ".taskcage/bin/ffmpeg",
        "bin/ffmpeg/",
        "bin/\u{7f}ffmpeg",
    ] {
        let mut package = package_value();
        package["entrypoint"] = json!(path);
        assert_invalid(parse_runtime_package(&encode(&package)));
    }

    let mut package = package_value();
    package["files"][0]["mode"] = json!("0755");
    assert_invalid(parse_runtime_package(&encode(&package)));

    let mut package = package_value();
    package["files"].as_array_mut().unwrap().swap(0, 1);
    assert_invalid(parse_runtime_package(&encode(&package)));

    let mut package = package_value();
    let duplicate = package["files"][0].clone();
    package["files"]
        .as_array_mut()
        .unwrap()
        .insert(1, duplicate);
    assert_invalid(parse_runtime_package(&encode(&package)));
}

#[test]
fn rejects_unsupported_argv_and_open_references() {
    let mut profile = profile_value();
    profile["argv"][0] = json!({"kind": "shell", "value": "echo unsafe"});
    assert_json_error(
        parse_execution_profile(&encode(&profile)),
        "unknown variant",
    );

    let mut profile = profile_value();
    profile["argv"][6]["slot"] = json!("missing");
    assert_invalid(parse_execution_profile(&encode(&profile)));

    let mut profile = profile_value();
    profile["argv"][14]["input"] = json!("missing");
    assert_invalid(parse_execution_profile(&encode(&profile)));

    let mut profile = profile_value();
    profile["argv"][23] = json!({"kind": "artifact", "slot": "source"});
    assert_invalid(parse_execution_profile(&encode(&profile)));

    let mut profile = profile_value();
    profile["argv"][14]["cases"][0]["equals"] = json!("18");
    assert_invalid(parse_execution_profile(&encode(&profile)));

    let mut profile = profile_value();
    let output = profile["outputSchema"]["artifacts"]
        .as_object_mut()
        .unwrap()
        .remove("result")
        .unwrap();
    profile["outputSchema"]["artifacts"]["source"] = output;
    assert_invalid(parse_execution_profile(&encode(&profile)));
}

#[test]
fn bundle_closes_embedded_profile_and_runtime_package_references() {
    let package = parse_runtime_package(PACKAGE).unwrap();

    let mut bundle = bundle_value();
    bundle["integrity"]["profileDigest"] =
        json!("sha256:0000000000000000000000000000000000000000000000000000000000000000");
    assert_invalid(parse_bundle(&encode(&bundle), &package));

    let mut bundle = bundle_value();
    bundle["runtimePackage"]["digest"] =
        json!("sha256:0000000000000000000000000000000000000000000000000000000000000000");
    bundle["integrity"]["runtimePackageDigest"] =
        json!("sha256:0000000000000000000000000000000000000000000000000000000000000000");
    assert_invalid(parse_bundle(&encode(&bundle), &package));

    let mut bundle = bundle_value();
    bundle["profile"]["version"] = json!("1.0.1");
    assert_invalid(parse_bundle(&encode(&bundle), &package));

    let mut bundle = bundle_value();
    bundle["platform"]["architecture"] = json!("aarch64");
    assert_invalid(parse_bundle(&encode(&bundle), &package));
}

#[test]
fn enforces_manifest_count_and_exact_integer_bounds() {
    let oversized = vec![b' '; MAX_MANIFEST_BYTES + 1];
    assert!(matches!(
        parse_execution_profile(&oversized),
        Err(ManifestError::TooLarge { .. })
    ));

    let mut profile = profile_value();
    profile["argv"] = Value::Array(vec![json!({"kind": "literal", "value": "x"}); 257]);
    assert_invalid(parse_execution_profile(&encode(&profile)));

    let too_large_integer = String::from_utf8(PROFILE.to_vec())
        .unwrap()
        .replace("2147483648", "9007199254740992");
    assert_json_error(
        parse_execution_profile(too_large_integer.as_bytes()),
        "exact I-JSON range",
    );

    let mut package = package_value();
    package["files"] = Value::Array(vec![package["files"][0].clone(); 4097]);
    assert_invalid(parse_runtime_package(&encode(&package)));
}

#[test]
fn rejects_inconsistent_policy_platform_and_resource_bounds() {
    let mut profile = profile_value();
    profile["resourcePolicy"]["defaults"]["limits"]["memoryMaxBytes"] = json!(2147483649_u64);
    assert_invalid(parse_execution_profile(&encode(&profile)));

    let mut profile = profile_value();
    profile["resourcePolicy"]["defaults"]["output"]["stdoutTailMaxBytes"] = json!(65537);
    assert_invalid(parse_execution_profile(&encode(&profile)));

    for key in ["PATH", "LD_PRELOAD", "LD_LIBRARY_PATH", "LD_AUDIT"] {
        let mut profile = profile_value();
        profile["environment"][key] = json!("/host/escape");
        assert_invalid(parse_execution_profile(&encode(&profile)));
    }

    let mut package = package_value();
    package["platform"]["os"] = json!("windows");
    assert_invalid(parse_runtime_package(&encode(&package)));

    let package = parse_runtime_package(PACKAGE).unwrap();
    let mut bundle = bundle_value();
    bundle["policy"]["overwritePublishedArtifacts"] = json!(true);
    assert_invalid(parse_bundle(&encode(&bundle), &package));
}

fn profile_value() -> Value {
    serde_json::from_slice(PROFILE).unwrap()
}

fn package_value() -> Value {
    serde_json::from_slice(PACKAGE).unwrap()
}

fn bundle_value() -> Value {
    serde_json::from_slice(BUNDLE).unwrap()
}

fn encode(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn assert_invalid<T>(result: Result<T, ManifestError>) {
    assert!(matches!(result, Err(ManifestError::Invalid { .. })));
}

fn assert_json_error<T>(result: Result<T, ManifestError>, expected: &str) {
    match result {
        Err(ManifestError::Json(message)) => assert!(
            message.contains(expected),
            "expected `{expected}` in `{message}`"
        ),
        _ => panic!("expected JSON error"),
    }
}
