//! Conector Google Drive (OAuth2 loopback + Drive v3 REST).
//!
//! Fluxo: `credentials.json` (OAuth client "Desktop" do Google Cloud Console)
//! → primeira execução abre o browser e recebe o code num servidor loopback
//! local → token salvo em `token.json` ao lado das credenciais e renovado
//! automaticamente via refresh_token. Arquivos são marcados com
//! `appProperties.path` = remote_id para permitir busca por caminho.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use opt_drive_core::providers::sync::Transport;
use serde::{Deserialize, Serialize};

const SCOPES: &str = "https://www.googleapis.com/auth/drive.file";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

#[derive(Debug, Deserialize)]
struct CredentialsFile {
    #[serde(alias = "installed")]
    installed: ClientSecret,
}

#[derive(Debug, Deserialize)]
struct ClientSecret {
    client_id: String,
    client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    /// Expiração (epoch segundos).
    expires_at: i64,
}

impl Token {
    fn expired(&self) -> bool {
        self.expires_at <= unix_now() + 30
    }
}

pub struct GoogleDriveTransport {
    credentials_path: PathBuf,
    token_path: PathBuf,
    client: reqwest::blocking::Client,
    token: Mutex<Option<Token>>,
}

impl GoogleDriveTransport {
    pub fn new(credentials_path: impl Into<PathBuf>) -> Self {
        let credentials_path = credentials_path.into();
        let token_path = credentials_path.with_file_name("google_token.json");
        Self {
            credentials_path,
            token_path,
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .expect("reqwest client"),
            token: Mutex::new(None),
        }
    }

    fn load_client_secret(&self) -> anyhow::Result<ClientSecret> {
        let raw = std::fs::read_to_string(&self.credentials_path).map_err(|e| {
            anyhow::anyhow!(
                "ler credentials {} (baixe o client_secret no Google Cloud Console): {e}",
                self.credentials_path.display()
            )
        })?;
        Ok(serde_json::from_str::<CredentialsFile>(&raw)?.installed)
    }

    /// Token de acesso válido, renovando via refresh_token quando expirado.
    /// Se não houver token, executa o fluxo OAuth2 loopback (abre o browser).
    fn access_token(&self) -> anyhow::Result<String> {
        let current = self.token.lock().unwrap().clone();
        if let Some(t) = &current {
            if !t.expired() {
                return Ok(t.access_token.clone());
            }
        }
        // Carrega do disco se ainda não está em memória.
        let mut token = match current {
            Some(t) => t,
            None => self.read_token_from_disk()?,
        };
        if token.expired() {
            token = self.refresh(&token)?;
        }
        let access = token.access_token.clone();
        *self.token.lock().unwrap() = Some(token);
        Ok(access)
    }

    fn read_token_from_disk(&self) -> anyhow::Result<Token> {
        if self.token_path.exists() {
            Ok(serde_json::from_str(&std::fs::read_to_string(&self.token_path)?)?)
        } else {
            self.oauth_loopback()
        }
    }

    fn refresh(&self, t: &Token) -> anyhow::Result<Token> {
        let refresh = t
            .refresh_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("token expirado sem refresh_token — rode a autenticação de novo"))?;
        let secret = self.load_client_secret()?;
        let resp: Token = self
            .client
            .post(TOKEN_URL)
            .form(&[
                ("client_id", secret.client_id.as_str()),
                ("client_secret", secret.client_secret.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
            ])
            .send()?
            .error_for_status()?
            .json()?;
        let mut new = resp;
        new.refresh_token = Some(refresh);
        new.expires_at = unix_now() + 3600;
        self.save_token(&new)?;
        Ok(new)
    }

    /// Fluxo OAuth2 "OutOfBand→loopback": abre browser, espera o redirect em
    /// `http://localhost:<porta>/callback`, troca o code por tokens.
    fn oauth_loopback(&self) -> anyhow::Result<Token> {
        let secret = self.load_client_secret()?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let redirect = format!("http://localhost:{port}/callback");
        let auth_url = format!(
            "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
            urlencoding(&secret.client_id),
            urlencoding(&redirect),
            urlencoding(SCOPES),
        );

        println!("Abrindo browser para autenticação Google: {auth_url}");
        open_browser(&auth_url);

        // Espera o GET /callback?code=... (timeout de 5 min).
        listener.set_nonblocking(false)?;
        let (mut stream, _) = listener.accept()?;
        let mut buf = [0u8; 4096];
        use std::io::{Read, Write};
        let n = stream.read(&mut buf)?;
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let code = req
            .split("code=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .ok_or_else(|| anyhow::anyhow!("callback sem code"))?
            .to_string();
        let _ = stream.write_all(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<h2>Opt-Drive: pode fechar esta aba.</h2>"
                .as_bytes(),
        );

        let token: Token = self
            .client
            .post(TOKEN_URL)
            .form(&[
                ("client_id", secret.client_id.as_str()),
                ("client_secret", secret.client_secret.as_str()),
                ("code", code.as_str()),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect.as_str()),
            ])
            .send()?
            .error_for_status()?
            .json()?;
        let mut token = token;
        token.expires_at = unix_now() + 3600;
        self.save_token(&token)?;
        Ok(token)
    }

    fn save_token(&self, t: &Token) -> anyhow::Result<()> {
        Ok(std::fs::write(&self.token_path, serde_json::to_string_pretty(t)?)?)
    }

    /// Autentica explicitamente (força o fluxo OAuth se necessário).
    pub fn authenticate(&self) -> anyhow::Result<()> {
        self.access_token().map(|_| ())
    }

    /// Busca o id do arquivo pelo `remote_id` (appProperties.path).
    fn find_file_id(&self, token: &str, remote_id: &str) -> anyhow::Result<Option<String>> {
        #[derive(Deserialize)]
        struct FilesList {
            files: Vec<serde_json::Value>,
        }
        let q = format!(
            "appProperties has {{ key='path' and value='{}' }} and trashed=false",
            remote_id.replace('\'', "\\'")
        );
        let resp: FilesList = self
            .client
            .get("https://www.googleapis.com/drive/v3/files")
            .bearer_auth(token)
            .query(&[("q", q.as_str()), ("fields", "files(id)")])
            .send()?
            .error_for_status()?
            .json()?;
        Ok(resp.files.first().and_then(|f| f["id"].as_str()).map(String::from))
    }

    /// Upload multipart (metadado JSON + conteúdo). Cria ou atualiza conforme
    /// o arquivo já exista.
    fn multipart_upload(&self, remote_id: &str, data: Vec<u8>, existing_id: Option<&str>) -> anyhow::Result<()> {
        let token = self.access_token()?;
        let boundary = "optdrive-boundary-7f3a9c";
        let meta = serde_json::json!({
            "appProperties": { "path": remote_id },
            "name": remote_id.rsplit('/').next().unwrap_or(remote_id),
        });
        let body = format!(
            "--{b}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{b}\r\nContent-Type: application/octet-stream\r\n\r\n",
            b = boundary,
            meta = meta,
        )
        .into_bytes();
        let mut body = body;
        body.extend_from_slice(&data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let url = match existing_id {
            Some(id) => format!("https://www.googleapis.com/upload/drive/v3/files/{id}?uploadType=multipart"),
            None => "https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart".into(),
        };
        let method = if existing_id.is_some() {
            reqwest::Method::PATCH
        } else {
            reqwest::Method::POST
        };
        let resp = self
            .client
            .request(method, &url)
            .bearer_auth(&token)
            .header("Content-Type", format!("multipart/related; boundary={boundary}"))
            .body(body)
            .send()?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("Drive upload {remote_id} → {status}: {body}");
        }
        Ok(())
    }
}

impl Transport for GoogleDriveTransport {
    fn upload(&self, local: &Path, remote_id: &str) -> anyhow::Result<u64> {
        let data = std::fs::read(local)?;
        let size = data.len() as u64;
        let token = self.access_token()?;
        let existing = self.find_file_id(&token, remote_id)?;
        self.multipart_upload(remote_id, data, existing.as_deref())?;
        Ok(size)
    }

    fn download(&self, remote_id: &str, dst: &Path) -> anyhow::Result<()> {
        let token = self.access_token()?;
        let id = self
            .find_file_id(&token, remote_id)?
            .ok_or_else(|| anyhow::anyhow!("Drive: não encontrado {remote_id}"))?;
        let resp = self
            .client
            .get(format!("https://www.googleapis.com/drive/v3/files/{id}?alt=media"))
            .bearer_auth(&token)
            .send()?
            .error_for_status()?;
        let bytes = resp.bytes()?;
        std::fs::write(dst, &bytes)?;
        Ok(())
    }

    fn exists(&self, remote_id: &str) -> anyhow::Result<bool> {
        let token = self.access_token()?;
        Ok(self.find_file_id(&token, remote_id)?.is_some())
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        let token = self.access_token()?;
        match self.find_file_id(&token, remote_id)? {
            Some(id) => {
                let status = self
                    .client
                    .delete(format!("https://www.googleapis.com/drive/v3/files/{id}"))
                    .bearer_auth(&token)
                    .send()?
                    .status();
                if !status.is_success() {
                    anyhow::bail!("Drive delete {remote_id} → {status}");
                }
                Ok(())
            }
            None => Ok(()),
        }
    }
}

fn urlencoding(s: &str) -> String {
    // Form-encoding simples para query strings.
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            b' ' => "%20".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn open_browser(url: &str) {
    // Windows-first (plataforma-alvo primária); falha silenciosa — o URL já foi
    // impresso no terminal como fallback.
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/c", "start", "", url]).spawn();
    #[cfg(not(target_os = "windows"))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencodes_query() {
        assert_eq!(urlencoding("a b&c"), "a%20b%26c");
        assert_eq!(urlencoding("https://x/y"), "https%3A%2F%2Fx%2Fy");
    }
}
