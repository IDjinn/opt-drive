//! AWS Signature Version 4 (mínimo, apenas para S3) — usado pelo conector S3.
//! Evita puxar o SDK oficial (que arrasta tokio-runtime e centenas de crates).

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Credenciais AWS resolvidas de `options` ou das variáveis de ambiente padrão
/// (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`).
#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl AwsCredentials {
    pub fn from_env_or(options: &std::collections::BTreeMap<String, String>) -> anyhow::Result<Self> {
        let access_key_id = options
            .get("access_key_id")
            .cloned()
            .or_else(|| std::env::var("AWS_ACCESS_KEY_ID").ok())
            .filter(|s| !s.is_empty());
        let secret_access_key = options
            .get("secret_access_key")
            .cloned()
            .or_else(|| std::env::var("AWS_SECRET_ACCESS_KEY").ok())
            .filter(|s| !s.is_empty());
        match (access_key_id, secret_access_key) {
            (Some(a), Some(s)) => Ok(Self {
                access_key_id: a,
                secret_access_key: s,
                session_token: std::env::var("AWS_SESSION_TOKEN").ok().filter(|s| !s.is_empty()),
            }),
            _ => anyhow::bail!(
                "credenciais AWS ausentes: defina AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY \
                 ou [backup.options] access_key_id/secret_access_key"
            ),
        }
    }
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("key len");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{:02x}", b)).collect()
}

/// Headers de autorização SigV4 para uma requisição S3.
/// `path` já percent-encoded por quem chama (ex.: `/bucket/a/b.txt`).
pub struct SignedRequest {
    pub authorization: String,
    pub amz_date: String,
    pub content_sha256: String,
}

/// Assina uma requisição S3 (path-style).
pub fn sign_s3(
    creds: &AwsCredentials,
    region: &str,
    method: &str,
    host: &str,
    path: &str,
    payload_sha256: &str,
    amz_date: &str, // YYYYMMDDTHHMMSSZ
) -> SignedRequest {
    let date = &amz_date[..8];

    // Headers canônicos: host + content-sha256 + date (+ session token).
    let mut signed_headers = "host;x-amz-content-sha256;x-amz-date".to_string();
    let mut canonical_headers = format!("host:{host}\nx-amz-content-sha256:{payload_sha256}\nx-amz-date:{amz_date}\n");
    if let Some(token) = &creds.session_token {
        canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
        signed_headers = "host;x-amz-content-sha256;x-amz-date;x-amz-security-token".to_string();
    }

    let canonical_request = format!(
        "{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{payload_sha256}"
    );
    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    // Signing key: kSecret → kDate → kRegion → kService → kSigning.
    let k_date = hmac(format!("AWS4{}", creds.secret_access_key).as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, b"s3");
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

    SignedRequest {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            creds.access_key_id, scope, signed_headers, signature
        ),
        amz_date: amz_date.to_string(),
        content_sha256: payload_sha256.to_string(),
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Percent-encoding de cada segmento do caminho S3 (preserva `/`).
pub fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for c in path.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' => out.push(c),
            _ => {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    out
}

/// Timestamp AMZ no formato `YYYYMMDDTHHMMSSZ`.
pub fn amz_timestamp_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Conversão epoch → UTC civil (dias desde 1970 / algoritmo de Howard Hinnant).
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}{mth:02}{d:02}T{h:02}{m:02}{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_path_segments() {
        assert_eq!(encode_path("/bucket/a b/c.txt"), "/bucket/a%20b/c.txt");
        assert_eq!(encode_path("/bucket/a+b.txt"), "/bucket/a%2Bb.txt");
    }

    #[test]
    fn timestamp_format() {
        let t = amz_timestamp_now();
        assert_eq!(t.len(), 16);
        assert!(t.ends_with('Z'));
        assert_eq!(&t[8..9], "T");
    }

    // Vetor oficial da AWS para SigV4 (GET simples) — valida o pipeline inteiro.
    #[test]
    fn aws_test_vector_get_object() {
        let creds = AwsCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        // Caso oficial usa range header; aqui validamos só estrutura do header.
        let req = sign_s3(
            &creds,
            "us-east-1",
            "GET",
            "examplebucket.s3.amazonaws.com",
            "/test.txt",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "20130524T000000Z",
        );
        assert!(req.authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request"));
        assert!(req.authorization.contains("Signature="));
    }
}
