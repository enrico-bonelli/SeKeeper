use crate::{helpers, history, storage};
use sekeeper_core::i18n::{is_italian, tr};
use serde::{Deserialize, Serialize};
use std::fmt::{self, Write as _};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[derive(Debug)]
pub enum CloudError {
    Message(String),
    Io(std::io::Error),
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(message) => f.write_str(message),
            Self::Io(error) => write!(
                f,
                "{}: {error}",
                tr("local S3 file error", "errore nei file locali S3")
            ),
        }
    }
}

impl std::error::Error for CloudError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Message(_) => None,
        }
    }
}

impl From<std::io::Error> for CloudError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
pub type Result<T> = std::result::Result<T, CloudError>;
pub fn error(message: &str) -> CloudError {
    CloudError::Message(message.into())
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub enabled: bool,
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub device: String,
    pub credential_id: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            bucket: String::new(),
            region: "eu-central-1".into(),
            prefix: tr("sekeeper/personal/", "sekeeper/personale/").into(),
            device: tr("This PC", "Questo PC").into(),
            credential_id: format!("{:032x}", rand::random::<u128>()),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if !(3..=63).contains(&self.bucket.len())
            || !self
                .bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
            || self
                .bucket
                .split('.')
                .any(|part| part.is_empty() || part.starts_with('-') || part.ends_with('-'))
        {
            return Err(error(tr(
                "invalid S3 bucket name",
                "nome del bucket S3 non valido",
            )));
        }
        if self.region.len() > 32
            || self.region.split('-').count() < 3
            || !self
                .region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(error(tr(
                "invalid AWS region, for example eu-central-1",
                "regione AWS non valida, per esempio eu-central-1",
            )));
        }
        if self.prefix.is_empty()
            || self.prefix.len() > 256
            || !self.prefix.ends_with('/')
            || self.prefix.starts_with('/')
            || self
                .prefix
                .split('/')
                .any(|part| part == "." || part == "..")
            || !self
                .prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
        {
            return Err(error(tr(
                "invalid prefix: use letters, numbers, /, -, _ and . and end with /",
                "prefisso non valido: usa lettere, numeri, /, -, _ e . e termina con /",
            )));
        }
        if self.device.is_empty()
            || self.device.len() > 128
            || !history::valid_id(&self.credential_id)
        {
            return Err(error(tr(
                "invalid PC name or credential identifier",
                "nome del PC o identificativo delle credenziali non valido",
            )));
        }
        Ok(())
    }
    pub fn path(vault: &Path) -> PathBuf {
        storage::sibling(vault, ".s3.json")
    }
    pub fn load(vault: &Path) -> Result<Option<Self>> {
        let bytes = match storage::read(&Self::path(vault), 64 * 1024) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let config: Self = serde_json::from_slice(&bytes).map_err(|_| {
            error(tr(
                "invalid S3 configuration",
                "configurazione S3 non valida",
            ))
        })?;
        config.validate()?;
        Ok(Some(config))
    }
    pub fn save(&self, vault: &Path) -> Result<storage::SaveOutcome> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| {
            error(tr(
                "invalid S3 configuration",
                "configurazione S3 non valida",
            ))
        })?;
        Ok(storage::write_atomic(&Self::path(vault), &bytes, false)?)
    }
    pub fn remote_prefix(&self) -> String {
        format!("{}v3/versions/", self.prefix)
    }
    pub fn cache(&self, vault: &Path) -> PathBuf {
        let identity = format!("v3\n{}\n{}\n{}", self.bucket, self.region, self.prefix);
        storage::sibling(vault, ".s3-cache").join(history::fingerprint(identity.as_bytes()))
    }
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    #[serde(default)]
    pub session_token: String,
}
impl Credentials {
    pub fn validate(&self) -> Result<()> {
        if !(16..=128).contains(&self.access_key.len())
            || !self.access_key.bytes().all(|b| b.is_ascii_alphanumeric())
            || self.secret_key.is_empty()
            || self.secret_key.len() > 256
            || self.session_token.len() > 8192
            || self
                .secret_key
                .bytes()
                .chain(self.session_token.bytes())
                .any(|b| !(0x21..=0x7e).contains(&b))
        {
            return Err(error(tr(
                "invalid AWS credentials",
                "credenziali AWS non valide",
            )));
        }
        Ok(())
    }
    pub fn store(&self, config: &Config) -> Result<()> {
        self.validate()?;
        let bytes = Zeroizing::new(
            serde_json::to_vec(self)
                .map_err(|_| error(tr("invalid credentials", "credenziali non valide")))?,
        );
        let mut command = helpers::command(helpers::Program::SecretTool)?;
        let mut child = command
            .args([
                "store",
                "--label=Sekeeper AWS S3",
                "application",
                "sekeeper",
                "credential",
                &config.credential_id,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                error(tr(
                    "install secret-tool (libsecret) to store credentials in the keyring",
                    "installa secret-tool (libsecret) per salvare le credenziali nel portachiavi",
                ))
            })?;
        child
            .stdin
            .take()
            .ok_or_else(|| error(tr("keyring unavailable", "portachiavi non disponibile")))?
            .write_all(&bytes)?;
        if !child.wait()?.success() {
            return Err(error(tr(
                "unable to save to the desktop keyring: make sure it is unlocked",
                "impossibile salvare nel portachiavi del desktop: verifica che sia sbloccato",
            )));
        }
        Ok(())
    }
    /// Removes a retired keyring item once a new one is active.
    pub fn clear(config: &Config) -> Result<()> {
        config.validate()?;
        let mut command = helpers::command(helpers::Program::SecretTool)?;
        let status = command
            .args([
                "clear",
                "application",
                "sekeeper",
                "credential",
                &config.credential_id,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| {
                error(tr(
                    "secret-tool unavailable: install libsecret",
                    "secret-tool non disponibile: installa libsecret",
                ))
            })?;
        if !status.success() {
            return Err(error(tr(
                "unable to remove old S3 credentials",
                "impossibile rimuovere le vecchie credenziali S3",
            )));
        }
        Ok(())
    }
    pub fn load(config: &Config) -> Result<Self> {
        let mut command = helpers::command(helpers::Program::SecretTool)?;
        let output = command
            .args([
                "lookup",
                "application",
                "sekeeper",
                "credential",
                &config.credential_id,
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| {
                error(tr(
                    "secret-tool unavailable: install libsecret",
                    "secret-tool non disponibile: installa libsecret",
                ))
            })?;
        let bytes = Zeroizing::new(output.stdout);
        if !output.status.success() {
            return Err(error(tr(
                "S3 credentials unavailable: open S3 settings and enter them again",
                "credenziali S3 non disponibili: apri le impostazioni S3 e inseriscile nuovamente",
            )));
        }
        let credentials: Self = serde_json::from_slice(&bytes).map_err(|_| {
            error(tr(
                "invalid S3 credentials in the keyring",
                "credenziali S3 non valide nel portachiavi",
            ))
        })?;
        credentials.validate()?;
        Ok(credentials)
    }
}

pub trait RemoteStore {
    fn list(&mut self) -> Result<Vec<String>>;
    fn get(&mut self, id: &str) -> Result<Vec<u8>>;
    fn put(&mut self, id: &str, bytes: &[u8]) -> Result<()>;
    fn delete(&mut self, id: &str) -> Result<()>;
}

pub struct S3 {
    config: Config,
    credentials: Credentials,
    deadline: Instant,
}
impl S3 {
    pub fn connect(config: Config) -> Result<Self> {
        config.validate()?;
        let credentials = Credentials::load(&config)?;
        Ok(Self {
            config,
            credentials,
            deadline: Instant::now() + Duration::from_secs(60),
        })
    }
    fn url(&self, key: &str, query: &str) -> String {
        let suffix = if self.config.region.starts_with("cn-") {
            "amazonaws.com.cn"
        } else {
            "amazonaws.com"
        };
        format!(
            "https://s3.{}.{suffix}/{}/{}{}",
            self.config.region,
            self.config.bucket,
            encode(key, true),
            query
        )
    }
    fn request(&self, key: &str, query: &str, upload: Option<&[u8]>) -> Result<(u16, Vec<u8>)> {
        self.request_command(
            helpers::command(helpers::Program::Curl)?,
            key,
            query,
            upload,
            false,
        )
    }

    #[cfg(test)]
    fn request_with(
        &self,
        program: &Path,
        key: &str,
        query: &str,
        upload: Option<&[u8]>,
    ) -> Result<(u16, Vec<u8>)> {
        self.request_command(helpers::testing_command(program), key, query, upload, false)
    }

    fn request_command(
        &self,
        mut command: helpers::TrustedCommand,
        key: &str,
        query: &str,
        upload: Option<&[u8]>,
        delete: bool,
    ) -> Result<(u16, Vec<u8>)> {
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_secs()
            .min(20);
        if remaining == 0 {
            return Err(error(tr(
                "sync timed out; local copies remain available",
                "tempo di sincronizzazione esaurito; le copie locali restano disponibili",
            )));
        }
        // Secrets are passed through a pipe, never argv, environment or a config file.
        let mut config = Zeroizing::new(String::new());
        writeln!(
            &mut *config,
            "user = \"{}:{}\"",
            escape(&self.credentials.access_key).as_str(),
            escape(&self.credentials.secret_key).as_str()
        )
        .unwrap();
        if !self.credentials.session_token.is_empty() {
            writeln!(
                &mut *config,
                "header = \"X-Amz-Security-Token: {}\"",
                escape(&self.credentials.session_token).as_str()
            )
            .unwrap();
        }
        command.args([
            "--disable",
            "--config",
            "-",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--connect-timeout",
            "5",
            "--max-time",
            &remaining.to_string(),
            "--max-filesize",
            &history::MAX_SNAPSHOT_BYTES.to_string(),
            "--aws-sigv4",
            &format!("aws:amz:{}:s3", self.config.region),
            "--write-out",
            "\n%{http_code}",
            "--url",
            &self.url(key, query),
        ]);
        let upload = upload.map(helpers::SealedUpload::new).transpose()?;
        if delete {
            command.args(["--request", "DELETE"]);
        }
        if let Some(upload) = &upload {
            command.args([
                "--header",
                "If-None-Match: *",
                "--header",
                "Content-Type: application/octet-stream",
            ]);
            upload.attach(&mut command);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                error(tr(
                    "curl unavailable: curl 7.75 or later with AWS SigV4 support is required",
                    "curl non disponibile: serve curl 7.75 o successivo con supporto AWS SigV4",
                ))
            })?;
        // Every exit path reaps curl: a failed write or an oversized answer
        // must not leave a zombie or a transfer running in the background.
        let stop = |child: &mut std::process::Child, message: &str| {
            let _ = child.kill();
            let _ = child.wait();
            error(message)
        };
        let written = match child.stdin.take() {
            Some(mut stdin) => stdin.write_all(config.as_bytes()),
            None => Err(std::io::ErrorKind::BrokenPipe.into()),
        };
        if written.is_err() {
            return Err(stop(
                &mut child,
                tr("unable to start curl", "impossibile avviare curl"),
            ));
        }
        // Old curl ignores --max-filesize on chunked answers: read at most one
        // copy plus the status line, instead of buffering whatever arrives.
        let limit = history::MAX_SNAPSHOT_BYTES + 4;
        let mut bytes = Vec::new();
        let Some(stdout) = child.stdout.take() else {
            return Err(stop(
                &mut child,
                tr("unable to start curl", "impossibile avviare curl"),
            ));
        };
        if stdout.take(limit + 1).read_to_end(&mut bytes).is_err() {
            return Err(stop(
                &mut child,
                tr("unreadable S3 response", "risposta S3 non leggibile"),
            ));
        }
        if bytes.len() as u64 > limit {
            return Err(stop(
                &mut child,
                tr("S3 response too large", "risposta S3 troppo grande"),
            ));
        }
        if !child.wait()?.success() {
            return Err(error(tr(
                "S3 connection failed: check network, curl, and configuration; will retry later",
                "connessione S3 non riuscita: verifica rete, curl e configurazione; riproverò più tardi",
            )));
        }
        parse_response(bytes)
    }
    fn key(&self, id: &str) -> Result<String> {
        if !history::valid_id(id) {
            return Err(error(tr(
                "invalid S3 copy identifier",
                "identificativo della copia S3 non valido",
            )));
        }
        Ok(format!("{}{id}.sek", self.config.remote_prefix()))
    }
}

/// MAX_VERSIONS copies fit in three pages of 1000 keys; a margin covers
/// pages S3 returns partially filled.
const MAX_LIST_PAGES: usize = 16;

impl RemoteStore for S3 {
    fn list(&mut self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let mut token: Option<String> = None;
        let mut seen_tokens = std::collections::HashSet::new();
        loop {
            let mut query = format!(
                "?list-type=2&max-keys=1000&prefix={}",
                encode(&self.config.remote_prefix(), false)
            );
            if let Some(token) = &token {
                write!(&mut query, "&continuation-token={}", encode(token, false)).unwrap();
            }
            let (status, bytes) = self.request("", &query, None)?;
            check_status(status)?;
            let listing = parse_listing(&bytes)?;
            for entry in listing.contents {
                let Some(name) = entry
                    .key
                    .strip_prefix(&self.config.remote_prefix())
                    .and_then(|name| name.strip_suffix(".sek"))
                else {
                    continue;
                };
                if !history::valid_id(name) || entry.size > history::MAX_SNAPSHOT_BYTES {
                    return Err(error(tr(
                        "invalid or oversized S3 copy",
                        "copia S3 non valida o troppo grande",
                    )));
                }
                ids.push(name.to_string());
                if ids.len() > history::MAX_VERSIONS {
                    return Err(error(tr(
                        "too many S3 versions: archive the history before continuing",
                        "troppe versioni S3: archivia lo storico prima di continuare",
                    )));
                }
            }
            if !listing.is_truncated {
                break;
            }
            let next = listing
                .next_token
                .filter(|s| !s.is_empty())
                .ok_or_else(|| error(tr("incomplete S3 listing", "elenco S3 incompleto")))?;
            // Empty pages with fresh tokens would otherwise go on until the deadline.
            if seen_tokens.len() >= MAX_LIST_PAGES {
                return Err(error(tr("S3 listing too long", "elenco S3 troppo lungo")));
            }
            if !seen_tokens.insert(next.clone()) {
                return Err(error(tr(
                    "invalid S3 pagination",
                    "paginazione S3 non valida",
                )));
            }
            token = Some(next);
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
    fn get(&mut self, id: &str) -> Result<Vec<u8>> {
        let (status, bytes) = self.request(&self.key(id)?, "", None)?;
        check_status(status)?;
        Ok(bytes)
    }
    fn put(&mut self, id: &str, bytes: &[u8]) -> Result<()> {
        let (status, _) = self.request(&self.key(id)?, "", Some(bytes))?;
        if status == 412 {
            if self.get(id)? == bytes {
                return Ok(());
            }
            return Err(error(tr(
                "S3 contains a different copy with the same identifier: no file was overwritten",
                "S3 contiene una copia diversa con lo stesso identificativo: nessun file è stato sovrascritto",
            )));
        }
        check_status(status)
    }
    fn delete(&mut self, id: &str) -> Result<()> {
        let (status, _) = self.request_command(
            helpers::command(helpers::Program::Curl)?,
            &self.key(id)?,
            "",
            None,
            true,
        )?;
        if status == 404 {
            return Ok(());
        }
        check_status(status)
    }
}

fn check_status(status: u16) -> Result<()> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(error(tr(
            "S3 access denied: check credentials, IAM permissions, and session token expiration",
            "accesso S3 negato: verifica credenziali, permessi IAM e scadenza dell'eventuale session token",
        ))),
        301 | 307 | 400 => Err(error(tr(
            "S3 rejected the request: check the region and bucket name",
            "S3 ha rifiutato la richiesta: verifica regione e nome del bucket",
        ))),
        404 => Err(error(tr(
            "S3 bucket or copy unavailable",
            "bucket o copia S3 non disponibile",
        ))),
        _ => Err(CloudError::Message(if is_italian() {
            format!("S3 ha risposto con errore HTTP {status}; riproverò più tardi")
        } else {
            format!("S3 returned HTTP error {status}; will retry later")
        })),
    }
}
fn parse_response(mut bytes: Vec<u8>) -> Result<(u16, Vec<u8>)> {
    if bytes.len() < 4 || bytes[bytes.len() - 4] != b'\n' {
        return Err(error(tr(
            "incomplete S3 response",
            "risposta S3 incompleta",
        )));
    }
    let status = std::str::from_utf8(&bytes[bytes.len() - 3..])
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| error(tr("invalid HTTP response", "risposta HTTP non valida")))?;
    bytes.truncate(bytes.len() - 4);
    if bytes.len() as u64 > history::MAX_SNAPSHOT_BYTES {
        return Err(error(tr(
            "S3 response too large",
            "risposta S3 troppo grande",
        )));
    }
    Ok((status, bytes))
}
fn encode(value: &str, preserve_slash: bool) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) || preserve_slash && byte == b'/'
        {
            encoded.push(byte as char);
        } else {
            write!(encoded, "%{byte:02X}").unwrap();
        }
    }
    encoded
}
fn escape(value: &str) -> Zeroizing<String> {
    let mut result = Zeroizing::new(String::with_capacity(value.len() * 2));
    for c in value.chars() {
        if c == '\\' || c == '"' {
            result.push('\\');
        }
        result.push(c);
    }
    result
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Listing {
    #[serde(default, rename = "Contents")]
    contents: Vec<Object>,
    #[serde(default)]
    is_truncated: bool,
    #[serde(rename = "NextContinuationToken")]
    next_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Object {
    key: String,
    size: u64,
}
fn parse_listing(bytes: &[u8]) -> Result<Listing> {
    quick_xml::de::from_reader(bytes).map_err(|_| {
        error(tr(
            "invalid S3 copy listing",
            "elenco delle copie S3 non valido",
        ))
    })
}

#[cfg(test)]
mod tests;
