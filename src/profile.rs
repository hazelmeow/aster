use anyhow::Context as _;
use iroh::SecretKey;

/// Persisted data.
#[derive(Debug)]
pub struct Profile {
    name: Option<String>,
    secret_key: SecretKey,
}

impl Profile {
    pub async fn load_or_create(name: &str) -> anyhow::Result<Self> {
        Ok(Self::load(name)
            .await?
            .unwrap_or_else(|| Self::new(Some(name.to_string()))))
    }

    pub fn new(name: Option<String>) -> Self {
        Self {
            name,
            secret_key: SecretKey::generate(rand::rngs::OsRng),
        }
    }

    pub async fn load(name: &str) -> anyhow::Result<Option<Self>> {
        let base_path = format!("./data/{name}");
        let secret_key_path = format!("{base_path}/secretkey.txt");

        if tokio::fs::try_exists(&secret_key_path).await? {
            let secret_key_string = tokio::fs::read_to_string(secret_key_path)
                .await
                .context("failed to read profile file")?;
            let secret_key = secret_key_string
                .parse()
                .context("failed to parse profile file")?;
            Ok(Some(Self {
                name: Some(name.to_string()),
                secret_key,
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn save(&self) -> anyhow::Result<()> {
        // only save if named
        let Some(profile_name) = &self.name else {
            return Ok(());
        };

        let base_path = format!("./data/{profile_name}");
        tokio::fs::create_dir_all(&base_path).await?;

        let secret_key_path = format!("{base_path}/secretkey.txt");
        tokio::fs::write(secret_key_path, self.secret_key.to_string()).await?;

        Ok(())
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|s| s.as_str())
    }

    pub fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }
}
