use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use config::{Config as ConfigBuilder, ConfigError, File, FileFormat};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub workers: usize,
    pub max_body_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub jwt_secret: String,
    pub token_expiry: i64,
    pub refresh_token_expiry: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    pub url: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub name: String,
    pub environment: String,
    pub log_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub redis: Option<RedisConfig>,
    pub app: AppConfig,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 3000,
                workers: 1,
                max_body_size: 1024 * 1024 * 10, // 10MB
            },
            database: DatabaseConfig {
                host: "localhost".to_string(),
                port: 5432,
                username: "postgres".to_string(),
                password: "postgres".to_string(),
                database: "app".to_string(),
                max_connections: 10,
                min_connections: 2,
                connect_timeout: 10,
            },
            auth: AuthConfig {
                jwt_secret: "super-secret-key-change-in-production".to_string(),
                token_expiry: 3600,
                refresh_token_expiry: 86400 * 7,
            },
            redis: None,
            app: AppConfig {
                name: "lime-app".to_string(),
                environment: "development".to_string(),
                log_level: "info".to_string(),
            },
            extra: HashMap::new(),
        }
    }
}

impl Config {
    pub fn from_file(path: &str) -> Result<Self, ConfigError> {
        let mut builder = ConfigBuilder::builder()
            .add_source(File::with_name(path))
            .add_source(
                config::Environment::with_prefix("LIME")
                    .separator("__")
                    .ignore_empty(true),
            );

        // Add environment-specific config
        if let Ok(env) = std::env::var("LIME_ENV") {
            let env_path = path.replace(".toml", &format!(".{}.toml", env));
            builder = builder.add_source(File::with_name(&env_path).required(false));
        }

        let config = builder.build()?;

        Ok(config.try_deserialize()?)
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        ConfigBuilder::builder()
            .add_source(config::Environment::with_prefix("LIME").separator("__"))
            .build()?
            .try_deserialize()
    }

    pub fn database_url(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            self.database.username,
            self.database.password,
            self.database.host,
            self.database.port,
            self.database.database
        )
    }

    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.extra.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn set<T: Serialize>(&mut self, key: &str, value: T) {
        if let Ok(v) = serde_json::to_value(value) {
            self.extra.insert(key.to_string(), v);
        }
    }
}

pub struct ConfigWatcher {
    config: Arc<RwLock<Config>>,
    watchers: Arc<RwLock<HashMap<String, Vec<watch::Sender<Config>>>>>,
}

impl ConfigWatcher {
    pub fn new(initial: Config) -> Self {
        Self {
            config: Arc::new(RwLock::new(initial)),
            watchers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn watch(&self) -> watch::Receiver<Config> {
        let config = self.config.read().unwrap().clone();
        let (tx, rx) = watch::channel(config);
        
        let key = "global".to_string();
        let mut watchers = self.watchers.write().unwrap();
        watchers.entry(key).or_default().push(tx);
        
        rx
    }

    pub fn update(&self, new_config: Config) {
        {
            let mut config = self.config.write().unwrap();
            *config = new_config.clone();
        }

        let watchers = self.watchers.read().unwrap();
        for sender in watchers.get("global").unwrap_or(&vec![]) {
            let _ = sender.send(new_config.clone());
        }
    }

    pub async fn start_hot_reload(&self, path: &str) -> tokio::task::JoinHandle<()> {
        let path = path.to_string();
        let watcher = self.clone();
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            
            loop {
                interval.tick().await;
                
                match Config::from_file(&path) {
                    Ok(new_config) => {
                        info!("Config reloaded from {}", path);
                        watcher.update(new_config);
                    }
                    Err(e) => {
                        warn!("Failed to reload config: {}", e);
                    }
                }
            }
        })
    }
}

impl Clone for ConfigWatcher {
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            watchers: Arc::clone(&self.watchers),
        }
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[test]
    fn test_config_from_file() {
        let toml_content = r#"
[server]
host = "127.0.0.1"
port = 8080
workers = 4
max_body_size = 5242880

[database]
host = "localhost"
port = 5432
username = "test_user"
password = "test_pass"
database = "test_db"
max_connections = 5
min_connections = 1
connect_timeout = 5

[auth]
jwt_secret = "test-secret"
token_expiry = 1800
refresh_token_expiry = 86400

[app]
name = "test-app"
environment = "test"
log_level = "debug"
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(toml_content.as_bytes()).unwrap();
        
        let config = Config::from_file(file.path().to_str().unwrap()).unwrap();
        
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.database.username, "test_user");
        assert_eq!(config.database.database, "test_db");
        assert_eq!(config.auth.jwt_secret, "test-secret");
        assert_eq!(config.app.name, "test-app");
    }

    #[test]
    fn test_config_env_override() {
        std::env::set_var("LIME__SERVER__PORT", "9090");
        std::env::set_var("LIME__DATABASE__HOST", "db.example.com");
        
        let config = Config::from_env().unwrap();
        
        assert_eq!(config.server.port, 9090);
        assert_eq!(config.database.host, "db.example.com");
        
        // Clean up
        std::env::remove_var("LIME__SERVER__PORT");
        std::env::remove_var("LIME__DATABASE__HOST");
    }

    #[test]
    fn test_config_defaults() {
        let config = Config::default();
        
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 3000);
        assert_eq!(config.database.host, "localhost");
        assert_eq!(config.auth.jwt_secret, "super-secret-key-change-in-production");
    }
}