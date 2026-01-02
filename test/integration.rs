use anyhow::Result;
use lime::{Lime, Config};
use reqwest::StatusCode;
use serde_json::json;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_health_endpoint() -> Result<()> {
    // Start server on a random port
    let config = Config {
        server: lime::config::ServerConfig {
            port: 0, // 0 means random port
            ..Default::default()
        },
        ..Default::default()
    };
    
    let server = tokio::spawn(async move {
        let mut app = Lime::new().with_config(config).unwrap();
        app.get("/health", |_req| async {
            lime::api::health_handler().await
        })
        .run()
        .await
        .unwrap();
    });
    
    // Give server time to start
    sleep(Duration::from_millis(100)).await;
    
    // Test health endpoint
    let response = reqwest::get("http://localhost:3000/health").await?;
    assert_eq!(response.status(), StatusCode::OK);
    
    let body = response.json::<serde_json::Value>().await?;
    assert_eq!(body["status"], "healthy");
    
    server.abort(); // Stop server
    Ok(())
}

#[tokio::test]
async fn test_router_path_params() -> Result<()> {
    let config = Config {
        server: lime::config::ServerConfig {
            port: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let server = tokio::spawn(async move {
        let mut app = Lime::new().with_config(config).unwrap();
        app.get("/users/:id", |req| async move {
            let id = req.param("id").unwrap();
            lime::Response::json(
                hyper::StatusCode::OK,
                &json!({ "user_id": id })
            )
        })
        .get("/products/:category/:id", |req| async move {
            let category = req.param("category").unwrap();
            let id = req.param("id").unwrap();
            lime::Response::json(
                hyper::StatusCode::OK,
                &json!({ "category": category, "id": id })
            )
        })
        .run()
        .await
        .unwrap();
    });
    
    sleep(Duration::from_millis(100)).await;
    
    // Test path parameters
    let response = reqwest::get("http://localhost:3000/users/123").await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await?;
    assert_eq!(body["user_id"], "123");
    
    // Test multiple path parameters
    let response = reqwest::get("http://localhost:3000/products/electronics/456").await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await?;
    assert_eq!(body["category"], "electronics");
    assert_eq!(body["id"], "456");
    
    server.abort();
    Ok(())
}

#[tokio::test]
async fn test_query_parameters() -> Result<()> {
    let config = Config {
        server: lime::config::ServerConfig {
            port: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let server = tokio::spawn(async move {
        let mut app = Lime::new().with_config(config).unwrap();
        app.get("/search", |req| async move {
            let query = req.query_param("q").unwrap_or("default");
            let page = req.query_param("page").unwrap_or("1");
            lime::Response::json(
                hyper::StatusCode::OK,
                &json!({ "query": query, "page": page })
            )
        })
        .run()
        .await
        .unwrap();
    });
    
    sleep(Duration::from_millis(100)).await;
    
    // Test query parameters
    let response = reqwest::get("http://localhost:3000/search?q=rust&page=2").await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await?;
    assert_eq!(body["query"], "rust");
    assert_eq!(body["page"], "2");
    
    // Test with missing parameters
    let response = reqwest::get("http://localhost:3000/search").await?;
    let body = response.json::<serde_json::Value>().await?;
    assert_eq!(body["query"], "default");
    assert_eq!(body["page"], "1");
    
    server.abort();
    Ok(())
}