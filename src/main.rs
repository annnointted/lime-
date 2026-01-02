use anyhow::Result;
use lime::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();
    
    // Create a new Lime app
    let mut app = Lime::new()
        .config_from_file("config.toml")?;
    
    // Optionally add database if configured
    if !app.config().database.host.is_empty() {
        app = app.with_db_pool().await?;
        println!("--- Database connected ---");
    }
    
    // Optionally add auth if JWT secret is configured
    if !app.config().auth.jwt_secret.is_empty() {
        let jwt_secret = app.config().auth.jwt_secret.clone();
        app = app.with_auth(&jwt_secret)?;
        println!("--- Auth configured ---");
    }
    
    // Optionally add task runner if Redis is configured
    if app.config().redis.is_some() {
        app = app.with_task_runner().await?;
        println!("--- Task runner initialized ---");
    }
    
    println!("--- Server starting on http://{}:{} ---", 
             app.config().server.host, 
             app.config().server.port);
    
    // Define routes
    app.get("/health", lime::api::health_handler)
        .get("/metrics", lime::api::metrics_handler)
        .get("/users/:id", get_user)
        .run()
        .await
}


lime::db_model! {
    User {
        email: String,
        password_hash: String,
        role: String,
        is_active: bool,
    }
}

// Example endpoint
async fn get_user(request: Request) -> Response {
    let user_id = match request.param("id") {
        Some(id) => id,
        None => return Response::bad_request("Missing user ID"),
    };
    
    let pool = match request.db_pool() {
        Some(p) => p,
        None => return Response::internal_error("Database not available"),
    };
    
    let user_id = match Uuid::parse_str(user_id) {
        Ok(id) => id,
        Err(_) => return Response::bad_request("Invalid user ID"),
    };
    
    match User::find_by_id(pool.get(), user_id).await {
        Ok(Some(user)) => Response::json(StatusCode::OK, &user),
        Ok(None) => Response::not_found("User not found"),
        Err(e) => Response::internal_error(&format!("Database error: {}", e)),
    }
}
