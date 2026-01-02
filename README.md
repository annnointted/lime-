# Lime - Minimalist Backend Engine for Rapid Service Creation

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org)
[![Async](https://img.shields.io/badge/async-tokio-blueviolet.svg)](https://tokio.rs)


Lime is a minimalist Rust backend framework designed to eliminate boilerplate and complexity when starting new backend services. It provides the essential building blocks for 80% of what 80% of backend services need, with 20% of the code.

## Primary Function

Lime solves the excessive boilerplate problem by providing:

- **Ready-to-use HTTP server** with async/await support
- **Built-in configuration management** (TOML/YAML/ENV)
- **Database layer** with PostgreSQL support and connection pooling
- **JSON-first API** with `serde` integration
- **Authentication core** with JWT and RBAC
- **Simple task queue** for background jobs
- **Health checks & monitoring** endpoints
- **Structured logging** with `tracing`


## Target Audience

Lime is perfect for:
- **Startups** needing to prototype quickly
- **Indie developers** building side projects
- **Small-to-medium production services** (not enterprise-scale)
- **Developers tired of writing the same boilerplate**

## Quick Start

### Installation

Add to your `Cargo.toml`:
```toml
[dependencies]
lime = "0.1"
```

### Create Your First Service (15 minutes)

```rust
use lime::{Lime, Config, db_model};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize with config
    let config = Config::from_file("config.toml")?;
    
    // Build your app
    let mut app = Lime::new().with_config(config.clone())?;
    
    // Add components
    if config.database_configured() {
        app = app.with_db_pool().await?;
    }
    
    // Define routes
    app.get("/health", |_req| async {
        lime::api::health_handler().await
    })
    .get("/users/:id", get_user)
    .post("/users", create_user)
    .run()
    .await?;
    
    Ok(())
}

// Define a model
db_model! {
    User {
        email: String,
        name: String,
        is_active: bool,
    }
}

// Handler with database access
async fn get_user(mut request: lime::Request) -> lime::Response {
    let user_id = match request.param("id") {
        Some(id) => id,
        None => return lime::Response::bad_request("Missing user ID"),
    };

    let pool = request.db_pool().unwrap();
    let user_id = Uuid::parse_str(user_id).unwrap();
    
    match User::find_by_id(pool.get(), user_id).await {
        Ok(Some(user)) => lime::Response::json(hyper::StatusCode::OK, &user),
        Ok(None) => lime::Response::not_found("User not found"),
        Err(e) => lime::Response::internal_error(&format!("Error: {}", e)),
    }
}
```

### Configuration (`config.toml`)
```toml
[server]
host = "0.0.0.0"
port = 3000
workers = 1

[database]
host = "localhost"
port = 5432
username = "postgres"
password = "postgres"
database = "mydb"

[auth]
jwt_secret = "your-secret-here"

[app]
name = "my-service"
environment = "development"
```

## Key Features

### 1. HTTP Server with Router
- Async/await with Tokio runtime
- Path parameter parsing (`/users/:id`)
- Query string parsing
- Request/Response abstractions

### 2. Configuration System
- TOML/YAML/ENV multi-source loading
- Environment-specific configs (dev/staging/prod)
- Hot-reload capability

### 3. Database Layer
- PostgreSQL via `sqlx`
- Connection pooling (simple setup)
- ORM-like wrapper for common CRUD
- Basic migrations

### 4. Authentication Core
- JWT token handling (verify/sign)
- Simple RBAC middleware
- Password hashing with Argon2

### 5. Task Queue
- Background job execution
- Redis-backed or in-memory
- Retry logic with exponential backoff

### 6. Health & Monitoring
- `/health` endpoint with DB connectivity test
- Metrics endpoint
- Structured logging with `tracing`

## Performance

- **Setup Time**: < 15 minutes for new service
- **Overhead**: < 50ms vs raw `hyper`
- **Memory**: < 50MB idle
- **Throughput**: 1k RPS on 2GB RAM/1 CPU
- **Dependencies**: < 20 crates

## What Lime Does NOT Include

- GraphQL server
- WebSocket support
- gRPC/protobuf
- API documentation generator
- Admin dashboard
- Multiple database support (PostgreSQL only)
- Complex caching layers

## Documentation

- [Getting Started Guide](docs/getting-started.md) (coming soon)
- [API Reference](docs/api-reference.md) (coming soon)
- [Deployment Guide](docs/deployment.md) (coming soon)
- [Migration Path](docs/migration.md) (coming soon)

## Docker Deployment

```dockerfile
FROM rust:1.70-slim as builder
RUN apt-get update && apt-get install -y pkg-config libssl-dev
WORKDIR /usr/src/lime
COPY . .
RUN cargo build --release

FROM debian:bullseye-slim
RUN apt-get update && apt-get install -y ca-certificates libssl1.1
COPY --from=builder /usr/src/lime/target/release/lime /usr/local/bin/lime
COPY config.toml /etc/lime/config.toml
EXPOSE 3000
CMD ["lime"]
```

## Contributing

Lime is **open for contributions**! We welcome:

- **Bug reports** via GitHub Issues
- **Feature requests** 
- **Documentation improvements**
- **Code contributions** via Pull Requests

### Contribution Guidelines

1. **Fork** the repository
2. **Create a feature branch** (`git checkout -b feature/amazing-feature`)
3. **Commit your changes** (`git commit -m 'Add amazing feature'`)
4. **Push to the branch** (`git push origin feature/amazing-feature`)
5. **Open a Pull Request**

### Development Setup

```bash
# Clone repository
git clone https://github.com/yourusername/lime.git
cd lime

# Build
cargo build

# Run tests
cargo test

# Run examples
cargo run --example basic_server
```

### Code Standards
- **No `unwrap()`/`expect()`** in main code paths
- **Zero unsafe code** unless absolutely necessary
- **100% test coverage** of critical paths
- **Follow Rust API Guidelines**

## License

Lime is licensed under the **GNU General Public License v3.0**.

```
Copyright (C) 2024 Lime Contributors

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program.  If not, see <https://www.gnu.org/licenses/>.
```

## Success Metrics

Lime succeeds when:
- You can **start a new backend service in < 15 minutes**
- Your **codebase is 80% smaller** than equivalent services
- You **never write database connection boilerplate again**
- You can **deploy to production in under an hour**
- You have a **clear upgrade path** when you outgrow Lime

## Acknowledgments

- Built with Rust
- Inspired by Express.js's simplicity
- Powered by Tokio, Hyper, and SQLx
- Thanks to all contributors
