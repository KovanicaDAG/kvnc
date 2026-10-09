//! Rate limiting and authentication middleware for the RPC server.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{Extension, Request},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use parking_lot::RwLock;

/// Rate limiter configuration
///
/// A `requests_per_minute` of `0` disables rate limiting entirely (every
/// request is admitted); this is the documented `KVNC_RPC_RATE_LIMIT_PER_MIN=0`
/// escape hatch for tests and trusted local tooling.
#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    /// Requests per minute per IP. `0` disables rate limiting.
    pub requests_per_minute: u32,
    /// Burst allowance (token-bucket capacity)
    pub burst: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_minute: 60, // 1 req/sec
            burst: 10,
        }
    }
}

impl RateLimitConfig {
    /// Build a config from a requests-per-minute figure, deriving the burst.
    ///
    /// Burst rule: `burst = max(1, requests_per_minute / 6)`, preserving the
    /// historical default (`60` rpm -> `10` burst). `requests_per_minute == 0`
    /// disables rate limiting (burst is irrelevant and set to `0`).
    pub fn from_requests_per_minute(requests_per_minute: u32) -> Self {
        if requests_per_minute == 0 {
            return Self {
                requests_per_minute: 0,
                burst: 0,
            };
        }
        Self {
            requests_per_minute,
            burst: (requests_per_minute / 6).max(1),
        }
    }

    /// Whether rate limiting is disabled (`requests_per_minute == 0`).
    pub fn is_disabled(&self) -> bool {
        self.requests_per_minute == 0
    }
}

/// Token bucket for rate limiting
struct TokenBucket {
    capacity: u32,
    tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl TokenBucket {
    fn new(capacity: u32, refill_rate: f64) -> Self {
        Self {
            capacity,
            tokens: capacity as f64,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    fn consume(&mut self, tokens: u32) -> bool {
        self.refill();
        if self.tokens >= tokens as f64 {
            self.tokens -= tokens as f64;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity as f64);
        self.last_refill = now;
    }

    fn retry_after(&self) -> Option<u64> {
        if self.tokens >= 1.0 {
            return None;
        }
        let needed = 1.0 - self.tokens;
        let seconds = (needed / self.refill_rate).ceil() as u64;
        Some(seconds.max(1))
    }
}

/// Per-IP rate limiter state
struct IpRateLimiter {
    bucket: TokenBucket,
    last_seen: Instant,
}

use parking_lot::Mutex;

/// Global rate limiter state
pub struct RateLimiterState {
    limiters: RwLock<HashMap<SocketAddr, IpRateLimiter>>,
    config: RateLimitConfig,
    #[allow(dead_code)]
    cleanup_interval: Duration,
    last_cleanup: Mutex<Instant>,
}

impl RateLimiterState {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            limiters: RwLock::new(HashMap::new()),
            config,
            cleanup_interval: Duration::from_secs(300), // 5 minutes
            last_cleanup: Mutex::new(Instant::now()),
        }
    }

    /// Check if request is allowed, returns (allowed, retry_after_secs)
    pub fn check_limit(&self, addr: SocketAddr) -> (bool, Option<u64>) {
        // A configured rate of 0 disables rate limiting (admit everything).
        if self.config.is_disabled() {
            return (true, None);
        }

        let mut limiters = self.limiters.write();

        // Cleanup old entries periodically
        let now = Instant::now();
        let mut last_cleanup = self.last_cleanup.lock();
        if now.duration_since(*last_cleanup) > Duration::from_secs(300) {
            limiters.retain(|_, v| now.duration_since(v.last_seen) < Duration::from_secs(600));
            *last_cleanup = now;
        }
        drop(last_cleanup);

        let refill_rate = self.config.requests_per_minute as f64 / 60.0;
        let limiter = limiters.entry(addr).or_insert_with(|| IpRateLimiter {
            bucket: TokenBucket::new(self.config.burst, refill_rate),
            last_seen: Instant::now(),
        });

        limiter.last_seen = Instant::now();
        let allowed = limiter.bucket.consume(1);
        let retry_after = limiter.bucket.retry_after();
        (allowed, retry_after)
    }
}

/// List of write methods that require authentication
const WRITE_METHODS: &[&str] = &[
    "kvnc_sendRawTransaction",
    "htlc_create",
    "htlc_claim",
    "htlc_refund",
    "vault_create",
    "vault_claim",
    "vault_cancel",
    "multisig_create",
    "multisig_propose",
    "multisig_confirm",
    "multisig_execute",
    "token_create",
    "token_transfer",
    "token_mint",
    "token_burn",
];

/// Authentication configuration
#[derive(Clone, Debug)]
pub struct AuthConfig {
    /// Bearer tokens for write operations
    pub write_tokens: Vec<String>,
    /// Whether auth is required for write methods
    pub require_auth_for_writes: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            write_tokens: Vec::new(),
            require_auth_for_writes: true,
        }
    }
}

/// Rate limiting middleware
#[allow(dead_code)]
pub async fn rate_limit_middleware(
    Extension(rate_limiter): Extension<Arc<RateLimiterState>>,
    request: Request,
    next: Next,
) -> Response {
    let addr = request
        .extensions()
        .get::<SocketAddr>()
        .copied()
        .unwrap_or_else(|| "0.0.0.0:0".parse().unwrap());
    let (allowed, retry_after) = rate_limiter.check_limit(addr);

    if !allowed {
        let mut response = Response::new("Rate limit exceeded".into());
        *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
        if let Some(seconds) = retry_after {
            response
                .headers_mut()
                .insert("Retry-After", seconds.to_string().parse().unwrap());
        }
        return response;
    }

    next.run(request).await
}

/// Authentication middleware
#[allow(dead_code)]
pub async fn auth_middleware(
    Extension(auth_config): Extension<Arc<AuthConfig>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let method = request
        .uri()
        .path()
        .strip_prefix("/rpc")
        .unwrap_or(request.uri().path());

    // Check if this is a write method
    let is_write = WRITE_METHODS.iter().any(|m| method.contains(m));

    if is_write && auth_config.require_auth_for_writes {
        let auth_header = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));

        let token = match auth_header {
            Some(t) => t,
            None => {
                let mut response =
                    Response::new("Authentication required for write operations".into());
                *response.status_mut() = StatusCode::UNAUTHORIZED;
                response
                    .headers_mut()
                    .insert("WWW-Authenticate", "Bearer".parse().unwrap());
                return response;
            }
        };

        if !auth_config.write_tokens.contains(&token.to_string()) {
            let mut response = Response::new("Invalid authentication token".into());
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            return response;
        }
    }

    next.run(request).await
}

/// Combined middleware: rate limiting + auth
pub async fn combined_middleware(
    Extension(rate_limiter): Extension<Arc<RateLimiterState>>,
    Extension(auth_config): Extension<Arc<AuthConfig>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    // Rate limiting
    let addr = request
        .extensions()
        .get::<SocketAddr>()
        .copied()
        .unwrap_or_else(|| "0.0.0.0:0".parse().unwrap());
    let (allowed, retry_after) = rate_limiter.check_limit(addr);
    if !allowed {
        let mut response = Response::new("Rate limit exceeded".into());
        *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
        if let Some(seconds) = retry_after {
            response
                .headers_mut()
                .insert("Retry-After", seconds.to_string().parse().unwrap());
        }
        return response;
    }

    // Authentication for write methods
    let method = request
        .uri()
        .path()
        .strip_prefix("/rpc")
        .unwrap_or(request.uri().path());
    let is_write = WRITE_METHODS.iter().any(|m| method.contains(m));

    if is_write && auth_config.require_auth_for_writes {
        let auth_header = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));

        let token = match auth_header {
            Some(t) => t,
            None => {
                let mut response =
                    Response::new("Authentication required for write operations".into());
                *response.status_mut() = StatusCode::UNAUTHORIZED;
                response
                    .headers_mut()
                    .insert("WWW-Authenticate", "Bearer".parse().unwrap());
                return response;
            }
        };

        if !auth_config.write_tokens.contains(&token.to_string()) {
            let mut response = Response::new("Invalid authentication token".into());
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            return response;
        }
    }

    next.run(request).await
}
