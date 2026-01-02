use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
    Argon2,
};
use sqlx::FromRow;

use crate::database::DatabasePool;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: Uuid, // user id
    pub email: Option<String>,
    pub role: String,
    pub exp: i64,
    pub iat: i64,
}

#[derive(Clone)]
pub struct AuthContext {
    secret: String,
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    db_pool: Option<DatabasePool>,
}

impl AuthContext {
    pub fn new(secret: &str, db_pool: Option<DatabasePool>) -> Result<Self> {
        Ok(Self {
            secret: secret.to_string(),
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            db_pool,
        })
    }

    pub fn create_token(&self, user_id: Uuid, email: Option<String>, role: &str, expiry_seconds: i64) -> Result<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_secs() as i64;
        
        let claims = Claims {
            sub: user_id,
            email,
            role: role.to_string(),
            exp: now + expiry_seconds,
            iat: now,
        };

        Ok(encode(&Header::default(), &claims, &self.encoding_key)?)
    }

    pub fn verify_token(&self, token: &str) -> Result<Claims> {
        let token_data = decode::<Claims>(
            token,
            &self.decoding_key,
            &Validation::default(),
        )?;
        
        Ok(token_data.claims)
    }

    pub fn extract_from_header(&self, auth_header: &str) -> Result<Claims> {
        if !auth_header.starts_with("Bearer ") {
            anyhow::bail!("Invalid authorization header format");
        }

        let token = auth_header[7..].trim();
        self.verify_token(token)
    }
}

// User model
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub role: String,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl User {
    pub async fn find_by_email(pool: &sqlx::PgPool, email: &str) -> Result<Option<Self>> {
        let user = sqlx::query_as::<_, User>(
            r#"
            SELECT id, email, password_hash, role, is_active, created_at, updated_at
            FROM users 
            WHERE email = $1 AND is_active = true
            "#
        )
        .bind(email)
        .fetch_optional(pool)
        .await?;
        
        Ok(user)
    }

    pub async fn find_by_id(pool: &sqlx::PgPool, user_id: Uuid) -> Result<Option<Self>> {
        let user = sqlx::query_as::<_, User>(
            r#"
            SELECT id, email, password_hash, role, is_active, created_at, updated_at
            FROM users 
            WHERE id = $1 AND is_active = true
            "#
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
        
        Ok(user)
    }

    pub async fn create(
        pool: &sqlx::PgPool,
        email: &str,
        password: &str,
        role: &str,
    ) -> Result<Self> {
        let password_hash = Self::hash_password(password)?;
        let id = Uuid::new_v4();
        let now = chrono::Utc::now();

        let user = sqlx::query_as::<_, User>(
            r#"
            INSERT INTO users (id, email, password_hash, role, is_active, created_at, updated_at)
            VALUES ($1, $2, $3, $4, true, $5, $6)
            RETURNING id, email, password_hash, role, is_active, created_at, updated_at
            "#
        )
        .bind(id)
        .bind(email)
        .bind(password_hash)
        .bind(role)
        .bind(now)
        .bind(now)
        .fetch_one(pool)
        .await?;

        Ok(user)
    }

    pub async fn update_password(&self, pool: &sqlx::PgPool, new_password: &str) -> Result<()> {
        let password_hash = Self::hash_password(new_password)?;
        let now = chrono::Utc::now();

        sqlx::query(
            r#"
            UPDATE users 
            SET password_hash = $1, updated_at = $2
            WHERE id = $3
            "#
        )
        .bind(password_hash)
        .bind(now)
        .bind(self.id)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn deactivate(&self, pool: &sqlx::PgPool) -> Result<()> {
        let now = chrono::Utc::now();

        sqlx::query(
            r#"
            UPDATE users 
            SET is_active = false, updated_at = $1
            WHERE id = $2
            "#
        )
        .bind(now)
        .bind(self.id)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub fn verify_password(&self, password: &str) -> Result<bool> {
        let parsed_hash = PasswordHash::new(&self.password_hash)
            .map_err(|e| anyhow::anyhow!("Failed to parse password hash: {}", e))?;
        
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    }

    pub fn hash_password(password: &str) -> Result<String> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| anyhow::anyhow!("Failed to hash password: {}", e))?;
        Ok(password_hash.to_string())
    }
}

// RBAC middleware
pub struct RequireAuth {
    required_role: Option<String>,
    auth_context: Arc<AuthContext>,
}

impl RequireAuth {
    pub fn new(auth_context: Arc<AuthContext>, required_role: Option<&str>) -> Self {
        Self {
            required_role: required_role.map(|s| s.to_string()),
            auth_context,
        }
    }

    pub async fn authenticate(
        &self,
        mut request: crate::api::Request,
    ) -> Result<crate::api::Request, crate::api::Response> {
        let auth_header = match request.headers().get("Authorization") {
            Some(h) => h.to_str().map_err(|_| {
                crate::api::Response::unauthorized("Invalid authorization header")
            })?,
            None => {
                return Err(crate::api::Response::unauthorized(
                    "Authorization header required"
                ));
            }
        };

        let claims = match self.auth_context.extract_from_header(auth_header) {
            Ok(c) => c,
            Err(e) => {
                return Err(crate::api::Response::unauthorized(
                    &format!("Invalid token: {}", e)
                ));
            }
        };

        // Check role if required
        if let Some(required_role) = &self.required_role {
            if &claims.role != required_role && claims.role != "admin" {
                return Err(crate::api::Response::forbidden(
                    "Insufficient permissions"
                ));
            }
        }

        // Add claims to request extensions
        request.extensions_mut().insert(
            std::any::type_name::<Claims>().to_string(),
            Box::new(claims) as Box<dyn std::any::Any + Send + Sync>
        );
        
        Ok(request)
    }
}

// Built-in auth endpoints
pub async fn login_handler(mut request: crate::api::Request) -> crate::api::Response {
    #[derive(Deserialize)]
    struct LoginRequest {
        email: String,
        password: String,
    }

    let login_req: LoginRequest = match request.json().await {
        Ok(r) => r,
        Err(e) => return crate::api::Response::bad_request(
            &format!("Invalid request: {}", e)
        ),
    };

    // Validate input
    if login_req.email.is_empty() || login_req.password.is_empty() {
        return crate::api::Response::bad_request("Email and password are required");
    }

    let pool = match request.db_pool() {
        Some(p) => p,
        None => return crate::api::Response::internal_error("Database not configured"),
    };

    let user = match User::find_by_email(pool.get(), &login_req.email).await {
        Ok(Some(u)) => u,
        Ok(None) => return crate::api::Response::unauthorized("Invalid credentials"),
        Err(e) => return crate::api::Response::internal_error(
            &format!("Database error: {}", e)
        ),
    };

    if !user.verify_password(&login_req.password).unwrap_or(false) {
        return crate::api::Response::unauthorized("Invalid credentials");
    }

    let auth = match request.auth() {
        Some(a) => a,
        None => return crate::api::Response::internal_error("Auth not configured"),
    };

    let token = match auth.create_token(user.id, Some(user.email.clone()), &user.role, 86400) { // 24 hours
        Ok(t) => t,
        Err(e) => return crate::api::Response::internal_error(
            &format!("Failed to create token: {}", e)
        ),
    };

    #[derive(Serialize)]
    struct LoginResponse {
        token: String,
        user_id: Uuid,
        email: String,
        role: String,
        expires_in: i64,
    }

    crate::api::Response::json(
        hyper::StatusCode::OK,
        &LoginResponse {
            token,
            user_id: user.id,
            email: user.email,
            role: user.role,
            expires_in: 86400,
        }
    )
}

pub async fn register_handler(mut request: crate::api::Request) -> crate::api::Response {
    #[derive(Deserialize)]
    struct RegisterRequest {
        email: String,
        password: String,
        role: Option<String>,
    }

    let register_req: RegisterRequest = match request.json().await {
        Ok(r) => r,
        Err(e) => return crate::api::Response::bad_request(
            &format!("Invalid request: {}", e)
        ),
    };

    // Validate input
    if register_req.email.is_empty() {
        return crate::api::Response::bad_request("Email is required");
    }

    if register_req.password.len() < 8 {
        return crate::api::Response::bad_request(
            "Password must be at least 8 characters long"
        );
    }

    // Basic email validation
    if !register_req.email.contains('@') {
        return crate::api::Response::bad_request("Invalid email format");
    }

    let pool = match request.db_pool() {
        Some(p) => p,
        None => return crate::api::Response::internal_error("Database not configured"),
    };

    // Check if user already exists
    match User::find_by_email(pool.get(), &register_req.email).await {
        Ok(Some(_)) => return crate::api::Response::bad_request("Email already registered"),
        Ok(None) => {},
        Err(e) => return crate::api::Response::internal_error(
            &format!("Database error: {}", e)
        ),
    }

    let role = register_req.role.unwrap_or_else(|| "user".to_string());
    
    // Don't allow registration as admin through public endpoint
    if role == "admin" {
        return crate::api::Response::forbidden("Cannot register as admin");
    }

    let user = match User::create(
        pool.get(),
        &register_req.email,
        &register_req.password,
        &role,
    ).await {
        Ok(u) => u,
        Err(e) => return crate::api::Response::internal_error(
            &format!("Failed to create user: {}", e)
        ),
    };

    let auth = match request.auth() {
        Some(a) => a,
        None => return crate::api::Response::internal_error("Auth not configured"),
    };

    let token = match auth.create_token(user.id, Some(user.email.clone()), &user.role, 86400) {
        Ok(t) => t,
        Err(e) => return crate::api::Response::internal_error(
            &format!("Failed to create token: {}", e)
        ),
    };

    #[derive(Serialize)]
    struct RegisterResponse {
        token: String,
        user_id: Uuid,
        email: String,
        role: String,
    }

    crate::api::Response::json(
        hyper::StatusCode::CREATED,
        &RegisterResponse {
            token,
            user_id: user.id,
            email: user.email,
            role: user.role,
        }
    )
}

pub async fn me_handler(request: crate::api::Request) -> crate::api::Response {
    // Extract claims from request extensions
    let claims = match request.get_extension::<Claims>() {
        Some(c) => c,
        None => return crate::api::Response::unauthorized("Not authenticated"),
    };

    let pool = match request.db_pool() {
        Some(p) => p,
        None => return crate::api::Response::internal_error("Database not configured"),
    };

    let user = match User::find_by_id(pool.get(), claims.sub).await {
        Ok(Some(u)) => u,
        Ok(None) => return crate::api::Response::not_found("User not found"),
        Err(e) => return crate::api::Response::internal_error(
            &format!("Database error: {}", e)
        ),
    };

    #[derive(Serialize)]
    struct UserResponse {
        id: Uuid,
        email: String,
        role: String,
        created_at: chrono::DateTime<chrono::Utc>,
    }

    crate::api::Response::json(
        hyper::StatusCode::OK,
        &UserResponse {
            id: user.id,
            email: user.email,
            role: user.role,
            created_at: user.created_at,
        }
    )
}

pub async fn change_password_handler(mut request: crate::api::Request) -> crate::api::Response {
    #[derive(Deserialize)]
    struct ChangePasswordRequest {
        current_password: String,
        new_password: String,
    }

    let change_req: ChangePasswordRequest = match request.json().await {
        Ok(r) => r,
        Err(e) => return crate::api::Response::bad_request(
            &format!("Invalid request: {}", e)
        ),
    };

    if change_req.new_password.len() < 8 {
        return crate::api::Response::bad_request(
            "New password must be at least 8 characters long"
        );
    }

    let claims = match request.get_extension::<Claims>() {
        Some(c) => c,
        None => return crate::api::Response::unauthorized("Not authenticated"),
    };

    let pool = match request.db_pool() {
        Some(p) => p,
        None => return crate::api::Response::internal_error("Database not configured"),
    };

    let user = match User::find_by_id(pool.get(), claims.sub).await {
        Ok(Some(u)) => u,
        Ok(None) => return crate::api::Response::not_found("User not found"),
        Err(e) => return crate::api::Response::internal_error(
            &format!("Database error: {}", e)
        ),
    };

    // Verify current password
    if !user.verify_password(&change_req.current_password).unwrap_or(false) {
        return crate::api::Response::unauthorized("Current password is incorrect");
    }

    // Update password
    if let Err(e) = user.update_password(pool.get(), &change_req.new_password).await {
        return crate::api::Response::internal_error(
            &format!("Failed to update password: {}", e)
        );
    }

    crate::api::Response::json(
        hyper::StatusCode::OK,
        &serde_json::json!({
            "message": "Password updated successfully"
        })
    )
}

// SQL migration for creating the users table
pub const CREATE_USERS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY,
    email VARCHAR(255) UNIQUE NOT NULL,
    password_hash TEXT NOT NULL,
    role VARCHAR(50) NOT NULL DEFAULT 'user',
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_users_email ON users(email);
CREATE INDEX IF NOT EXISTS idx_users_is_active ON users(is_active);
CREATE INDEX IF NOT EXISTS idx_users_role ON users(role);
"#;

// Helper function to run migrations
pub async fn run_migrations(pool: &sqlx::PgPool) -> Result<()> {
    sqlx::query(CREATE_USERS_TABLE)
        .execute(pool)
        .await?;
    
    Ok(())
}

// Password strength checker
pub fn check_password_strength(password: &str) -> PasswordStrength {
    let mut score = 0;
    
    if password.len() >= 8 {
        score += 1;
    }
    if password.len() >= 12 {
        score += 1;
    }
    if password.chars().any(|c| c.is_uppercase()) {
        score += 1;
    }
    if password.chars().any(|c| c.is_lowercase()) {
        score += 1;
    }
    if password.chars().any(|c| c.is_numeric()) {
        score += 1;
    }
    if password.chars().any(|c| !c.is_alphanumeric()) {
        score += 1;
    }
    
    match score {
        0..=2 => PasswordStrength::Weak,
        3..=4 => PasswordStrength::Medium,
        5..=6 => PasswordStrength::Strong,
        _ => PasswordStrength::Strong,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasswordStrength {
    Weak,
    Medium,
    Strong,
}

impl PasswordStrength {
    pub fn is_acceptable(&self) -> bool {
        matches!(self, PasswordStrength::Medium | PasswordStrength::Strong)
    }
}
