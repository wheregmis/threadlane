//! Versioned, non-executable model metadata catalog and independently refreshed snapshot cache.
use crate::model_registry::ModelInfo;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const MAX_CATALOG_BYTES: usize = 1024 * 1024;
pub const CATALOG_URL: &str =
    "https://raw.githubusercontent.com/wheregmis/threadlane/main/resources/models.json";
pub const CATALOG_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub models: Vec<ModelInfo>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheFile {
    catalog: CatalogSnapshot,
    checked_at: u64,
}

pub fn parse_catalog(bytes: &[u8]) -> Result<CatalogSnapshot, String> {
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err("catalog exceeds 1 MiB".into());
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if let Some(models) = value.get("models").and_then(|v| v.as_array()) {
        for model in models {
            let fields = model.as_object().ok_or("model must be an object")?;
            if fields.keys().any(|key| {
                ![
                    "id",
                    "label",
                    "provider",
                    "context_window",
                    "supported_efforts",
                    "default_effort",
                ]
                .contains(&key.as_str())
            }) {
                return Err("unknown model field".into());
            }
        }
    }
    let catalog: CatalogSnapshot = serde_json::from_value(value).map_err(|e| e.to_string())?;
    validate_catalog(&catalog)?;
    Ok(catalog)
}

fn effort_valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

pub fn validate_catalog(c: &CatalogSnapshot) -> Result<(), String> {
    if c.schema_version != SCHEMA_VERSION || c.revision == 0 {
        return Err("unsupported schema or revision".into());
    }
    if c.models.is_empty() {
        return Err("empty catalog".into());
    }
    let mut ids = HashSet::new();
    for m in &c.models {
        if m.id.trim().is_empty()
            || m.id.trim() != m.id
            || m.id.chars().any(char::is_whitespace)
            || m.label.trim().is_empty()
            || !ids.insert(&m.id)
        {
            return Err("empty or duplicate model id/label".into());
        }
        let provider = m.provider.as_deref().ok_or("model provider is required")?;
        let prefix = match provider {
            "openai" => None,
            "antigravity" => Some("antigravity/"),
            "opencode" => Some("opencode-go/"),
            "anthropic" => Some("anthropic/"),
            _ => return Err(format!("unsupported provider: {provider}")),
        };
        if prefix.is_some_and(|p| !m.id.starts_with(p) || m.id.len() == p.len())
            || (provider == "openai"
                && (m.id.starts_with("antigravity/")
                    || m.id.starts_with("opencode-go/")
                    || m.id.starts_with("anthropic/")
                    || m.id.starts_with("acp/")))
        {
            return Err(format!("provider/prefix mismatch: {}", m.id));
        }
        if m.context_window.is_some_and(|n| n == 0 || n > 10_000_000) {
            return Err(format!("invalid context window: {}", m.id));
        }
        if m.supported_efforts.iter().any(|e| !effort_valid(e))
            || m.supported_efforts.iter().collect::<HashSet<_>>().len() != m.supported_efforts.len()
        {
            return Err(format!("invalid or duplicate effort: {}", m.id));
        }
        if let Some(default) = &m.default_effort {
            if !effort_valid(default)
                || (!m.supported_efforts.is_empty() && !m.supported_efforts.contains(default))
            {
                return Err(format!("default effort not supported: {}", m.id));
            }
        }
    }
    Ok(())
}

pub fn snapshot_path() -> Option<PathBuf> {
    threadlane_project::default_global_threadlane_dir()
        .map(|p| p.join("cache").join("model-catalog.json"))
}

fn read_cache(path: &std::path::Path) -> Option<CacheFile> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take((MAX_CATALOG_BYTES + 257) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_CATALOG_BYTES + 256 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    parse_catalog(&serde_json::to_vec(value.get("catalog")?).ok()?).ok()?;
    let cache: CacheFile = serde_json::from_value(value).ok()?;
    Some(cache)
}

pub fn best_snapshot(bundle: &CatalogSnapshot, path: Option<&std::path::Path>) -> CatalogSnapshot {
    let Some(cached) = path.and_then(read_cache) else {
        return bundle.clone();
    };
    if cached.catalog.revision < bundle.revision {
        return bundle.clone();
    }
    if cached.catalog.revision == bundle.revision && cached.catalog != *bundle {
        return bundle.clone();
    }
    cached.catalog
}

fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("cache path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let name = format!(
        ".catalog-{}-{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let temp = parent.join(name);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static LAST_ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Fetch fixed trusted HTTPS resource, without redirects or credentials. Errors never mutate last-good cache.
pub async fn refresh_catalog(force: bool) -> Result<bool, String> {
    crate::exec::try_get_runtime()?
        .spawn(refresh_catalog_network(force))
        .await
        .map_err(|error| error.to_string())?
}

async fn refresh_catalog_network(force: bool) -> Result<bool, String> {
    let _guard = REFRESH_LOCK.lock().await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let previous = LAST_ATTEMPT.load(std::sync::atomic::Ordering::Relaxed);
    let path = snapshot_path().ok_or("home directory unavailable")?;
    let current = read_cache(&path);
    if !refresh_due(force, now, previous, current.as_ref().map(|c| c.checked_at)) {
        return Ok(false);
    }
    LAST_ATTEMPT.store(now, std::sync::atomic::Ordering::Relaxed);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let bytes = fetch_catalog_bytes(&client, CATALOG_URL).await?;
    let bundle = parse_catalog(include_bytes!("../../../resources/models.json"))?;
    let next = accept_update(&bytes, &bundle, current.as_ref().map(|c| &c.catalog))?;
    let data = serde_json::to_vec(&CacheFile {
        catalog: next,
        checked_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    })
    .map_err(|e| e.to_string())?;
    atomic_write(&path, &data)?;
    Ok(true)
}

async fn fetch_catalog_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("catalog HTTP status {}", response.status()));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if bytes.len().saturating_add(chunk.len()) > MAX_CATALOG_BYTES {
            return Err("catalog exceeds 1 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn refresh_due(force: bool, now: u64, attempted: u64, checked: Option<u64>) -> bool {
    force
        || !((attempted != 0 && now.saturating_sub(attempted) < 300)
            || checked
                .is_some_and(|checked| checked <= now && now - checked < CATALOG_TTL.as_secs()))
}

fn accept_update(
    bytes: &[u8],
    bundle: &CatalogSnapshot,
    current: Option<&CatalogSnapshot>,
) -> Result<CatalogSnapshot, String> {
    let next = parse_catalog(bytes)?;
    for old in std::iter::once(bundle).chain(current) {
        if next.revision < old.revision {
            return Err("catalog revision rollback".into());
        }
        if next.revision == old.revision && next != *old {
            return Err("catalog changed without revision bump".into());
        }
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_fields_and_bad_entries() {
        let good = br#"{"schema_version":1,"revision":1,"models":[{"id":"gpt-x","label":"GPT X","provider":"openai"}]}"#;
        assert!(parse_catalog(good).is_ok());
        assert!(parse_catalog(&[good.as_slice(), br#" "extra":true"#].concat()).is_err());
        let bad = br#"{"schema_version":1,"revision":1,"models":[{"id":"antigravity/x","label":"X","provider":"openai"}]}"#;
        assert!(parse_catalog(bad).is_err());
    }
    #[test]
    fn bundled_snapshot_is_strict_valid_and_full() {
        let s = parse_catalog(include_bytes!("../../../resources/models.json")).unwrap();
        assert!(s.models.len() >= 40);
    }
    #[test]
    fn same_revision_changed_snapshot_rejected_as_cache_choice() {
        let bundle = parse_catalog(include_bytes!("../../../resources/models.json")).unwrap();
        let path = tempfile::NamedTempFile::new().unwrap();
        let mut altered = bundle.clone();
        altered.models[0].label.push('!');
        std::fs::write(
            path.path(),
            serde_json::to_vec(&CacheFile {
                catalog: altered,
                checked_at: 0,
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(best_snapshot(&bundle, Some(path.path())), bundle);
    }
    #[test]
    fn invalid_catalogs_are_rejected() {
        let good = serde_json::json!({"schema_version":1,"revision":1,"models":[{"id":"gpt-test","label":"Test","provider":"openai"}]});
        for bad in [
            serde_json::json!({"schema_version":2,"revision":1,"models":good["models"]}),
            serde_json::json!({"schema_version":1,"revision":0,"models":good["models"]}),
            serde_json::json!({"schema_version":1,"revision":1,"models":[]}),
        ] {
            assert!(parse_catalog(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        for (field, value) in [
            ("endpoint", serde_json::json!("https://evil.test")),
            ("supported_efforts", serde_json::json!(["invalid effort"])),
            ("context_window", serde_json::json!(0)),
            ("id", serde_json::json!("anthropic/")),
        ] {
            let mut bad = good.clone();
            bad["models"][0][field] = value;
            assert!(parse_catalog(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        let mut duplicate = good.clone();
        duplicate["models"]
            .as_array_mut()
            .unwrap()
            .push(good["models"][0].clone());
        assert!(parse_catalog(&serde_json::to_vec(&duplicate).unwrap()).is_err());
        assert!(parse_catalog(&vec![b' '; MAX_CATALOG_BYTES + 1]).is_err());
    }

    #[test]
    fn update_revisions_and_atomic_cache_preserve_last_good() {
        let bundle = parse_catalog(include_bytes!("../../../resources/models.json")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        assert_eq!(best_snapshot(&bundle, Some(&path)), bundle);
        std::fs::write(&path, b"broken").unwrap();
        assert_eq!(best_snapshot(&bundle, Some(&path)), bundle);
        let mut next = bundle.clone();
        next.revision += 1;
        next.models.remove(0);
        let bytes = serde_json::to_vec(&next).unwrap();
        assert_eq!(accept_update(&bytes, &bundle, None).unwrap(), next);
        let cache = serde_json::to_vec(&CacheFile {
            catalog: next.clone(),
            checked_at: 123,
        })
        .unwrap();
        atomic_write(&path, &cache).unwrap();
        assert_eq!(best_snapshot(&bundle, Some(&path)), next);
        assert!(
            accept_update(&serde_json::to_vec(&bundle).unwrap(), &bundle, Some(&next)).is_err()
        );
        let mut changed = bundle.clone();
        changed.models[0].label.push('!');
        assert!(accept_update(&serde_json::to_vec(&changed).unwrap(), &bundle, None).is_err());
        let mut changed = next.clone();
        changed.models[0].label.push('!');
        assert!(
            accept_update(&serde_json::to_vec(&changed).unwrap(), &bundle, Some(&next)).is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), cache);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn refresh_ttl_force_and_failure_backoff() {
        assert!(refresh_due(false, 100_000, 0, None));
        assert!(!refresh_due(false, 100_000, 99_999, None));
        assert!(!refresh_due(false, 100_000, 0, Some(99_999)));
        assert!(refresh_due(true, 100_000, 99_999, Some(99_999)));
        assert!(refresh_due(false, 100_000, 0, Some(1)));
        assert!(refresh_due(false, 100_000, 0, Some(100_001)));
    }
    #[tokio::test]
    async fn http_download_is_bounded_and_does_not_follow_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        async fn serve(status: &str, body: Vec<u8>) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let header = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1\r\nConnection: close\r\n\r\n", body.len());
            tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let n = socket.read(&mut request).await.unwrap();
                assert!(!String::from_utf8_lossy(&request[..n])
                    .to_lowercase()
                    .contains("authorization:"));
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
            url
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let url = serve("200 OK", b"catalog".to_vec()).await;
        assert_eq!(
            fetch_catalog_bytes(&client, &url).await.unwrap(),
            b"catalog"
        );
        let url = serve("302 Found", vec![]).await;
        assert!(fetch_catalog_bytes(&client, &url)
            .await
            .unwrap_err()
            .contains("302"));
        let url = serve("500 Error", vec![]).await;
        assert!(fetch_catalog_bytes(&client, &url)
            .await
            .unwrap_err()
            .contains("500"));
        let url = serve("200 OK", vec![b' '; MAX_CATALOG_BYTES + 1]).await;
        assert!(fetch_catalog_bytes(&client, &url)
            .await
            .unwrap_err()
            .contains("1 MiB"));
    }
    #[test]
    fn data_driven_efforts_and_repeated_atomic_replacement() {
        assert!(effort_valid("ultra"));
        assert!(effort_valid("none"));
        assert!(!effort_valid(""));
        assert!(!effort_valid(" high"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"second");
    }
}
