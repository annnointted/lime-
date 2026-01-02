use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, error, warn};

#[cfg(feature = "redis")]
use redis::aio::MultiplexedConnection;
#[cfg(feature = "redis")]
use redis::{AsyncCommands, Client};

use crate::config::{Config, RedisConfig};

#[derive(Clone)]
pub struct TaskRunner {
    #[cfg(feature = "redis")]
    redis_client: Option<Arc<Client>>,
    #[cfg(feature = "redis")]
    redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
    #[cfg(not(feature = "redis"))]
    _phantom: std::marker::PhantomData<()>,
    queue_name: String,
    max_retries: u32,
    workers: usize,
}

impl TaskRunner {
    #[cfg(feature = "redis")]
    pub async fn new(config: &Config) -> Result<Self> {
        let (redis_client, redis_conn) = if let Some(redis_config) = &config.redis {
            match Client::open(redis_config.url.as_str()) {
                Ok(client) => {
                    let client_arc = Arc::new(client);
                    // Try to establish connection
                    match client_arc.get_multiplexed_tokio_connection().await {
                        Ok(conn) => {
                            info!("Successfully connected to Redis");
                            (Some(client_arc), Some(Arc::new(Mutex::new(conn))))
                        }
                        Err(e) => {
                            warn!("Failed to connect to Redis: {}. Tasks will run immediately.", e);
                            (None, None)
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to create Redis client: {}. Tasks will run immediately.", e);
                    (None, None)
                }
            }
        } else {
            info!("Redis not configured. Tasks will run immediately.");
            (None, None)
        };

        Ok(Self {
            redis_client,
            redis_conn,
            queue_name: "lime:tasks".to_string(),
            max_retries: 3,
            workers: 4,
        })
    }

    #[cfg(not(feature = "redis"))]
    pub async fn new(config: &Config) -> Result<Self> {
        info!("Redis feature not enabled. Tasks will run immediately.");
        Ok(Self {
            _phantom: std::marker::PhantomData,
            queue_name: "lime:tasks".to_string(),
            max_retries: 3,
            workers: 4,
        })
    }

    #[cfg(feature = "redis")]
    pub async fn enqueue<T: Serialize>(&self, task: &T) -> Result<()> {
        if let Some(conn) = &self.redis_conn {
            let task_json = serde_json::to_string(task)?;
            let mut conn = conn.lock().await;
            conn.lpush(&self.queue_name, task_json).await?;
            info!("Task enqueued to Redis queue: {}", self.queue_name);
        } else {
            // In-memory fallback - execute immediately
            warn!("Redis not available, executing task immediately");
            self.execute_task_json(&serde_json::to_value(task)?).await?;
        }
        
        Ok(())
    }

    #[cfg(not(feature = "redis"))]
    pub async fn enqueue<T: Serialize>(&self, task: &T) -> Result<()> {
        warn!("Redis not available, executing task immediately");
        self.execute_task_json(&serde_json::to_value(task)?).await?;
        Ok(())
    }

    async fn execute_task_json(&self, task: &serde_json::Value) -> Result<()> {
        // Log task execution
        info!("Executing task: {}", serde_json::to_string_pretty(task)?);
        
        // Extract task type and execute appropriate handler
        if let Some(task_type) = task.get("type").and_then(|v| v.as_str()) {
            match task_type {
                "email" => {
                    if let Ok(email_task) = serde_json::from_value::<EmailTask>(task.clone()) {
                        email_task.execute().await?;
                    }
                }
                "health_check" => {
                    info!("Health check task executed at {}", 
                        task.get("timestamp").and_then(|v| v.as_str()).unwrap_or("unknown"));
                }
                "notification" => {
                    if let Ok(notification) = serde_json::from_value::<NotificationTask>(task.clone()) {
                        notification.execute().await?;
                    }
                }
                "data_cleanup" => {
                    if let Ok(cleanup) = serde_json::from_value::<DataCleanupTask>(task.clone()) {
                        cleanup.execute().await?;
                    }
                }
                _ => {
                    warn!("Unknown task type: {}", task_type);
                }
            }
        } else {
            warn!("Task missing 'type' field");
        }
        
        Ok(())
    }

    #[cfg(feature = "redis")]
    pub fn start_workers(&self) -> Vec<JoinHandle<()>> {
        if self.redis_conn.is_none() {
            info!("Redis not available, skipping worker startup");
            return Vec::new();
        }

        let mut handles = Vec::new();
        
        for worker_id in 0..self.workers {
            let runner = self.clone();
            let handle = tokio::spawn(async move {
                runner.worker_loop(worker_id).await;
            });
            handles.push(handle);
        }
        
        info!("Started {} workers", self.workers);
        handles
    }

    #[cfg(not(feature = "redis"))]
    pub fn start_workers(&self) -> Vec<JoinHandle<()>> {
        info!("Redis not available, skipping worker startup");
        Vec::new()
    }

    #[cfg(feature = "redis")]
    async fn worker_loop(&self, worker_id: usize) {
        info!("Worker {} started", worker_id);
        
        loop {
            if let Some(conn) = &self.redis_conn {
                match self.process_next_task(conn).await {
                    Ok(processed) => {
                        if !processed {
                            // No task available, wait a bit
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                    Err(e) => {
                        error!("Worker {} error: {}", worker_id, e);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            } else {
                // Redis not available, sleep and retry
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }

    #[cfg(feature = "redis")]
    async fn process_next_task(&self, conn: &Arc<Mutex<MultiplexedConnection>>) -> Result<bool> {
        let mut conn = conn.lock().await;
        
        // Use BRPOP with timeout
        let result: Option<(String, String)> = conn.brpop(&self.queue_name, 1.0).await?;
        
        if let Some((_queue, task_json)) = result {
            drop(conn); // Release lock before processing
            
            let task: serde_json::Value = serde_json::from_str(&task_json)?;
            
            match self.execute_task_json(&task).await {
                Ok(_) => {
                    info!("Task completed successfully");
                }
                Err(e) => {
                    error!("Task execution failed: {}. Task: {}", e, task_json);
                    // Could implement retry logic here
                    self.handle_failed_task(&task, e).await?;
                }
            }
            
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn handle_failed_task(&self, task: &serde_json::Value, error: anyhow::Error) -> Result<()> {
        // Extract retry count
        let retry_count = task.get("retry_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        
        if retry_count < self.max_retries {
            // Increment retry count and re-enqueue
            let mut task_with_retry = task.clone();
            if let Some(obj) = task_with_retry.as_object_mut() {
                obj.insert("retry_count".to_string(), serde_json::json!(retry_count + 1));
                obj.insert("last_error".to_string(), serde_json::json!(error.to_string()));
                obj.insert("retry_at".to_string(), serde_json::json!(chrono::Utc::now().to_rfc3339()));
            }
            
            warn!("Retrying task (attempt {}/{})", retry_count + 1, self.max_retries);
            
            // Re-enqueue with exponential backoff
            let delay = 2_u64.pow(retry_count);
            self.schedule(&task_with_retry, delay).await?;
        } else {
            error!("Task failed after {} retries. Moving to dead letter queue.", self.max_retries);
            self.move_to_dead_letter_queue(task).await?;
        }
        
        Ok(())
    }

    #[cfg(feature = "redis")]
    async fn move_to_dead_letter_queue(&self, task: &serde_json::Value) -> Result<()> {
        if let Some(conn) = &self.redis_conn {
            let dead_queue = format!("{}:dead", self.queue_name);
            let task_json = serde_json::to_string(task)?;
            
            let mut conn = conn.lock().await;
            conn.lpush(&dead_queue, task_json).await?;
            
            info!("Task moved to dead letter queue");
        }
        
        Ok(())
    }

    #[cfg(not(feature = "redis"))]
    async fn move_to_dead_letter_queue(&self, task: &serde_json::Value) -> Result<()> {
        error!("Task failed permanently (no dead letter queue available): {}", 
            serde_json::to_string(task)?);
        Ok(())
    }

    #[cfg(feature = "redis")]
    pub async fn schedule<T: Serialize>(
        &self,
        task: &T,
        delay_seconds: u64,
    ) -> Result<()> {
        if let Some(conn) = &self.redis_conn {
            let task_json = serde_json::to_string(task)?;
            let delayed_queue = format!("{}:delayed", self.queue_name);
            
            // Store in sorted set with timestamp as score
            let timestamp = chrono::Utc::now().timestamp() + delay_seconds as i64;
            
            let mut conn = conn.lock().await;
            conn.zadd(&delayed_queue, &task_json, timestamp).await?;
            
            info!("Task scheduled for execution in {} seconds", delay_seconds);
        } else {
            warn!("Redis not available, cannot schedule task");
        }
        
        Ok(())
    }

    #[cfg(not(feature = "redis"))]
    pub async fn schedule<T: Serialize>(
        &self,
        task: &T,
        delay_seconds: u64,
    ) -> Result<()> {
        warn!("Redis not available, executing task after delay");
        tokio::time::sleep(Duration::from_secs(delay_seconds)).await;
        self.execute_task_json(&serde_json::to_value(task)?).await?;
        Ok(())
    }

    #[cfg(feature = "redis")]
    pub async fn process_delayed_tasks(&self) -> Result<usize> {
        if let Some(conn) = &self.redis_conn {
            let delayed_queue = format!("{}:delayed", self.queue_name);
            let now = chrono::Utc::now().timestamp();
            
            let mut conn = conn.lock().await;
            
            // Get tasks that are due
            let tasks: Vec<String> = conn.zrangebyscore(&delayed_queue, 0, now).await?;
            
            if !tasks.is_empty() {
                // Remove from delayed queue
                for task in &tasks {
                    conn.zrem(&delayed_queue, task).await?;
                }
                
                // Add to main queue
                for task in &tasks {
                    conn.lpush(&self.queue_name, task).await?;
                }
                
                info!("Processed {} delayed tasks", tasks.len());
                Ok(tasks.len())
            } else {
                Ok(0)
            }
        } else {
            Ok(0)
        }
    }

    #[cfg(not(feature = "redis"))]
    pub async fn process_delayed_tasks(&self) -> Result<usize> {
        Ok(0)
    }

    #[cfg(feature = "redis")]
    pub async fn get_queue_stats(&self) -> Result<QueueStats> {
        if let Some(conn) = &self.redis_conn {
            let mut conn = conn.lock().await;
            
            let pending: usize = conn.llen(&self.queue_name).await?;
            let delayed: usize = conn.zcard(&format!("{}:delayed", self.queue_name)).await?;
            let dead: usize = conn.llen(&format!("{}:dead", self.queue_name)).await?;
            
            Ok(QueueStats {
                pending,
                delayed,
                dead,
            })
        } else {
            Ok(QueueStats::default())
        }
    }

    #[cfg(not(feature = "redis"))]
    pub async fn get_queue_stats(&self) -> Result<QueueStats> {
        Ok(QueueStats::default())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueueStats {
    pub pending: usize,
    pub delayed: usize,
    pub dead: usize,
}

#[async_trait]
pub trait Task: Send + Sync {
    async fn execute(&self) -> Result<()>;
    
    fn retry_count(&self) -> u32 {
        0
    }
    
    fn max_retries(&self) -> u32 {
        3
    }
    
    fn should_retry(&self, _error: &anyhow::Error) -> bool {
        self.retry_count() < self.max_retries()
    }
    
    fn retry_delay(&self) -> Duration {
        Duration::from_secs(2_u64.pow(self.retry_count()))
    }
}

// Example task implementations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailTask {
    #[serde(rename = "type")]
    pub task_type: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub retry_count: u32,
}

impl EmailTask {
    pub fn new(to: String, subject: String, body: String) -> Self {
        Self {
            task_type: "email".to_string(),
            to,
            subject,
            body,
            attempts: 0,
            retry_count: 0,
        }
    }
}

#[async_trait]
impl Task for EmailTask {
    async fn execute(&self) -> Result<()> {
        info!("Sending email to {}: {}", self.to, self.subject);
        
        // Simulate email sending
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        // In production, use a library like lettre:
        // let email = Message::builder()
        //     .to(self.to.parse()?)
        //     .subject(&self.subject)
        //     .body(self.body.clone())?;
        // 
        // let mailer = SmtpTransport::relay("smtp.example.com")?
        //     .credentials(Credentials::new("user".to_string(), "pass".to_string()))
        //     .build();
        // 
        // mailer.send(&email)?;
        
        info!("Email sent successfully to {}", self.to);
        Ok(())
    }
    
    fn retry_count(&self) -> u32 {
        self.retry_count
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationTask {
    #[serde(rename = "type")]
    pub task_type: String,
    pub user_id: uuid::Uuid,
    pub message: String,
    pub channel: String, // "push", "sms", "email"
    #[serde(default)]
    pub retry_count: u32,
}

impl NotificationTask {
    pub fn new(user_id: uuid::Uuid, message: String, channel: String) -> Self {
        Self {
            task_type: "notification".to_string(),
            user_id,
            message,
            channel,
            retry_count: 0,
        }
    }
}

#[async_trait]
impl Task for NotificationTask {
    async fn execute(&self) -> Result<()> {
        info!("Sending {} notification to user {}: {}", 
            self.channel, self.user_id, self.message);
        
        match self.channel.as_str() {
            "push" => {
                // Send push notification
                info!("Push notification sent");
            }
            "sms" => {
                // Send SMS
                info!("SMS sent");
            }
            "email" => {
                // Send email notification
                info!("Email notification sent");
            }
            _ => {
                warn!("Unknown notification channel: {}", self.channel);
            }
        }
        
        Ok(())
    }
    
    fn retry_count(&self) -> u32 {
        self.retry_count
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataCleanupTask {
    #[serde(rename = "type")]
    pub task_type: String,
    pub table: String,
    pub older_than_days: i64,
    #[serde(default)]
    pub retry_count: u32,
}

impl DataCleanupTask {
    pub fn new(table: String, older_than_days: i64) -> Self {
        Self {
            task_type: "data_cleanup".to_string(),
            table,
            older_than_days,
            retry_count: 0,
        }
    }
}

#[async_trait]
impl Task for DataCleanupTask {
    async fn execute(&self) -> Result<()> {
        info!("Cleaning up data from table {} older than {} days", 
            self.table, self.older_than_days);
        
        // In production, execute cleanup query:
        // sqlx::query("DELETE FROM ? WHERE created_at < NOW() - INTERVAL ? DAY")
        //     .bind(&self.table)
        //     .bind(self.older_than_days)
        //     .execute(pool)
        //     .await?;
        
        info!("Data cleanup completed for table {}", self.table);
        Ok(())
    }
    
    fn retry_count(&self) -> u32 {
        self.retry_count
    }
}

// Health check task for monitoring
pub async fn health_check_task(runner: &TaskRunner) -> Result<()> {
    let task = serde_json::json!({
        "type": "health_check",
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    
    runner.enqueue(&task).await?;
    Ok(())
}

// Delayed task processor - should be run periodically (e.g., every minute)
#[cfg(feature = "redis")]
pub async fn start_delayed_task_processor(runner: TaskRunner) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!("Starting delayed task processor");
        
        loop {
            match runner.process_delayed_tasks().await {
                Ok(count) if count > 0 => {
                    info!("Processed {} delayed tasks", count);
                }
                Ok(_) => {
                    // No tasks processed, continue
                }
                Err(e) => {
                    error!("Error processing delayed tasks: {}", e);
                }
            }
            
            // Check every 10 seconds
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    })
}

#[cfg(not(feature = "redis"))]
pub async fn start_delayed_task_processor(runner: TaskRunner) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!("Delayed task processor not available (Redis feature not enabled)");
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    })
}
