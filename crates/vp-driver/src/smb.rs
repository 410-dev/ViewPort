//! SMB2/3 driver (pure Rust, via the `smb2` crate).
//!
//! Config: `host`, `port` (445), `share`, `username`, `password`, `domain`.
//! The source root is a folder inside the share (`""` for the share root).
//!
//! The protocol client is async; this driver runs it on its own small runtime and
//! hands results back over a channel, so the blocking [`MediaDriver`] API can be called
//! from any thread, including other runtimes' blocking pools.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::{
    Capabilities, ConfigField, ConfigKind, DriverError, DriverFactory, Entry, MediaDriver, MediaReader, Result,
    checked_components,
};

#[derive(Debug, Clone, Deserialize)]
struct SmbConfig {
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    share: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    domain: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_port() -> u16 {
    445
}

fn default_timeout() -> u64 {
    10
}

pub struct SmbFactory;

impl DriverFactory for SmbFactory {
    fn kind(&self) -> &'static str {
        "smb"
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField::new("host", ConfigKind::Text, true),
            ConfigField::new("port", ConfigKind::Integer { min: Some(1), max: Some(65535) }, false)
                .default_value(serde_json::json!(445)),
            ConfigField::new("share", ConfigKind::Text, true),
            ConfigField::new("username", ConfigKind::Text, false),
            ConfigField::new("password", ConfigKind::Secret, false),
            ConfigField::new("domain", ConfigKind::Text, false),
            ConfigField::new("timeout_secs", ConfigKind::Integer { min: Some(1), max: Some(300) }, false)
                .default_value(serde_json::json!(10)),
        ]
    }

    fn connect(&self, root: &str, config: &serde_json::Value) -> Result<Arc<dyn MediaDriver>> {
        let cfg: SmbConfig =
            serde_json::from_value(config.clone()).map_err(|e| DriverError::InvalidConfig(e.to_string()))?;
        if cfg.host.trim().is_empty() || cfg.share.trim().is_empty() {
            return Err(DriverError::InvalidConfig("host and share are required".into()));
        }
        let root: Vec<String> = checked_components(root)?.into_iter().map(String::from).collect();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("smb")
            .enable_all()
            .build()
            .map_err(DriverError::Io)?;
        let rt = Arc::new(rt);
        let session = run(&rt, {
            let cfg = cfg.clone();
            async move { Session::open(&cfg).await }
        })??;
        Ok(Arc::new(SmbDriver { rt, root, session: Arc::new(Mutex::new(session)) }))
    }
}

struct Session {
    client: smb2::SmbClient,
    tree: smb2::Tree,
}

impl Session {
    async fn open(cfg: &SmbConfig) -> Result<Self> {
        let config = smb2::ClientConfig {
            addr: format!("{}:{}", cfg.host.trim(), cfg.port),
            timeout: Duration::from_secs(cfg.timeout_secs.max(1)),
            username: cfg.username.clone(),
            password: cfg.password.clone(),
            domain: cfg.domain.clone(),
            auto_reconnect: true,
            ..Default::default()
        };
        let mut client = smb2::SmbClient::connect(config).await.map_err(map_err)?;
        let tree = client.connect_share(cfg.share.trim()).await.map_err(map_err)?;
        Ok(Self { client, tree })
    }
}

fn map_err(e: smb2::Error) -> DriverError {
    use smb2::ErrorKind as K;
    match e.kind() {
        K::NotFound => DriverError::NotFound(e.to_string()),
        K::AuthRequired | K::AccessDenied => DriverError::Auth,
        _ => DriverError::Io(std::io::Error::other(e.to_string())),
    }
}

/// Runs a future on the driver's runtime and waits for it from the calling thread.
fn run<T: Send + 'static>(rt: &tokio::runtime::Runtime, f: impl Future<Output = T> + Send + 'static) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    rt.spawn(async move {
        let _ = tx.send(f.await);
    });
    rx.recv().map_err(|_| DriverError::Io(std::io::Error::other("SMB runtime stopped")))
}

fn to_ms(t: smb2::pack::FileTime) -> Option<i64> {
    let st: SystemTime = t.to_system_time()?;
    st.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

pub struct SmbDriver {
    rt: Arc<tokio::runtime::Runtime>,
    root: Vec<String>,
    session: Arc<Mutex<Session>>,
}

impl SmbDriver {
    /// Path inside the share for a root-relative path.
    fn remote(&self, path: &str) -> Result<String> {
        let parts = checked_components(path)?;
        Ok(self.root.iter().map(String::as_str).chain(parts).collect::<Vec<_>>().join("/"))
    }
}

fn join(dir: &str, name: &str) -> String {
    let dir = dir.trim_matches('/');
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

impl MediaDriver for SmbDriver {
    fn kind(&self) -> &'static str {
        "smb"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { write: true, watch: false, cheap_seek: true }
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let remote = self.remote(dir)?;
        let session = self.session.clone();
        let entries = run(&self.rt, async move {
            let mut s = session.lock().await;
            let Session { client, tree } = &mut *s;
            client.list_directory(tree, &remote).await.map_err(map_err)
        })??;
        Ok(entries
            .into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .map(|e| Entry {
                path: join(dir, &e.name),
                is_dir: e.is_directory,
                size: if e.is_directory { 0 } else { e.size },
                created_ms: to_ms(e.created),
                modified_ms: to_ms(e.modified),
                added_ms: None,
            })
            .collect())
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        let remote = self.remote(path)?;
        let session = self.session.clone();
        let info = run(&self.rt, async move {
            let mut s = session.lock().await;
            let Session { client, tree } = &mut *s;
            client.stat(tree, &remote).await.map_err(map_err)
        })??;
        Ok(Entry {
            path: path.trim_matches('/').to_string(),
            is_dir: info.is_directory,
            size: info.size,
            created_ms: to_ms(info.created),
            modified_ms: to_ms(info.modified),
            added_ms: None,
        })
    }

    fn open(&self, path: &str) -> Result<Box<dyn MediaReader>> {
        let remote = self.remote(path)?;
        let session = self.session.clone();
        let reader = run(&self.rt, async move {
            let mut s = session.lock().await;
            let Session { client, tree } = &mut *s;
            client.open_file_reader(tree, &remote).await.map_err(map_err)
        })??;
        let len = reader.size();
        Ok(Box::new(SmbReader { rt: self.rt.clone(), reader: Some(Arc::new(reader)), len }))
    }

    fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let remote = self.remote(path)?;
        let session = self.session.clone();
        let data = data.to_vec();
        run(&self.rt, async move {
            let mut s = session.lock().await;
            let Session { client, tree } = &mut *s;
            client.write_file(tree, &remote, &data).await.map(|_| ()).map_err(map_err)
        })?
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        let (from, to) = (self.remote(from)?, self.remote(to)?);
        let session = self.session.clone();
        run(&self.rt, async move {
            let mut s = session.lock().await;
            let Session { client, tree } = &mut *s;
            client.rename(tree, &from, &to).await.map_err(map_err)
        })?
    }

    fn delete(&self, path: &str) -> Result<()> {
        let remote = self.remote(path)?;
        let session = self.session.clone();
        run(&self.rt, async move {
            let mut s = session.lock().await;
            let Session { client, tree } = &mut *s;
            client.delete_file(tree, &remote).await.map_err(map_err)
        })?
    }
}

/// Positional reads over one open SMB handle; concurrent reads proceed in parallel.
struct SmbReader {
    rt: Arc<tokio::runtime::Runtime>,
    reader: Option<Arc<smb2::FileReader>>,
    len: u64,
}

impl MediaReader for SmbReader {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let reader = self.reader.clone().expect("open reader");
        let len = buf.len() as u64;
        let data = run(&self.rt, async move { reader.read_at(offset, len).await })
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }
}

impl Drop for SmbReader {
    fn drop(&mut self) {
        // Close the remote handle in the background once no read holds it.
        if let Some(reader) = self.reader.take().and_then(|r| Arc::try_unwrap(r).ok()) {
            self.rt.spawn(async move {
                let _ = reader.close().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validation() {
        let err = SmbFactory.connect("", &serde_json::json!({ "host": "", "share": "x" }));
        assert!(matches!(err, Err(DriverError::InvalidConfig(_))));
        let err = SmbFactory.connect("", &serde_json::json!({ "host": "h" }));
        assert!(matches!(err, Err(DriverError::InvalidConfig(_))));
        let err = SmbFactory.connect("../x", &serde_json::json!({ "host": "h", "share": "s" }));
        assert!(matches!(err, Err(DriverError::PathEscapesRoot(_))));
    }

    /// Runs against a real server when `VP_TEST_SMB` is set, e.g.
    /// `VP_TEST_SMB='{"host":"nas","share":"media","username":"u","password":"p"}'`,
    /// with `VP_TEST_SMB_FILE` naming a file of at least 2 KB in the share root.
    #[test]
    fn real_server() {
        let Ok(cfg) = std::env::var("VP_TEST_SMB") else { return };
        let config: serde_json::Value = serde_json::from_str(&cfg).expect("VP_TEST_SMB is JSON");
        let driver = SmbFactory.connect("", &config).expect("connect");
        let entries = driver.list("").expect("list");
        assert!(!entries.is_empty());
        if let Ok(file) = std::env::var("VP_TEST_SMB_FILE") {
            let st = driver.stat(&file).unwrap();
            let r = driver.open(&file).unwrap();
            assert_eq!(r.len(), st.size);
            let mut a = vec![0u8; 1024];
            let mut b = vec![0u8; 1024];
            assert_eq!(r.read_full_at(1024, &mut a).unwrap(), 1024);
            // Reads are positional: a second read at the same offset returns the same bytes.
            assert_eq!(r.read_full_at(1024, &mut b).unwrap(), 1024);
            assert_eq!(a, b);
        }
    }
}
