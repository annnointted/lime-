use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use hyper::{Body, HeaderMap, Method, Request as HyperRequest, Response as HyperResponse, StatusCode, Uri, header};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use validator::Validate;

use crate::auth::{AuthContext, Claims};
use crate::database::DatabasePool;
use crate::tasks::TaskRunner;

pub struct Request {
    inner: HyperRequest<Body>,
    params: HashMap<String, String>,
    query: HashMap<String, String>,
    extensions: HashMap<String, Box<dyn std::any::Any + Send + Sync>>,
    db_pool: Option<DatabasePool>,
    auth: Option<AuthContext>,
    task_runner: Option<TaskRunner>,
}

impl Request {
    pub async fn from_hyper(
        mut hyper_req: HyperRequest<Body>,
        params: HashMap<String, String>,
        db_pool: Option<DatabasePool>,
        auth: Option<AuthContext>,
        task_runner: Option<TaskRunner>,
    ) -> anyhow::Result<Self> {
        // Parse query string
        let query = hyper_req
            .uri()
            .query()
            .map(parse_query)
            .unwrap_or_default();

        // Extract extensions if any
        let extensions = hyper_req
            .extensions_mut()
            .remove::<HashMap<String, Box<dyn std::any::Any + Send + Sync>>>()
            .unwrap_or_default();

        Ok(Self {
            inner: hyper_req,
            params,
            query,
            extensions,
            db_pool,
            auth,
            task_runner,
        })
    }

    pub async fn json<T: DeserializeOwned>(&mut self) -> anyhow::Result<T> {
        let body_bytes = hyper::body::to_bytes(self.inner.body_mut()).await?;
        Ok(serde_json::from_slice(&body_bytes)?)
    }

    pub async fn json_validated<T>(&mut self) -> anyhow::Result<T>
    where
        T: DeserializeOwned + Validate,
    {
        let value: T = self.json().await?;
        value.validate()?;
        Ok(value)
    }

    pub fn params(&self) -> &HashMap<String, String> {
        &self.params
    }

    pub fn param(&self, key: &str) -> Option<&String> {
        self.params.get(key)
    }

    pub fn query(&self) -> &HashMap<String, String> {
        &self.query
    }

    pub fn query_param(&self, key: &str) -> Option<&String> {
        self.query.get(key)
    }

    pub fn method(&self) -> &Method {
        self.inner.method()
    }

    pub fn uri(&self) -> &Uri {
        self.inner.uri()
    }

    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    pub fn remote_addr(&self) -> Option<String> {
        self.inner
            .extensions()
            .get::<SocketAddr>()
            .map(|addr| addr.to_string())
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

    pub fn extensions(&self) -> &HashMap<String, Box<dyn std::any::Any + Send + Sync>> {
        &self.extensions
    }

    pub fn extensions_mut(&mut self) -> &mut HashMap<String, Box<dyn std::any::Any + Send + Sync>> {
        &mut self.extensions
    }

    pub fn get_extension<T: 'static>(&self) -> Option<&T> {
        self.extensions
            .get(std::any::type_name::<T>())
            .and_then(|boxed| boxed.downcast_ref::<T>())
    }
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            match (parts.next(), parts.next()) {
                (Some(key), Some(value)) => {
                    Some((key.to_string(), urlencoding::decode(value).unwrap_or_else(|_| value.into()).to_string()))
                }
                _ => None,
            }
        })
        .collect()
}

pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    body: Body,
}

impl Response {
    pub fn new(status: StatusCode) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body: Body::empty(),
        }
    }

    pub fn json<T: Serialize>(status: StatusCode, data: &T) -> Self {
        let body = match serde_json::to_string(data) {
            Ok(json) => json,
            Err(_) => return Self::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to serialize response"
            ),
        };

        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());

        Self {
            status,
            headers,
            body: Body::from(body),
        }
    }

    pub fn error(status: StatusCode, message: &str) -> Self {
        #[derive(Serialize)]
        struct ErrorResponse {
            error: String,
            status: u16,
        }

        Self::json(
            status,
            &ErrorResponse {
                error: message.to_string(),
                status: status.as_u16(),
            },
        )
    }

    pub fn text(status: StatusCode, text: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());

        Self {
            status,
            headers,
            body: Body::from(text.to_string()),
        }
    }

    pub fn html(status: StatusCode, html: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "text/html; charset=utf-8".parse().unwrap());

        Self {
            status,
            headers,
            body: Body::from(html.to_string()),
        }
    }

    pub fn with_header(mut self, key: &str, value: &str) -> Self {
        if let (Ok(header_name), Ok(header_value)) = (
            key.parse::<header::HeaderName>(),
            value.parse::<header::HeaderValue>()
        ) {
            self.headers.insert(header_name, header_value);
        }
        self
    }

    pub fn with_cookie(self, name: &str, value: &str, max_age_seconds: Option<i64>) -> Self {
        let mut cookie = format!("{}={}", name, value);
        
        if let Some(max_age) = max_age_seconds {
            cookie.push_str(&format!("; Max-Age={}", max_age));
        }
        
        cookie.push_str("; Path=/; HttpOnly; SameSite=Lax");
        
        self.with_header("Set-Cookie", &cookie)
    }

    pub fn redirect(location: &str) -> Self {
        Self::new(StatusCode::FOUND)
            .with_header("Location", location)
    }

    pub fn into_hyper(self) -> HyperResponse<Body> {
        let mut response = HyperResponse::builder()
            .status(self.status);
        
        // Add all headers
        for (key, value) in self.headers.iter() {
            response = response.header(key, value);
        }
        
        response.body(self.body).unwrap()
    }
}

// Helper functions for common responses
impl Response {
    pub fn ok() -> Self {
        Self::new(StatusCode::OK)
    }

    pub fn created() -> Self {
        Self::new(StatusCode::CREATED)
    }

    pub fn no_content() -> Self {
        Self::new(StatusCode::NO_CONTENT)
    }

    pub fn bad_request(message: &str) -> Self {
        Self::error(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: &str) -> Self {
        Self::error(StatusCode::UNAUTHORIZED, message)
    }

    pub fn forbidden(message: &str) -> Self {
        Self::error(StatusCode::FORBIDDEN, message)
    }

    pub fn not_found(message: &str) -> Self {
        Self::error(StatusCode::NOT_FOUND, message)
    }

    pub fn internal_error(message: &str) -> Self {
        Self::error(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    pub fn service_unavailable(message: &str) -> Self {
        Self::error(StatusCode::SERVICE_UNAVAILABLE, message)
    }
}

// Metrics storage using atomic counters for thread-safe updates
pub struct MetricsCollector {
    requests_total: AtomicU64,
    requests_success: AtomicU64,
    requests_error: AtomicU64,
    response_time_sum_ms: AtomicU64,
    last_minute_requests: Arc<parking_lot::Mutex<Vec<Instant>>>,
    start_time: Instant,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            requests_success: AtomicU64::new(0),
            requests_error: AtomicU64::new(0),
            response_time_sum_ms: AtomicU64::new(0),
            last_minute_requests: Arc::new(parking_lot::Mutex::new(Vec::new())),
            start_time: Instant::now(),
        }
    }

    pub fn record_request(&self, response_time_ms: u64, status_code: u16) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.response_time_sum_ms.fetch_add(response_time_ms, Ordering::Relaxed);
        
        if status_code < 400 {
            self.requests_success.fetch_add(1, Ordering::Relaxed);
        } else {
            self.requests_error.fetch_add(1, Ordering::Relaxed);
        }
        
        let now = Instant::now();
        let mut requests = self.last_minute_requests.lock();
        requests.push(now);
        
        // Remove requests older than 1 minute
        let one_minute_ago = now - Duration::from_secs(60);
        requests.retain(|&time| time > one_minute_ago);
    }

    pub fn get_metrics(&self) -> MetricsSnapshot {
        let total = self.requests_total.load(Ordering::Relaxed);
        let success = self.requests_success.load(Ordering::Relaxed);
        let error = self.requests_error.load(Ordering::Relaxed);
        let sum_ms = self.response_time_sum_ms.load(Ordering::Relaxed);
        
        let requests = self.last_minute_requests.lock();
        let requests_last_minute = requests.len() as f64;
        let requests_per_second = requests_last_minute / 60.0;
        
        let average_response_time_ms = if total > 0 {
            sum_ms as f64 / total as f64
        } else {
            0.0
        };

        let success_rate = if total > 0 {
            (success as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        MetricsSnapshot {
            requests_total: total,
            requests_success: success,
            requests_error: error,
            success_rate,
            requests_per_second,
            average_response_time_ms,
            uptime_seconds: self.start_time.elapsed().as_secs(),
        }
    }

    pub fn reset(&self) {
        self.requests_total.store(0, Ordering::Relaxed);
        self.requests_success.store(0, Ordering::Relaxed);
        self.requests_error.store(0, Ordering::Relaxed);
        self.response_time_sum_ms.store(0, Ordering::Relaxed);
        self.last_minute_requests.lock().clear();
    }
}

#[derive(Serialize)]
pub struct MetricsSnapshot {
    requests_total: u64,
    requests_success: u64,
    requests_error: u64,
    success_rate: f64,
    requests_per_second: f64,
    average_response_time_ms: f64,
    uptime_seconds: u64,
}

// Global metrics collector using OnceLock (Rust 1.70+)
static METRICS: OnceLock<MetricsCollector> = OnceLock::new();

fn get_metrics_collector() -> &'static MetricsCollector {
    METRICS.get_or_init(|| MetricsCollector::new())
}

// Example endpoints for the API module
pub async fn health_handler(request: Request) -> Response {
    #[derive(Serialize)]
    struct HealthResponse {
        status: String,
        timestamp: String,
        database: Option<String>,
        redis: Option<String>,
    }

    let mut health = HealthResponse {
        status: "healthy".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        database: None,
        redis: None,
    };

    // Check database health if available
    if let Some(pool) = request.db_pool() {
        let db_healthy = pool.health_check().await;
        health.database = Some(if db_healthy { "healthy".to_string() } else { "unhealthy".to_string() });
    }

    // Check Redis/task runner health if available
    if let Some(task_runner) = request.task_runner() {
        #[cfg(feature = "redis")]
        {
            let stats = task_runner.get_queue_stats().await;
            health.redis = Some(if stats.is_ok() { "healthy".to_string() } else { "unhealthy".to_string() });
        }
        #[cfg(not(feature = "redis"))]
        {
            health.redis = Some("not_configured".to_string());
        }
    }

    Response::json(StatusCode::OK, &health)
}

pub async fn metrics_handler(_request: Request) -> Response {
    let metrics = get_metrics_collector().get_metrics();
    Response::json(StatusCode::OK, &metrics)
}

pub async fn reset_metrics_handler(_request: Request) -> Response {
    get_metrics_collector().reset();
    Response::json(StatusCode::OK, &serde_json::json!({
        "message": "Metrics reset successfully"
    }))
}

// Middleware function to wrap handlers and collect metrics
pub async fn with_metrics<F, Fut>(handler: F, request: Request) -> Response
where
    F: FnOnce(Request) -> Fut,
    Fut: std::future::Future<Output = Response>,
{
    let start = Instant::now();
    let response = handler(request).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    
    // Extract status code from response (we'll need to rebuild it)
    let status_code = response.status.as_u16();
    
    get_metrics_collector().record_request(elapsed_ms, status_code);
    
    response
}

// Request validation helpers
pub trait RequestExt {
    fn require_json_content_type(&self) -> Result<(), Response>;
    fn require_auth(&self) -> Result<&Claims, Response>;
    fn get_bearer_token(&self) -> Option<String>;
}

impl RequestExt for Request {
    fn require_json_content_type(&self) -> Result<(), Response> {
        if let Some(content_type) = self.headers().get(header::CONTENT_TYPE) {
            if content_type.to_str().ok().map(|s| s.contains("application/json")).unwrap_or(false) {
                return Ok(());
            }
        }
        Err(Response::bad_request("Content-Type must be application/json"))
    }

    fn require_auth(&self) -> Result<&Claims, Response> {
        self.get_extension::<Claims>()
            .ok_or_else(|| Response::unauthorized("Authentication required"))
    }

    fn get_bearer_token(&self) -> Option<String> {
        self.headers()
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
}

// Response builder for easier response construction
pub struct ResponseBuilder {
    status: StatusCode,
    headers: HashMap<String, String>,
}

impl ResponseBuilder {
    pub fn new(status: StatusCode) -> Self {
        Self {
            status,
            headers: HashMap::new(),
        }
    }

    pub fn header(mut self, key: &str, value: &str) -> Self {
        self.headers.insert(key.to_string(), value.to_string());
        self
    }

    pub fn json<T: Serialize>(self, data: &T) -> Response {
        let mut response = Response::json(self.status, data);
        for (key, value) in self.headers {
            response = response.with_header(&key, &value);
        }
        response
    }

    pub fn text(self, text: &str) -> Response {
        let mut response = Response::text(self.status, text);
        for (key, value) in self.headers {
            response = response.with_header(&key, &value);
        }
        response
    }

    pub fn html(self, html: &str) -> Response {
        let mut response = Response::html(self.status, html);
        for (key, value) in self.headers {
            response = response.with_header(&key, &value);
        }
        response
    }
}

// Pagination helper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pagination {
    pub page: u32,
    pub per_page: u32,
    pub total: u64,
    pub total_pages: u32,
}

impl Pagination {
    pub fn new(page: u32, per_page: u32, total: u64) -> Self {
        let total_pages = ((total as f64) / (per_page as f64)).ceil() as u32;
        Self {
            page,
            per_page,
            total,
            total_pages,
        }
    }

    pub fn offset(&self) -> u64 {
        ((self.page - 1) * self.per_page) as u64
    }

    pub fn limit(&self) -> u64 {
        self.per_page as u64
    }
}

#[derive(Serialize)]
pub struct PaginatedResponse<T> {
    pub data: Vec<T>,
    pub pagination: Pagination,
}

impl<T: Serialize> PaginatedResponse<T> {
    pub fn new(data: Vec<T>, pagination: Pagination) -> Self {
        Self { data, pagination }
    }
}
