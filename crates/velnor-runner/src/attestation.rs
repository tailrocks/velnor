use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sigstore_sign::bundle::BundleV03;
use sigstore_sign::crypto::KeyPair;
use sigstore_sign::fulcio::FulcioClient;
use sigstore_sign::oidc::IdentityToken;
use sigstore_sign::rekor::RekorApiVersion;
use sigstore_sign::tsa::TimestampClient;
use sigstore_sign::types::{pae, DsseEnvelope, DsseSignature, KeyId, PayloadBytes};
use sigstore_sign::{SigningConfig, SigningContext};

const SUBJECT_LIMIT: usize = 1024;
const PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub name: String,
    pub sha256: String,
}

#[derive(Debug)]
pub struct AttestationResult {
    pub bundle_path: String,
    pub attestation_id: String,
    pub attestation_url: String,
    pub subjects: Vec<Subject>,
    pub public_good: bool,
}

pub struct AttestationRequest<'a> {
    pub workspace: &'a Path,
    pub runner_temp: &'a Path,
    pub runner_temp_container: &'a str,
    pub oidc_url: &'a str,
    pub oidc_request_token: &'a str,
    pub github_token: &'a str,
    pub api_url: &'a str,
    pub server_url: &'a str,
    pub repository: &'a str,
    pub repository_visibility: Option<&'a str>,
}

pub fn attest_build_provenance(request: AttestationRequest<'_>) -> Result<AttestationResult> {
    let subjects = collect_subjects(request.workspace)?;
    let client = Client::builder()
        .user_agent("velnor-runner")
        .timeout(Duration::from_secs(30))
        .build()
        .context("build attestation HTTP client")?;

    let claims_token = request_oidc_token(
        &client,
        request.oidc_url,
        request.oidc_request_token,
        "nobody",
    )?;
    let claims = verify_oidc_claims(&client, request.server_url, &claims_token)?;
    let statement = provenance_statement(request.server_url, &claims, &subjects)?;
    let statement_bytes =
        serde_json::to_vec(&statement).context("serialize provenance statement")?;

    let signing_token = request_oidc_token(
        &client,
        request.oidc_url,
        request.oidc_request_token,
        "sigstore",
    )?;
    let public_good = request.repository_visibility == Some("public");
    let bundle = sign_statement(
        &statement_bytes,
        &signing_token,
        public_good,
        request.server_url,
    )?;
    let bundle_json = serde_json::to_value(&bundle).context("serialize Sigstore bundle")?;

    let upload_url = format!(
        "{}/repos/{}/attestations",
        request.api_url.trim_end_matches('/'),
        request.repository
    );
    let upload = upload_attestation(&client, &upload_url, request.github_token, &bundle_json)?;
    let attestation_id = upload
        .get("id")
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_u64().map(|v| v.to_string()))
        })
        .filter(|value| !value.is_empty())
        .context("repository attestation response is missing id")?;

    fs::create_dir_all(request.runner_temp).context("create RUNNER_TEMP")?;
    let output_dir = unique_output_dir(request.runner_temp)?;
    let bundle_path = output_dir.join("attestation.json");
    let mut bundle_file = File::create(&bundle_path).context("create attestation bundle")?;
    serde_json::to_writer(&mut bundle_file, &bundle).context("write attestation bundle")?;
    writeln!(bundle_file).context("finish attestation bundle")?;
    let mut paths = OpenOptions::new()
        .create(true)
        .append(true)
        .open(request.runner_temp.join("created_attestation_paths.txt"))
        .context("open created attestation paths")?;
    let relative_bundle = bundle_path
        .strip_prefix(request.runner_temp)
        .context("attestation bundle escaped RUNNER_TEMP")?;
    let bundle_path_container = format!(
        "{}/{}",
        request.runner_temp_container.trim_end_matches('/'),
        relative_bundle.to_string_lossy()
    );
    writeln!(paths, "{bundle_path_container}").context("record attestation bundle path")?;

    let attestation_url = format!(
        "{}/{}/attestations/{}",
        request.server_url.trim_end_matches('/'),
        request.repository,
        attestation_id
    );
    Ok(AttestationResult {
        bundle_path: bundle_path_container,
        attestation_id,
        attestation_url,
        subjects,
        public_good,
    })
}

fn upload_attestation(client: &Client, url: &str, token: &str, bundle: &Value) -> Result<Value> {
    const MAX_RETRIES: u32 = 5;
    for attempt in 0..=MAX_RETRIES {
        let response = client
            .post(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .json(&json!({"bundle": bundle}))
            .send();
        match response {
            Ok(response) if response.status().is_success() => {
                return response
                    .json()
                    .context("decode repository attestation response");
            }
            Ok(response)
                if response.status().is_client_error()
                    && response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS =>
            {
                let status = response.status();
                let detail = response
                    .text()
                    .map(|body| bounded_response_detail(&body))
                    .unwrap_or_else(|_| "response body unavailable".into());
                bail!("upload repository attestation returned terminal HTTP {status}: {detail}");
            }
            Ok(response) if attempt == MAX_RETRIES => {
                bail!(
                    "upload repository attestation returned HTTP {} after {MAX_RETRIES} retries",
                    response.status()
                );
            }
            Err(error) if attempt == MAX_RETRIES => {
                return Err(error).context("upload repository attestation after bounded retries");
            }
            Ok(_) | Err(_) => {
                std::thread::sleep(Duration::from_millis(100 * 2_u64.pow(attempt)));
            }
        }
    }
    unreachable!("bounded upload loop always returns")
}

fn bounded_response_detail(body: &str) -> String {
    const LIMIT: usize = 4096;
    let mut detail = body
        .chars()
        .filter(|character| !character.is_control() || *character == ' ')
        .take(LIMIT)
        .collect::<String>();
    if body.chars().count() > LIMIT {
        detail.push('…');
    }
    if detail.is_empty() {
        "empty response body".into()
    } else {
        detail
    }
}

fn collect_subjects(workspace: &Path) -> Result<Vec<Subject>> {
    let dist = workspace.join("dist");
    let mut subjects = Vec::new();
    for entry in fs::read_dir(&dist).with_context(|| format!("read {}", dist.display()))? {
        let entry = entry.context("read dist entry")?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".tar.gz") || !entry.file_type().context("read subject type")?.is_file()
        {
            continue;
        }
        let mut reader =
            BufReader::new(File::open(&path).with_context(|| format!("open subject {name}"))?);
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = reader
                .read(&mut buffer)
                .with_context(|| format!("read subject {name}"))?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let digest = hasher.finalize();
        subjects.push(Subject {
            name,
            sha256: hex_lower(digest.as_ref()),
        });
    }
    subjects.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.sha256.cmp(&right.sha256))
    });
    subjects.dedup();
    if subjects.is_empty() {
        bail!("subject-path 'dist/*.tar.gz' matched no regular files");
    }
    if subjects.len() > SUBJECT_LIMIT {
        bail!(
            "subject-path matched {} files; maximum is {SUBJECT_LIMIT}",
            subjects.len()
        );
    }
    Ok(subjects)
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn request_oidc_token(client: &Client, url: &str, bearer: &str, audience: &str) -> Result<String> {
    let mut url = url::Url::parse(url).context("parse OIDC request URL")?;
    url.query_pairs_mut().append_pair("audience", audience);
    let response = client
        .get(url)
        .bearer_auth(bearer)
        .send()
        .with_context(|| format!("request OIDC token for audience {audience}"))?;
    if !response.status().is_success() {
        bail!(
            "OIDC token request for audience {audience} returned HTTP {}",
            response.status()
        );
    }
    let body: Value = response.json().context("decode OIDC token response")?;
    body.get("value")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .context("OIDC token response is missing value")
}

fn verify_oidc_claims(client: &Client, server_url: &str, token: &str) -> Result<Value> {
    let issuer = oidc_issuer(server_url)?;
    let configuration: Value = client
        .get(format!("{issuer}/.well-known/openid-configuration"))
        .send()
        .context("fetch OIDC configuration")?
        .error_for_status()
        .context("fetch OIDC configuration")?
        .json()
        .context("decode OIDC configuration")?;
    let jwks_uri = configuration
        .get("jwks_uri")
        .and_then(Value::as_str)
        .context("OIDC configuration is missing jwks_uri")?;
    let jwks: JwkSet = client
        .get(jwks_uri)
        .send()
        .context("fetch OIDC JWKS")?
        .error_for_status()
        .context("fetch OIDC JWKS")?
        .json()
        .context("decode OIDC JWKS")?;
    let header = decode_header(token).context("decode OIDC header")?;
    let kid = header
        .kid
        .as_deref()
        .context("OIDC header is missing kid")?;
    let jwk = jwks
        .find(kid)
        .context("OIDC signing key is absent from JWKS")?;
    let key = DecodingKey::from_jwk(jwk).context("build OIDC verification key")?;
    let mut validation = Validation::new(header.alg);
    validation.set_audience(&["nobody"]);
    let claims = decode::<Value>(token, &key, &validation)
        .context("verify OIDC token")?
        .claims;
    let actual_issuer = claim(&claims, "iss")?;
    if !actual_issuer.starts_with(&issuer) {
        bail!("unexpected OIDC issuer");
    }
    for required in [
        "ref",
        "sha",
        "repository",
        "event_name",
        "job_workflow_ref",
        "workflow_ref",
        "repository_id",
        "repository_owner_id",
        "runner_environment",
        "run_id",
        "run_attempt",
    ] {
        claim(&claims, required)?;
    }
    Ok(claims)
}

fn oidc_issuer(server_url: &str) -> Result<String> {
    let parsed = url::Url::parse(server_url).context("parse GITHUB_SERVER_URL")?;
    let host = parsed.host_str().context("GITHUB_SERVER_URL has no host")?;
    if host != "github.com" && !host.ends_with(".ghe.com") {
        bail!("invalid GitHub server URL for attestation");
    }
    let token_host = if host == "github.com" {
        "githubusercontent.com"
    } else {
        host
    };
    Ok(format!("https://token.actions.{token_host}"))
}

fn provenance_statement(server_url: &str, claims: &Value, subjects: &[Subject]) -> Result<Value> {
    let repository = claim(claims, "repository")?;
    let workflow_ref = claim(claims, "workflow_ref")?;
    let prefix = format!("{repository}/");
    let workflow_path = workflow_ref
        .strip_prefix(&prefix)
        .unwrap_or(workflow_ref)
        .split('@')
        .next()
        .unwrap_or(workflow_ref);
    Ok(json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": subjects.iter().map(|subject| json!({
            "name": subject.name,
            "digest": {"sha256": subject.sha256}
        })).collect::<Vec<_>>(),
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {
            "buildDefinition": {
                "buildType": "https://actions.github.io/buildtypes/workflow/v1",
                "externalParameters": {"workflow": {
                    "ref": claim(claims, "ref")?,
                    "repository": format!("{server_url}/{repository}"),
                    "path": workflow_path
                }},
                "internalParameters": {"github": {
                    "event_name": claim(claims, "event_name")?,
                    "repository_id": claim(claims, "repository_id")?,
                    "repository_owner_id": claim(claims, "repository_owner_id")?,
                    "runner_environment": claim(claims, "runner_environment")?
                }},
                "resolvedDependencies": [{
                    "uri": format!("git+{server_url}/{repository}@{}", claim(claims, "ref")?),
                    "digest": {"gitCommit": claim(claims, "sha")?}
                }]
            },
            "runDetails": {
                "builder": {"id": format!("{server_url}/{}", claim(claims, "job_workflow_ref")?)},
                "metadata": {"invocationId": format!(
                    "{server_url}/{repository}/actions/runs/{}/attempts/{}",
                    claim(claims, "run_id")?, claim(claims, "run_attempt")?
                )}
            }
        }
    }))
}

fn claim<'a>(claims: &'a Value, name: &str) -> Result<&'a str> {
    claims
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("OIDC token is missing {name}"))
}

fn sign_statement(
    statement: &[u8],
    token: &str,
    public_good: bool,
    server_url: &str,
) -> Result<sigstore_sign::types::Bundle> {
    let statement = statement.to_vec();
    let token = token.to_owned();
    let server_url = server_url.to_owned();
    std::thread::spawn(move || -> Result<_> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build Sigstore runtime")?;
        runtime.block_on(async move {
            let identity = IdentityToken::from_jwt(&token).context("parse Sigstore OIDC token")?;
            if public_good {
                let config = SigningConfig {
                    fulcio_url: "https://fulcio.sigstore.dev".into(),
                    rekor_url: RekorApiVersion::V2.default_url().into(),
                    tsa_url: None,
                    signing_scheme: sigstore_sign::crypto::SigningScheme::EcdsaP256Sha256,
                    rekor_api_version: RekorApiVersion::V2,
                    oidc_url: None,
                };
                return SigningContext::with_config(config)
                    .signer(identity)
                    .sign_raw_statement(&statement)
                    .await
                    .context("sign public-good attestation");
            }
            sign_private_statement(&statement, identity, &server_url).await
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("Sigstore signing thread panicked"))?
}

async fn sign_private_statement(
    statement: &[u8],
    identity: IdentityToken,
    server_url: &str,
) -> Result<sigstore_sign::types::Bundle> {
    let host = url::Url::parse(server_url)
        .context("parse GITHUB_SERVER_URL")?
        .host_str()
        .context("GITHUB_SERVER_URL has no host")?
        .to_owned();
    let host = if host == "github.com" {
        "githubapp.com".to_owned()
    } else {
        host
    };
    let key_pair = KeyPair::generate_ecdsa_p256().context("generate ephemeral signing key")?;
    let certificate = FulcioClient::new(format!("https://fulcio.{host}"))
        .create_signing_certificate(&identity, &key_pair)
        .await
        .context("request GitHub Fulcio certificate")?
        .leaf_certificate()
        .context("read GitHub Fulcio certificate")?;
    let signature = key_pair
        .sign(&pae(PAYLOAD_TYPE, statement))
        .context("sign DSSE statement")?;
    let envelope = DsseEnvelope::new(
        PAYLOAD_TYPE.to_owned(),
        PayloadBytes::from_bytes(statement),
        vec![DsseSignature {
            sig: signature.clone(),
            keyid: KeyId::default(),
        }],
    );
    let timestamp = TimestampClient::new(format!("https://timestamp.{host}/api/v1/timestamp"))
        .timestamp_signature(&signature)
        .await
        .context("request GitHub timestamp")?;
    Ok(BundleV03::with_certificate_and_dsse(certificate, envelope)
        .with_rfc3161_timestamp(timestamp)
        .into_bundle())
}

fn unique_output_dir(runner_temp: &Path) -> Result<PathBuf> {
    for index in 0_u32..1024 {
        let candidate = runner_temp.join(format!("attestation-{}-{index}", std::process::id()));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("create attestation output directory"),
        }
    }
    bail!("could not allocate unique attestation output directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_collection_is_sorted_deduplicated_and_streamed() {
        let root = std::env::temp_dir().join(format!("velnor-attest-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("dist")).unwrap();
        fs::write(root.join("dist/b.tar.gz"), b"b").unwrap();
        fs::write(root.join("dist/a.tar.gz"), b"a").unwrap();
        fs::write(root.join("dist/ignored.zip"), b"z").unwrap();
        let subjects = collect_subjects(&root).unwrap();
        assert_eq!(
            subjects
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["a.tar.gz", "b.tar.gz"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subject_collection_rejects_no_regular_archive() {
        let root = std::env::temp_dir().join(format!("velnor-attest-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("dist/not-a-file.tar.gz")).unwrap();
        fs::write(root.join("dist/ignored.zip"), b"z").unwrap();
        let error = collect_subjects(&root).unwrap_err();
        assert!(error.to_string().contains("matched no regular files"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provenance_matches_upstream_shape() {
        let claims = json!({
            "ref":"refs/heads/main", "sha":"abc", "repository":"o/r", "event_name":"push",
            "job_workflow_ref":"o/r/.github/workflows/ci.yml@refs/heads/main",
            "workflow_ref":"o/r/.github/workflows/ci.yml@refs/heads/main",
            "repository_id":"1", "repository_owner_id":"2", "runner_environment":"self-hosted",
            "run_id":"3", "run_attempt":"1"
        });
        let statement = provenance_statement(
            "https://github.com",
            &claims,
            &[Subject {
                name: "a.tar.gz".into(),
                sha256: "00".into(),
            }],
        )
        .unwrap();
        assert_eq!(
            statement["predicate"]["buildDefinition"]["externalParameters"]["workflow"]["path"],
            ".github/workflows/ci.yml"
        );
        assert_eq!(
            statement["predicate"]["runDetails"]["metadata"]["invocationId"],
            "https://github.com/o/r/actions/runs/3/attempts/1"
        );
    }

    #[test]
    fn repository_error_detail_is_bounded_and_single_line() {
        let detail = bounded_response_detail(&format!("bad\n{}", "x".repeat(5000)));
        assert!(!detail.contains('\n'));
        assert!(detail.ends_with('…'));
        assert_eq!(detail.chars().count(), 4097);
    }
}
