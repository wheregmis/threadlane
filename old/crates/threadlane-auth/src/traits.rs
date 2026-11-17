use async_trait::async_trait;

#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Returns the provider identifier (e.g. "openai", "codex", "antigravity").
    fn provider_id(&self) -> &'static str;

    /// Checks if valid stored credentials exist on disk or in memory.
    fn has_credentials(&self) -> bool;

    /// Fetches a valid access token, automatically refreshing if near expiration.
    async fn get_token(&self) -> Result<String, String>;

    /// Revokes or removes stored credentials from disk.
    fn clear_credentials(&self) -> Result<(), String>;
}
