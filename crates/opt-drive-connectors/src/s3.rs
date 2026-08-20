//! Conector S3 (e S3-compatible: MinIO, Cloudflare R2, Backblaze B2).
//!
//! REST direto com SigV4 (ver [`crate::sigv4`]) via `reqwest` blocking — sem o
//! SDK oficial. Endereçamento path-style para funcionar com endpoints custom.

use std::collections::BTreeMap;
use std::path::Path;

use opt_drive_core::providers::sync::Transport;

use crate::sigv4::{self, AwsCredentials};

/// Transporte S3 com path-style addressing.
pub struct S3Transport {
    creds: AwsCredentials,
    bucket: String,
    region: String,
    /// Endpoint custom (ex.: `http://localhost:9000` p/ MinIO). Se ausente,
    /// usa `https://s3.<region>.amazonaws.com`.
    endpoint: Option<String>,
    client: reqwest::blocking::Client,
}

impl S3Transport {
    pub fn from_options(
        bucket: &str,
        options: &BTreeMap<String, String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            creds: AwsCredentials::from_env_or(options)?,
            bucket: bucket.to_string(),
            region: options.get("region").cloned().unwrap_or_else(|| "us-east-1".into()),
            endpoint: options.get("endpoint").cloned().filter(|s| !s.is_empty()),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()?,
        })
    }

    fn base_url(&self) -> String {
        match &self.endpoint {
            Some(ep) => format!("{}/{}", ep.trim_end_matches('/'), self.bucket),
            None => format!("https://s3.{}.amazonaws.com/{}", self.region, self.bucket),
        }
    }

    fn host(&self) -> String {
        let url = self.base_url();
        let rest = url.strip_prefix("http://").unwrap_or(&url);
        let rest = rest.strip_prefix("https://").unwrap_or(rest);
        rest.split('/').next().unwrap_or(rest).to_string()
    }

    fn request(
        &self,
        method: reqwest::Method,
        remote_id: &str,
        body: Vec<u8>,
    ) -> anyhow::Result<reqwest::blocking::Response> {
        let is_put = method == reqwest::Method::PUT;
        let path = format!("/{}/{}", self.bucket, sigv4::encode_path(remote_id));
        let payload_sha = sigv4::sha256_hex(&body);
        let signed = sigv4::sign_s3(
            &self.creds,
            &self.region,
            method.as_str(),
            &self.host(),
            &path,
            &payload_sha,
            &sigv4::amz_timestamp_now(),
        );

        let url = format!("{}{}", self.base_url(), sigv4::encode_path(remote_id));
        let mut req = self
            .client
            .request(method, &url)
            .header("x-amz-date", &signed.amz_date)
            .header("x-amz-content-sha256", &signed.content_sha256)
            .header("Authorization", &signed.authorization);
        if let Some(t) = &self.creds.session_token {
            req = req.header("x-amz-security-token", t);
        }
        let resp = if body.is_empty() && !is_put {
            req.send()?
        } else {
            // PUT com corpo vazio ainda precisa do body explicitamente.
            req.body(body).send()?
        };
        Ok(resp)
    }
}

impl Transport for S3Transport {
    fn upload(&self, local: &Path, remote_id: &str) -> anyhow::Result<u64> {
        let data = std::fs::read(local)?;
        let size = data.len() as u64;
        let resp = self.request(reqwest::Method::PUT, remote_id, data)?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("S3 PUT {} → {}", remote_id, status);
        }
        Ok(size)
    }

    fn download(&self, remote_id: &str, dst: &Path) -> anyhow::Result<()> {
        let resp = self.request(reqwest::Method::GET, remote_id, Vec::new())?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("S3 GET {} → {}", remote_id, status);
        }
        let bytes = resp.bytes()?;
        std::fs::write(dst, &bytes)?;
        Ok(())
    }

    fn exists(&self, remote_id: &str) -> anyhow::Result<bool> {
        let resp = self.request(reqwest::Method::HEAD, remote_id, Vec::new())?;
        match resp.status() {
            reqwest::StatusCode::OK => Ok(true),
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            s => anyhow::bail!("S3 HEAD {} → {}", remote_id, s),
        }
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        let resp = self.request(reqwest::Method::DELETE, remote_id, Vec::new())?;
        let status = resp.status();
        // 204 sucesso; 404 = já apagado (idempotente).
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            anyhow::bail!("S3 DELETE {} → {}", remote_id, status);
        }
    }
}
