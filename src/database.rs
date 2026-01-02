use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use sqlx::{PgPool, postgres::PgPoolOptions};
use tracing::{info, error};

use crate::config::{Config, DatabaseConfig};

#[derive(Clone)]
pub struct DatabasePool {
    inner: Arc<PgPool>,
}

impl DatabasePool {
    pub async fn new(config: &Config) -> Result<Self> {
        let pool = Self::create_pool(&config.database).await?;
        Ok(Self {
            inner: Arc::new(pool),
        })
    }

    async fn create_pool(config: &DatabaseConfig) -> Result<PgPool> {
        let url = format!(
            "postgres://{}:{}@{}:{}/{}",
            config.username, config.password, config.host, config.port, config.database
        );

        info!("Connecting to database at {}:{}/{}", config.host, config.port, config.database);

        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(Duration::from_secs(config.connect_timeout))
            .idle_timeout(Duration::from_secs(600)) // 10 minutes
            .max_lifetime(Duration::from_secs(1800)) // 30 minutes
            .connect(&url)
            .await?;

        info!("Database connection pool created successfully");
        Ok(pool)
    }

    pub async fn health_check(&self) -> bool {
        match sqlx::query("SELECT 1").execute(&*self.inner).await {
            Ok(_) => true,
            Err(e) => {
                error!("Database health check failed: {}", e);
                false
            }
        }
    }

    pub async fn run_migrations(&self) -> Result<()> {
        info!("Running database migrations...");
        
        // Create migrations table if it doesn't exist
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS _lime_migrations (
                id SERIAL PRIMARY KEY,
                version VARCHAR(255) NOT NULL UNIQUE,
                name VARCHAR(255) NOT NULL,
                applied_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
            )
            "#
        )
        .execute(&*self.inner)
        .await?;

        info!("Migrations table ready");
        Ok(())
    }

    pub fn get(&self) -> &PgPool {
        &self.inner
    }

    pub async fn transaction(&self) -> Result<sqlx::Transaction<'_, sqlx::Postgres>> {
        Ok(self.inner.begin().await?)
    }

    pub async fn close(&self) {
        self.inner.close().await;
        info!("Database connection pool closed");
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

pub struct Migration {
    version: String,
    name: String,
    sql: String,
}

impl Migration {
    pub fn new(version: &str, name: &str, sql: &str) -> Self {
        Self {
            version: version.to_string(),
            name: name.to_string(),
            sql: sql.to_string(),
        }
    }
}

pub struct MigrationRunner {
    migrations: Vec<Migration>,
}

impl MigrationRunner {
    pub fn new() -> Self {
        Self {
            migrations: Vec::new(),
        }
    }

    pub fn add_migration(&mut self, version: &str, name: &str, sql: &str) -> &mut Self {
        self.migrations.push(Migration {
            version: version.to_string(),
            name: name.to_string(),
            sql: sql.to_string(),
        });
        self
    }

    pub fn add(&mut self, migration: Migration) -> &mut Self {
        self.migrations.push(migration);
        self
    }

    pub async fn run(&self, pool: &DatabasePool) -> Result<()> {
        // Ensure migrations table exists
        pool.run_migrations().await?;

        if self.migrations.is_empty() {
            info!("No migrations to run");
            return Ok(());
        }

        for migration in &self.migrations {
            // Check if migration already applied
            let applied = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM _lime_migrations WHERE version = $1)"
            )
            .bind(&migration.version)
            .fetch_one(pool.get())
            .await?;

            if applied {
                info!("Migration {} already applied, skipping", migration.version);
                continue;
            }

            info!("Applying migration: {} - {}", migration.version, migration.name);
            
            let mut transaction = pool.transaction().await?;
            
            // Run migration SQL
            sqlx::query(&migration.sql)
                .execute(&mut *transaction)
                .await
                .map_err(|e| {
                    error!("Failed to apply migration {}: {}", migration.name, e);
                    e
                })?;
            
            // Record migration
            sqlx::query(
                "INSERT INTO _lime_migrations (version, name) VALUES ($1, $2)"
            )
            .bind(&migration.version)
            .bind(&migration.name)
            .execute(&mut *transaction)
            .await?;
            
            transaction.commit().await?;
            
            info!("✓ Migration applied successfully: {} - {}", migration.version, migration.name);
        }
        
        info!("All migrations completed successfully");
        Ok(())
    }

    pub async fn rollback_last(&self, pool: &DatabasePool) -> Result<()> {
        // Get the last applied migration
        let last_migration = sqlx::query_as::<_, (String, String)>(
            "SELECT version, name FROM _lime_migrations ORDER BY applied_at DESC LIMIT 1"
        )
        .fetch_optional(pool.get())
        .await?;

        if let Some((version, name)) = last_migration {
            info!("Rolling back migration: {} - {}", version, name);
            
            // Remove from migrations table
            sqlx::query("DELETE FROM _lime_migrations WHERE version = $1")
                .bind(&version)
                .execute(pool.get())
                .await?;
            
            info!("✓ Migration rolled back: {}", name);
        } else {
            info!("No migrations to roll back");
        }

        Ok(())
    }

    pub async fn list_applied(&self, pool: &DatabasePool) -> Result<Vec<(String, String, chrono::DateTime<chrono::Utc>)>> {
        let migrations = sqlx::query_as::<_, (String, String, chrono::DateTime<chrono::Utc>)>(
            "SELECT version, name, applied_at FROM _lime_migrations ORDER BY applied_at ASC"
        )
        .fetch_all(pool.get())
        .await?;

        Ok(migrations)
    }
}

impl Default for MigrationRunner {
    fn default() -> Self {
        Self::new()
    }
}

// Query builder helper
pub struct QueryBuilder {
    table: String,
    wheres: Vec<String>,
    params: Vec<String>,
    order_by: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl QueryBuilder {
    pub fn new(table: &str) -> Self {
        Self {
            table: table.to_string(),
            wheres: Vec::new(),
            params: Vec::new(),
            order_by: None,
            limit: None,
            offset: None,
        }
    }

    pub fn where_eq(&mut self, column: &str, value: &str) -> &mut Self {
        self.params.push(value.to_string());
        self.wheres.push(format!("{} = ${}", column, self.params.len()));
        self
    }

    pub fn order_by(&mut self, column: &str, direction: &str) -> &mut Self {
        self.order_by = Some(format!("{} {}", column, direction));
        self
    }

    pub fn limit(&mut self, limit: i64) -> &mut Self {
        self.limit = Some(limit);
        self
    }

    pub fn offset(&mut self, offset: i64) -> &mut Self {
        self.offset = Some(offset);
        self
    }

    pub fn build_select(&self) -> String {
        let mut query = format!("SELECT * FROM {}", self.table);
        
        if !self.wheres.is_empty() {
            query.push_str(&format!(" WHERE {}", self.wheres.join(" AND ")));
        }
        
        if let Some(ref order) = self.order_by {
            query.push_str(&format!(" ORDER BY {}", order));
        }
        
        if let Some(limit) = self.limit {
            query.push_str(&format!(" LIMIT {}", limit));
        }
        
        if let Some(offset) = self.offset {
            query.push_str(&format!(" OFFSET {}", offset));
        }
        
        query
    }
}

// Database model macros
#[macro_export]
macro_rules! create_model {
    ($name:ident { $($field:ident: $type:ty),* $(,)? }) => {
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
        pub struct $name {
            pub id: uuid::Uuid,
            pub created_at: chrono::DateTime<chrono::Utc>,
            pub updated_at: chrono::DateTime<chrono::Utc>,
            $(pub $field: $type,)*
        }

        impl $name {
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
                let table_name = stringify!($name).to_lowercase();
                let table_name = Self::to_snake_case(&table_name);
                Ok(sqlx::query_as::<_, Self>(
                    &format!("SELECT * FROM {} WHERE id = $1", table_name)
                )
                .bind(id)
                .fetch_optional(pool)
                .await?)
            }

            pub async fn find_all(pool: &sqlx::PgPool) -> anyhow::Result<Vec<Self>> {
                let table_name = stringify!($name).to_lowercase();
                let table_name = Self::to_snake_case(&table_name);
                Ok(sqlx::query_as::<_, Self>(
                    &format!("SELECT * FROM {} ORDER BY created_at DESC", table_name)
                )
                .fetch_all(pool)
                .await?)
            }

            pub async fn create(&self, pool: &sqlx::PgPool) -> anyhow::Result<Self> {
                let table_name = stringify!($name).to_lowercase();
                let table_name = Self::to_snake_case(&table_name);
                
                // Build field names and placeholders
                let mut fields = vec!["id", "created_at", "updated_at"];
                $(fields.push(stringify!($field));)*
                
                let placeholders: Vec<String> = (1..=fields.len())
                    .map(|i| format!("${}", i))
                    .collect();
                
                let query = format!(
                    "INSERT INTO {} ({}) VALUES ({}) RETURNING *",
                    table_name,
                    fields.join(", "),
                    placeholders.join(", ")
                );
                
                let mut query_builder = sqlx::query_as::<_, Self>(&query)
                    .bind(&self.id)
                    .bind(&self.created_at)
                    .bind(&self.updated_at);
                
                $(query_builder = query_builder.bind(&self.$field);)*
                
                Ok(query_builder.fetch_one(pool).await?)
            }

            pub async fn update(&mut self, pool: &sqlx::PgPool) -> anyhow::Result<Self> {
                self.updated_at = chrono::Utc::now();
                
                let table_name = stringify!($name).to_lowercase();
                let table_name = Self::to_snake_case(&table_name);
                
                let mut set_clauses = vec!["updated_at = $1"];
                let mut param_index = 2;
                
                $(
                    set_clauses.push(&format!("{} = ${}", stringify!($field), param_index));
                    param_index += 1;
                )*
                
                let query = format!(
                    "UPDATE {} SET {} WHERE id = ${} RETURNING *",
                    table_name,
                    set_clauses.join(", "),
                    param_index
                );
                
                let mut query_builder = sqlx::query_as::<_, Self>(&query)
                    .bind(&self.updated_at);
                
                $(query_builder = query_builder.bind(&self.$field);)*
                query_builder = query_builder.bind(&self.id);
                
                Ok(query_builder.fetch_one(pool).await?)
            }

            pub async fn delete(pool: &sqlx::PgPool, id: uuid::Uuid) -> anyhow::Result<bool> {
                let table_name = stringify!($name).to_lowercase();
                let table_name = Self::to_snake_case(&table_name);
                let result = sqlx::query(&format!("DELETE FROM {} WHERE id = $1", table_name))
                    .bind(id)
                    .execute(pool)
                    .await?;
                
                Ok(result.rows_affected() > 0)
            }

            pub async fn count(pool: &sqlx::PgPool) -> anyhow::Result<i64> {
                let table_name = stringify!($name).to_lowercase();
                let table_name = Self::to_snake_case(&table_name);
                Ok(sqlx::query_scalar::<_, i64>(
                    &format!("SELECT COUNT(*) FROM {}", table_name)
                )
                .fetch_one(pool)
                .await?)
            }

            pub async fn paginate(
                pool: &sqlx::PgPool,
                page: u32,
                per_page: u32
            ) -> anyhow::Result<(Vec<Self>, u64)> {
                let table_name = stringify!($name).to_lowercase();
                let table_name = Self::to_snake_case(&table_name);
                
                let offset = ((page - 1) * per_page) as i64;
                let limit = per_page as i64;
                
                let items = sqlx::query_as::<_, Self>(
                    &format!(
                        "SELECT * FROM {} ORDER BY created_at DESC LIMIT $1 OFFSET $2",
                        table_name
                    )
                )
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await?;
                
                let total = sqlx::query_scalar::<_, i64>(
                    &format!("SELECT COUNT(*) FROM {}", table_name)
                )
                .fetch_one(pool)
                .await? as u64;
                
                Ok((items, total))
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

#[cfg(test)]
mod tests {
    use super::*;

    // This would create a Post model with title and content fields
    // create_model!(Post {
    //     title: String,
    //     content: String,
    // });
}
