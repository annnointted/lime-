pub mod config;
pub mod server;
pub mod database;
pub mod auth;
pub mod tasks;
pub mod api;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use config::Config;
use database::DatabasePool;
use server::{Router, Server};
use auth::AuthContext;
use tasks::TaskRunner;

pub struct Lime {
    config: Config,
    router: Router,
    db_pool: Option<DatabasePool>,
    auth: Option<AuthContext>,
    task_runner: Option<TaskRunner>,
}

impl Lime {
    pub fn new() -> Self {
        Self {
            config: Config::default(),
            router: Router::new(),
            db_pool: None,
            auth: None,
            task_runner: None,
        }
    }

    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    pub fn config_from_file(mut self, path: &str) -> Result<Self> {
        self.config = Config::from_file(path)?;
        Ok(self)
    }

    pub fn config_from_env(mut self) -> Result<Self> {
        self.config = Config::from_env()?;
        Ok(self)
    }

    pub async fn with_db_pool(mut self) -> Result<Self> {
        let pool = DatabasePool::new(&self.config).await?;
        self.db_pool = Some(pool);
        Ok(self)
    }

    pub fn with_auth(mut self, jwt_secret: &str) -> Result<Self> {
        let auth = AuthContext::new(jwt_secret, self.db_pool.clone())?;
        self.auth = Some(auth);
        Ok(self)
    }

    pub async fn with_task_runner(mut self) -> Result<Self> {
        let task_runner = TaskRunner::new(&self.config).await?;
        self.task_runner = Some(task_runner);
        Ok(self)
    }

    pub fn get<H, F>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(api::Request) -> F + Send + Sync + 'static,
        F: Future<Output = api::Response> + Send + 'static,
    {
        self.router.get(path, handler);
        self
    }

    pub fn post<H, F>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(api::Request) -> F + Send + Sync + 'static,
        F: Future<Output = api::Response> + Send + 'static,
    {
        self.router.post(path, handler);
        self
    }

    pub fn put<H, F>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(api::Request) -> F + Send + Sync + 'static,
        F: Future<Output = api::Response> + Send + 'static,
    {
        self.router.put(path, handler);
        self
    }

    pub fn delete<H, F>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(api::Request) -> F + Send + Sync + 'static,
        F: Future<Output = api::Response> + Send + 'static,
    {
        self.router.delete(path, handler);
        self
    }

    pub fn with_middleware<M>(mut self, middleware: M) -> Self
    where
        M: server::Middleware + Send + Sync + 'static,
    {
        self.router.add_middleware(Arc::new(middleware));
        self
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn db_pool(&self) -> Option<&DatabasePool> {
        self.db_pool.as_ref()
    }

    pub fn auth(&self) -> Option<&AuthContext> {
        self.auth.as_ref()
    }

    pub fn task_runner(&self) -> Option<&TaskRunner> {
        self.task_runner.as_ref()
    }

    pub async fn run(self) -> Result<()> {
        let server = Server::new(
            self.config.server.clone(),
            self.router,
            self.db_pool,
            self.auth,
            self.task_runner,
        );

        server.run().await
    }
}

impl Default for Lime {
    fn default() -> Self {
        Self::new()
    }
}

// Builder pattern for more ergonomic setup
pub struct LimeBuilder {
    config: Config,
}

impl LimeBuilder {
    pub fn new() -> Self {
        Self {
            config: Config::default(),
        }
    }

    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    pub fn config_from_file(mut self, path: &str) -> Result<Self> {
        self.config = Config::from_file(path)?;
        Ok(self)
    }

    pub fn config_from_env(mut self) -> Result<Self> {
        self.config = Config::from_env()?;
        Ok(self)
    }

    pub async fn build(self) -> Result<Lime> {
        let mut lime = Lime::new().with_config(self.config);
        
        // Initialize database if host is configured (non-empty)
        // Check if database connection details are provided
        if !lime.config.database.host.is_empty() {
            lime = lime.with_db_pool().await?;
        }

        // Initialize auth if JWT secret is available in auth config
        let jwt_secret = lime.config.auth.jwt_secret.clone();
        if !jwt_secret.is_empty() {
            lime = lime.with_auth(&jwt_secret)?;
        }

        // Initialize task runner if Redis is configured
        if lime.config.redis.is_some() {
            lime = lime.with_task_runner().await?;
        }

        Ok(lime)
    }
}

impl Default for LimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// Database model macro (simplified version from database module)
#[macro_export]
macro_rules! db_model {
    ($struct:ident { $($field:ident: $type:ty),* $(,)? }) => {
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
        pub struct $struct {
            pub id: uuid::Uuid,
            pub created_at: chrono::DateTime<chrono::Utc>,
            pub updated_at: chrono::DateTime<chrono::Utc>,
            $(pub $field: $type,)*
        }

        impl $struct {
            pub fn new($($field: $type),*) -> Self {
                let now = chrono::Utc::now();
                Self {
                    id: uuid::Uuid::new_v4(),
                    created_at: now,
                    updated_at: now,
                    $($field,)*
                }
            }

            pub async fn find_by_id(pool: &sqlx::PgPool, id: uuid::Uuid) -> anyhow::Result<Option<Self>> {
                let table_name = Self::table_name();
                let query_str = format!("SELECT * FROM {} WHERE id = $1", table_name);
                Ok(sqlx::query_as::<_, Self>(&query_str)
                    .bind(id)
                    .fetch_optional(pool)
                    .await?)
            }

            pub async fn find_all(pool: &sqlx::PgPool) -> anyhow::Result<Vec<Self>> {
                let table_name = Self::table_name();
                let query_str = format!("SELECT * FROM {} ORDER BY created_at DESC", table_name);
                Ok(sqlx::query_as::<_, Self>(&query_str)
                    .fetch_all(pool)
                    .await?)
            }

            pub async fn create(&self, pool: &sqlx::PgPool) -> anyhow::Result<Self> {
                let table_name = Self::table_name();
                let mut fields = vec!["id", "created_at", "updated_at"];
                $(fields.push(stringify!($field));)*
                
                let placeholders: Vec<String> = (1..=fields.len())
                    .map(|i| format!("${}", i))
                    .collect();
                
                let query_str = format!(
                    "INSERT INTO {} ({}) VALUES ({}) RETURNING *",
                    table_name,
                    fields.join(", "),
                    placeholders.join(", ")
                );
                
                let mut query_builder = sqlx::query_as::<_, Self>(&query_str)
                    .bind(&self.id)
                    .bind(&self.created_at)
                    .bind(&self.updated_at);
                
                $(query_builder = query_builder.bind(&self.$field);)*
                
                Ok(query_builder.fetch_one(pool).await?)
            }

            pub async fn update(&mut self, pool: &sqlx::PgPool) -> anyhow::Result<Self> {
                self.updated_at = chrono::Utc::now();
                
                let table_name = Self::table_name();
                let mut set_clauses = vec!["updated_at = $1".to_string()];
                let mut param_index = 2;
                
                $(
                    set_clauses.push(format!("{} = ${}", stringify!($field), param_index));
                    param_index += 1;
                )*
                
                let query_str = format!(
                    "UPDATE {} SET {} WHERE id = ${} RETURNING *",
                    table_name,
                    set_clauses.join(", "),
                    param_index
                );
                
                let mut query_builder = sqlx::query_as::<_, Self>(&query_str)
                    .bind(&self.updated_at);
                
                $(query_builder = query_builder.bind(&self.$field);)*
                query_builder = query_builder.bind(&self.id);
                
                Ok(query_builder.fetch_one(pool).await?)
            }

            pub async fn delete(pool: &sqlx::PgPool, id: uuid::Uuid) -> anyhow::Result<bool> {
                let table_name = Self::table_name();
                let query_str = format!("DELETE FROM {} WHERE id = $1", table_name);
                let result = sqlx::query(&query_str)
                    .bind(id)
                    .execute(pool)
                    .await?;
                
                Ok(result.rows_affected() > 0)
            }

            pub async fn paginate(
                pool: &sqlx::PgPool,
                page: u32,
                per_page: u32
            ) -> anyhow::Result<(Vec<Self>, u64)> {
                let table_name = Self::table_name();
                let offset = ((page - 1) * per_page) as i64;
                let limit = per_page as i64;
                
                let query_str = format!(
                    "SELECT * FROM {} ORDER BY created_at DESC LIMIT $1 OFFSET $2",
                    table_name
                );
                let items = sqlx::query_as::<_, Self>(&query_str)
                    .bind(limit)
                    .bind(offset)
                    .fetch_all(pool)
                    .await?;
                
                let count_query = format!("SELECT COUNT(*) FROM {}", table_name);
                let total = sqlx::query_scalar::<_, i64>(&count_query)
                    .fetch_one(pool)
                    .await? as u64;
                
                Ok((items, total))
            }

            fn table_name() -> String {
                let name = stringify!($struct);
                Self::to_snake_case(name)
            }

            fn to_snake_case(s: &str) -> String {
                let mut result = String::new();
                for (i, ch) in s.chars().enumerate() {
                    if ch.is_uppercase() {
                        if i > 0 {
                            result.push('_');
                        }
                        result.push(ch.to_lowercase().next().unwrap());
                    } else {
                        result.push(ch);
                    }
                }
                result
            }
        }
    };
}

// Re-export commonly used types
pub mod prelude {
    pub use crate::api::{Request, Response, RequestExt, Pagination, PaginatedResponse};
    pub use crate::auth::{Claims, User, AuthContext};
    pub use crate::config::Config;
    pub use crate::database::{DatabasePool, Migration, MigrationRunner};
    pub use crate::server::{Router, Server, Middleware, MiddlewareResult};
    pub use crate::tasks::{Task, TaskRunner, EmailTask};
    pub use crate::Lime;
    pub use crate::db_model;
    
    // Common external crates
    pub use anyhow::{Result, Context};
    pub use hyper::StatusCode;
    pub use serde::{Deserialize, Serialize};
    pub use uuid::Uuid;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lime_creation() {
        let lime = Lime::new();
        assert!(lime.db_pool.is_none());
        assert!(lime.auth.is_none());
        assert!(lime.task_runner.is_none());
    }

    #[test]
    fn test_builder_pattern() {
        let builder = LimeBuilder::new();
        assert!(builder.config.server.host == "127.0.0.1");
    }
}
