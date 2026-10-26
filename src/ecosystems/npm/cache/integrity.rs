use std::{io::Read, path::Path};

use base64::{Engine, engine::general_purpose::STANDARD};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

use super::StorageError;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IntegrityExpectation {
    pub integrity: Option<String>,
    pub shasum: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedArtifact {
    pub local_sha256: String,
    pub upstream_verified: bool,
}

pub(super) fn verify_contents(
    artifact_key: &str,
    expectation: &IntegrityExpectation,
    contents: &[u8],
) -> Result<VerificationResult, StorageError> {
    let plan = VerificationPlan::new(expectation)?;
    let mut contents = std::io::Cursor::new(contents);
    let digests = hash_reader(&mut contents, &plan)?;
    verify_digests(artifact_key, &plan, digests)
}

pub(super) fn verify_file(
    artifact_key: &str,
    expectation: &IntegrityExpectation,
    path: &Path,
) -> Result<VerificationResult, StorageError> {
    let plan = VerificationPlan::new(expectation)?;
    let mut file = std::fs::File::open(path)?;
    let digests = hash_reader(&mut file, &plan)?;
    verify_digests(artifact_key, &plan, digests)
}

pub(super) struct VerificationResult {
    pub(super) local_sha256: String,
    pub(super) upstream_verified: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum IntegrityAlgorithm {
    Sha256,
    Sha384,
    Sha512,
}

struct ExpectedIntegrity {
    algorithm: IntegrityAlgorithm,
    digest: Vec<u8>,
}

struct VerificationPlan {
    integrity: Vec<ExpectedIntegrity>,
    shasum: Option<String>,
}

impl VerificationPlan {
    fn new(expectation: &IntegrityExpectation) -> Result<Self, StorageError> {
        if let Some(integrity) = expectation.integrity.as_deref() {
            let mut expected = Vec::new();
            for token in integrity.split_whitespace() {
                let Some((algorithm, encoded)) = token.split_once('-') else {
                    continue;
                };
                let digest = STANDARD
                    .decode(encoded)
                    .map_err(|_| StorageError::InvalidIntegrity(integrity.to_owned()))?;
                let algorithm = match algorithm {
                    "sha512" => IntegrityAlgorithm::Sha512,
                    "sha384" => IntegrityAlgorithm::Sha384,
                    "sha256" => IntegrityAlgorithm::Sha256,
                    _ => continue,
                };
                expected.push(ExpectedIntegrity { algorithm, digest });
            }
            if expected.is_empty() {
                return Err(StorageError::InvalidIntegrity(integrity.to_owned()));
            }
            return Ok(Self {
                integrity: expected,
                shasum: None,
            });
        }
        if let Some(shasum) = expectation.shasum.as_deref() {
            if shasum.len() != 40 || !shasum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(StorageError::InvalidIntegrity(shasum.to_owned()));
            }
            return Ok(Self {
                integrity: Vec::new(),
                shasum: Some(shasum.to_owned()),
            });
        }
        Ok(Self {
            integrity: Vec::new(),
            shasum: None,
        })
    }

    fn needs(&self, algorithm: IntegrityAlgorithm) -> bool {
        self.integrity
            .iter()
            .any(|expected| expected.algorithm == algorithm)
    }
}

struct FileDigests {
    sha256: Vec<u8>,
    sha1: Option<Vec<u8>>,
    sha384: Option<Vec<u8>>,
    sha512: Option<Vec<u8>>,
}

fn hash_reader<R: Read>(
    reader: &mut R,
    plan: &VerificationPlan,
) -> Result<FileDigests, StorageError> {
    let mut sha256 = Sha256::new();
    let mut sha1 = plan.shasum.as_ref().map(|_| Sha1::new());
    let mut sha384 = plan.needs(IntegrityAlgorithm::Sha384).then(Sha384::new);
    let mut sha512 = plan.needs(IntegrityAlgorithm::Sha512).then(Sha512::new);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        sha256.update(chunk);
        if let Some(hasher) = &mut sha1 {
            hasher.update(chunk);
        }
        if let Some(hasher) = &mut sha384 {
            hasher.update(chunk);
        }
        if let Some(hasher) = &mut sha512 {
            hasher.update(chunk);
        }
    }
    Ok(FileDigests {
        sha256: sha256.finalize().to_vec(),
        sha1: sha1.map(|hasher| hasher.finalize().to_vec()),
        sha384: sha384.map(|hasher| hasher.finalize().to_vec()),
        sha512: sha512.map(|hasher| hasher.finalize().to_vec()),
    })
}

fn verify_digests(
    artifact_key: &str,
    plan: &VerificationPlan,
    digests: FileDigests,
) -> Result<VerificationResult, StorageError> {
    if !plan.integrity.is_empty() {
        for expected in &plan.integrity {
            let actual = match expected.algorithm {
                IntegrityAlgorithm::Sha256 => &digests.sha256,
                IntegrityAlgorithm::Sha384 => digests
                    .sha384
                    .as_ref()
                    .expect("SHA-384 digest is computed when requested"),
                IntegrityAlgorithm::Sha512 => digests
                    .sha512
                    .as_ref()
                    .expect("SHA-512 digest is computed when requested"),
            };
            if actual == &expected.digest {
                return Ok(VerificationResult {
                    local_sha256: hex(&digests.sha256),
                    upstream_verified: true,
                });
            }
        }
        return Err(StorageError::IntegrityMismatch(artifact_key.to_owned()));
    }
    if let Some(shasum) = plan.shasum.as_deref() {
        let actual = hex(digests
            .sha1
            .as_ref()
            .expect("SHA-1 digest is computed when requested"));
        if !actual.eq_ignore_ascii_case(shasum) {
            return Err(StorageError::IntegrityMismatch(artifact_key.to_owned()));
        }
        return Ok(VerificationResult {
            local_sha256: hex(&digests.sha256),
            upstream_verified: true,
        });
    }
    Ok(VerificationResult {
        local_sha256: hex(&digests.sha256),
        upstream_verified: false,
    })
}

fn hex(contents: &[u8]) -> String {
    contents.iter().map(|byte| format!("{byte:02x}")).collect()
}
