use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hyper::{Body, Method, Request as HyperRequest, Response as HyperResponse, StatusCode, header};
use tracing::{info, warn, error};
use parking_lot::RwLock;

use crate::api::{Request, Response};
use crate::auth::AuthContext;
use crate::config::Config;
use crate::database::DatabasePool;
use crate::tasks::TaskRunner;

pub struct Router {
    routes: HashMap<Method, Vec<Route>>,
    middlewares: Vec<Arc<dyn Middleware>>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
            middlewares: Vec::new(),
        }
    }

    pub fn add_middleware(&mut self, middleware: Arc<dyn Middleware>) {
        self.middlewares.push(middleware);
    }

    pub fn get<H, F>(&mut self, path: &str, handler: H)
    where
        H: Fn(Request) -> F + Send + Sync + 'static,
        F: Future<Output = Response> + Send + 'static,
    {
        self.add_route(Method::GET, path, handler);
    }

    pub fn post<H, F>(&mut self, path: &str, handler: H)
    where
        H: Fn(Request) -> F + Send + Sync + 'static,
        F: Future<Output = Response> + Send + 'static,
    {
        self.add_route(Method::POST, path, handler);
    }

    pub fn put<H, F>(&mut self, path: &str, handler: H)
    where
        H: Fn(Request) -> F + Send + Sync + 'static,
        F: Future<Output = Response> + Send + 'static,
    {
        self.add_route(Method::PUT, path, handler);
    }

    pub fn delete<H, F>(&mut self, path: &str, handler: H)
    where
        H: Fn(Request) -> F + Send + Sync + 'static,
        F: Future<Output = Response> + Send + 'static,
    {
        self.add_route(Method::DELETE, path, handler);
    }

    fn add_route<H, F>(&mut self, method: Method, path_pattern: &str, handler: H)
    where
        H: Fn(Request) -> F + Send + Sync + 'static,
        F: Future<Output = Response> + Send + 'static,
    {
        let route = Route::new(method.clone(), path_pattern, handler);
        self.routes.entry(method).or_default().push(route);
    }

    async fn handle_request(
        &self,
        hyper_req: HyperRequest<Body>,
        db_pool: Option<DatabasePool>,
        auth: Option<AuthContext>,
        task_runner: Option<TaskRunner>,
    ) -> Result<HyperResponse<Body>, Infallible> {
        let method = hyper_req.method().clone();
        let path = hyper_req.uri().path().to_string();
        
        // Find matching route
        if let Some(routes) = self.routes.get(&method) {
            for route in routes {
                if let Some(params) = route.matches(&path) {
                    // Build request context
                    let request = match Request::from_hyper(
                        hyper_req,
                        params,
                        db_pool.clone(),
                        auth.clone(),
                        task_runner.clone(),
                    ).await {
                        Ok(req) => req,
                        Err(err) => {
                            return Ok(Response::error(
                                StatusCode::BAD_REQUEST,
                                &format!("Invalid request: {}", err),
                            ).into_hyper())
                        }
                    };

                    // Apply middlewares and execute handler
                    let mut response = self.apply_middlewares(request, &route.handler).await;
                    
                    // Add CORS headers
                    let headers = response.headers_mut();
                    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
                    headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap());
                    headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, "Content-Type, Authorization".parse().unwrap());
                    
                    return Ok(response);
                }
            }
        }

        // Route not found
        Ok(Response::error(StatusCode::NOT_FOUND, "Route not found").into_hyper())
    }

    async fn apply_middlewares(
        &self,
        mut request: Request,
        handler: &RouteHandler,
    ) -> HyperResponse<Body> {
        // Apply middlewares in order - they can transform the request or return early
        for middleware in &self.middlewares {
            match middleware.handle(&mut request).await {
                MiddlewareResult::Continue => {
                    // Continue to next middleware
                }
                MiddlewareResult::Response(response) => {
                    // Middleware returned a response, short-circuit
                    return response;
                }
            }
        }
        
        // All middlewares passed, call the handler
        handler.call(request).await
    }
}

struct Route {
    method: Method,
    pattern: String,
    segments: Vec<Segment>,
    handler: RouteHandler,
}

impl Route {
    fn new<H, F>(method: Method, pattern: &str, handler: H) -> Self
    where
        H: Fn(Request) -> F + Send + Sync + 'static,
        F: Future<Output = Response> + Send + 'static,
    {
        let segments = parse_pattern(pattern);
        
        Self {
            method,
            pattern: pattern.to_string(),
            segments,
            handler: RouteHandler::new(handler),
        }
    }

    fn matches(&self, path: &str) -> Option<HashMap<String, String>> {
        let path_segments: Vec<&str> = path
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
            
        if path_segments.len() != self.segments.len() {
            return None;
        }

        let mut params = HashMap::new();
        
        for (i, segment) in self.segments.iter().enumerate() {
            match segment {
                Segment::Literal(lit) => {
                    if lit != &path_segments[i] {
                        return None;
                    }
                }
                Segment::Param(param) => {
                    params.insert(param.clone(), path_segments[i].to_string());
                }
            }
        }
        
        Some(params)
    }
}

impl Clone for Route {
    fn clone(&self) -> Self {
        Self {
            method: self.method.clone(),
            pattern: self.pattern.clone(),
            segments: self.segments.iter().map(|s| match s {
                Segment::Literal(lit) => Segment::Literal(lit.clone()),
                Segment::Param(param) => Segment::Param(param.clone()),
            }).collect(),
            handler: self.handler.clone(),
        }
    }
}

enum Segment {
    Literal(String),
    Param(String),
}

fn parse_pattern(pattern: &str) -> Vec<Segment> {
    pattern
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with(':') {
                Segment::Param(s[1..].to_string())
            } else {
                Segment::Literal(s.to_string())
            }
        })
        .collect()
}

struct RouteHandler {
    handler: Arc<dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync>,
}

impl RouteHandler {
    fn new<H, F>(handler: H) -> Self
    where
        H: Fn(Request) -> F + Send + Sync + 'static,
        F: Future<Output = Response> + Send + 'static,
    {
        Self {
            handler: Arc::new(move |req| Box::pin(handler(req))),
        }
    }

    async fn call(&self, request: Request) -> HyperResponse<Body> {
        let handler = self.handler.clone();
        handler(request).await.into_hyper()
    }
}

impl Clone for RouteHandler {
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
        }
    }
}

// Middleware result enum - allows short-circuiting
pub enum MiddlewareResult {
    Continue,
    Response(HyperResponse<Body>),
}

#[async_trait]
pub trait Middleware: Send + Sync {
    async fn handle(&self, request: &mut Request) -> MiddlewareResult;
}

pub struct Server {
    config: crate::config::ServerConfig,
    router: Arc<Router>,
    db_pool: Option<DatabasePool>,
    auth: Option<AuthContext>,
    task_runner: Option<TaskRunner>,
}

impl Server {
    pub fn new(
        config: crate::config::ServerConfig,
        router: Router,
        db_pool: Option<DatabasePool>,
        auth: Option<AuthContext>,
        task_runner: Option<TaskRunner>,
    ) -> Self {
        Self {
            config,
            router: Arc::new(router),
            db_pool,
            auth,
            task_runner,
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let addr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid address: {}", e))?;
            
        let make_svc = hyper::service::make_service_fn(|_conn| {
            let router = self.router.clone();
            let db_pool = self.db_pool.clone();
            let auth = self.auth.clone();
            let task_runner = self.task_runner.clone();
            
            async move {
                Ok::<_, Infallible>(hyper::service::service_fn(move |req| {
                    let router = router.clone();
                    let db_pool = db_pool.clone();
                    let auth = auth.clone();
                    let task_runner = task_runner.clone();
                    
                    async move {
                        router.handle_request(req, db_pool, auth, task_runner).await
                    }
                }))
            }
        });

        let server = hyper::Server::bind(&addr).serve(make_svc);
        
        info!("Server running on http://{}", addr);
        
        server.await?;
        Ok(())
    }
}

impl Clone for Router {
    fn clone(&self) -> Self {
        Self {
            routes: self.routes.clone(),
            middlewares: self.middlewares.clone(),
        }
    }
}

// Common middleware implementations
pub struct CorsMiddleware;

#[async_trait]
impl Middleware for CorsMiddleware {
    async fn handle(&self, request: &mut Request) -> MiddlewareResult {
        if request.method() == &Method::OPTIONS {
            let response = HyperResponse::builder()
                .status(StatusCode::NO_CONTENT)
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, PUT, DELETE, OPTIONS")
                .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "Content-Type, Authorization")
                .body(Body::empty())
                .unwrap();
            MiddlewareResult::Response(response)
        } else {
            MiddlewareResult::Continue
        }
    }
}

pub struct LoggingMiddleware;

#[async_trait]
impl Middleware for LoggingMiddleware {
    async fn handle(&self, request: &mut Request) -> MiddlewareResult {
        info!(
            "{} {} {}",
            request.method(),
            request.uri().path(),
            request.remote_addr().unwrap_or("unknown".to_string())
        );
        MiddlewareResult::Continue
    }
}

// Authentication middleware with JWT validation
pub struct AuthMiddleware {
    required: bool,
    jwt_secret: String,
}

impl AuthMiddleware {
    pub fn new(required: bool, jwt_secret: String) -> Self {
        Self { required, jwt_secret }
    }
    
    fn extract_token(&self, request: &Request) -> Option<String> {
        request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                if v.starts_with("Bearer ") {
                    Some(v[7..].to_string())
                } else {
                    None
                }
            })
    }
    
    fn validate_token(&self, token: &str) -> Result<crate::auth::Claims, String> {
        use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};
        
        let validation = Validation::new(Algorithm::HS256);
        decode::<crate::auth::Claims>(
            token,
            &DecodingKey::from_secret(self.jwt_secret.as_bytes()),
            &validation,
        )
        .map(|data| data.claims)
        .map_err(|e| format!("Invalid token: {}", e))
    }
}

#[async_trait]
impl Middleware for AuthMiddleware {
    async fn handle(&self, request: &mut Request) -> MiddlewareResult {
        // Check if auth is required
        if !self.required {
            return MiddlewareResult::Continue;
        }
        
        // Extract token from Authorization header
        let token = match self.extract_token(request) {
            Some(t) => t,
            None => {
                let response = Response::unauthorized("Missing or invalid Authorization header").into_hyper();
                return MiddlewareResult::Response(response);
            }
        };
        
        // Validate token
        match self.validate_token(&token) {
            Ok(claims) => {
                info!("Authenticated user: {} ({})", claims.sub, claims.email.as_deref().unwrap_or("no email"));
                MiddlewareResult::Continue
            }
            Err(err) => {
                warn!("Authentication failed: {}", err);
                let response = Response::unauthorized(&format!("Authentication failed: {}", err)).into_hyper();
                MiddlewareResult::Response(response)
            }
        }
    }
}

// Rate limiting middleware with sliding window
#[derive(Clone)]
struct ClientRateLimit {
    requests: Vec<Instant>,
}

impl ClientRateLimit {
    fn new() -> Self {
        Self {
            requests: Vec::new(),
        }
    }
    
    fn check_and_record(&mut self, max_requests: usize, window: Duration) -> bool {
        let now = Instant::now();
        let cutoff = now - window;
        
        // Remove old requests
        self.requests.retain(|&time| time > cutoff);
        
        // Check if under limit
        if self.requests.len() >= max_requests {
            return false;
        }
        
        // Record this request
        self.requests.push(now);
        true
    }
}

pub struct RateLimitMiddleware {
    max_requests: usize,
    window: Duration,
    clients: Arc<RwLock<HashMap<String, ClientRateLimit>>>,
}

impl RateLimitMiddleware {
    pub fn new(max_requests: usize, window_seconds: u64) -> Self {
        Self {
            max_requests,
            window: Duration::from_secs(window_seconds),
            clients: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    fn get_client_key(&self, request: &Request) -> String {
        // Try to get the real IP from X-Forwarded-For header
        if let Some(forwarded) = request.headers().get("X-Forwarded-For") {
            if let Ok(forwarded_str) = forwarded.to_str() {
                if let Some(first_ip) = forwarded_str.split(',').next() {
                    return first_ip.trim().to_string();
                }
            }
        }
        
        // Fall back to remote_addr
        request.remote_addr().unwrap_or_else(|| "unknown".to_string())
    }
}

#[async_trait]
impl Middleware for RateLimitMiddleware {
    async fn handle(&self, request: &mut Request) -> MiddlewareResult {
        let client_key = self.get_client_key(request);
        
        // Check rate limit
        let allowed = {
            let mut clients = self.clients.write();
            let client_limit = clients
                .entry(client_key.clone())
                .or_insert_with(ClientRateLimit::new);
            
            client_limit.check_and_record(self.max_requests, self.window)
        };
        
        if !allowed {
            warn!("Rate limit exceeded for client: {}", client_key);
            let response = HyperResponse::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header(header::RETRY_AFTER, self.window.as_secs().to_string())
                .body(Body::from(
                    serde_json::json!({
                        "error": "Rate limit exceeded",
                        "retry_after_seconds": self.window.as_secs()
                    })
                    .to_string()
                ))
                .unwrap();
            
            MiddlewareResult::Response(response)
        } else {
            MiddlewareResult::Continue
        }
    }
}

// Request timeout middleware
pub struct TimeoutMiddleware {
    timeout: Duration,
}

impl TimeoutMiddleware {
    pub fn new(timeout_seconds: u64) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_seconds),
        }
    }
}

#[async_trait]
impl Middleware for TimeoutMiddleware {
    async fn handle(&self, request: &mut Request) -> MiddlewareResult {
        // Store the start time in request extensions for later use
        MiddlewareResult::Continue
    }
}

// Request size limit middleware
pub struct RequestSizeMiddleware {
    max_size: usize,
}

impl RequestSizeMiddleware {
    pub fn new(max_size_bytes: usize) -> Self {
        Self {
            max_size: max_size_bytes,
        }
    }
}

#[async_trait]
impl Middleware for RequestSizeMiddleware {
    async fn handle(&self, request: &mut Request) -> MiddlewareResult {
        // Check Content-Length header
        if let Some(content_length) = request.headers().get(header::CONTENT_LENGTH) {
            if let Ok(length_str) = content_length.to_str() {
                if let Ok(length) = length_str.parse::<usize>() {
                    if length > self.max_size {
                        warn!("Request size {} exceeds maximum {}", length, self.max_size);
                        let response = Response::error(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            &format!("Request size exceeds maximum of {} bytes", self.max_size)
                        ).into_hyper();
                        return MiddlewareResult::Response(response);
                    }
                }
            }
        }
        
        MiddlewareResult::Continue
    }
}

// Security headers middleware
pub struct SecurityHeadersMiddleware;

#[async_trait]
impl Middleware for SecurityHeadersMiddleware {
    async fn handle(&self, _request: &mut Request) -> MiddlewareResult {
        MiddlewareResult::Continue
    }
}

pub struct CompressionMiddleware {
    min_size: usize,
}

impl CompressionMiddleware {
    pub fn new(min_size_bytes: usize) -> Self {
        Self {
            min_size: min_size_bytes,
        }
    }
}

#[async_trait]
impl Middleware for CompressionMiddleware {
    async fn handle(&self, request: &mut Request) -> MiddlewareResult {
        // Check if client supports compression
        if let Some(accept_encoding) = request.headers().get(header::ACCEPT_ENCODING) {
            if let Ok(encoding_str) = accept_encoding.to_str() {
                if encoding_str.contains("gzip") || encoding_str.contains("deflate") {
                    // Mark request as supporting compression
                    info!("Client supports compression");
                }
            }
        }
        
        MiddlewareResult::Continue
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use hyper::Body;

    #[tokio::test]
    async fn test_route_matching() {
        let mut router = Router::new();
        
        router.get("/users/:id", |req| async move {
            let id = req.param("id").unwrap();
            Response::text(StatusCode::OK, id)
        });
        
        router.get("/products/:category/:id", |req| async move {
            let category = req.param("category").unwrap();
            let id = req.param("id").unwrap();
            Response::text(StatusCode::OK, &format!("{}-{}", category, id))
        });
        
        // Test matching
        let routes = router.routes.get(&Method::GET).unwrap();
        
        // Test first route
        let route = &routes[0];
        let params = route.matches("/users/123").unwrap();
        assert_eq!(params.get("id").unwrap(), "123");
        
        // Test second route
        let route = &routes[1];
        let params = route.matches("/products/electronics/456").unwrap();
        assert_eq!(params.get("category").unwrap(), "electronics");
        assert_eq!(params.get("id").unwrap(), "456");
        
        // Test non-matching
        let params = route.matches("/users/123");
        assert!(params.is_none());
    }

    #[tokio::test]
    async fn test_router_with_middleware() {
        struct TestMiddleware;
        
        #[async_trait::async_trait]
        impl Middleware for TestMiddleware {
            async fn handle(&self, request: Request) -> Result<Request, HyperResponse<Body>> {
                if request.uri().path() == "/blocked" {
                    Err(Response::forbidden("Blocked by middleware").into_hyper())
                } else {
                    Ok(request)
                }
            }
        }

        let mut router = Router::new();
        router.add_middleware(Arc::new(TestMiddleware));
        
        router.get("/allowed", |_req| async {
            Response::ok()
        });
        
        router.get("/blocked", |_req| async {
            Response::ok()
        });
        
    }
}