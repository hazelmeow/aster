use crate::proto::{acb::SignedMessage, clock::Clock, dgm::Operation};
use anyhow::Context as _;
use iroh::SecretKey;
use serde::{Deserialize, Serialize};

/// Persisted data.
#[derive(Debug, Serialize, Deserialize)]
pub struct Profile {
    pub name: Option<String>,
    pub secret_key: SecretKey,
    pub library_roots: Vec<String>,
    pub group: Option<ProfileGroup>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileGroup {
    pub group_id: u64,
    pub acb_clock: Clock,
    pub acb_messages: Vec<SignedMessage<Operation>>,
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
            library_roots: Vec::new(),
            group: None,
        }
    }

    pub async fn load(name: &str) -> anyhow::Result<Option<Self>> {
        let data_path = format!("./data/{name}/profile.json");

        if tokio::fs::try_exists(&data_path).await? {
            let data = tokio::fs::read_to_string(data_path)
                .await
                .context("failed to read profile file")?;
            let data = serde_json::from_str(&data).context("failed to parse profile file")?;

            Ok(Some(data))
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

        let data_path = format!("{base_path}/profile.json");
        let data = serde_json::to_vec(self).context("failed to serialize profile file")?;
        tokio::fs::write(data_path, data).await?;

        Ok(())
    }
}
