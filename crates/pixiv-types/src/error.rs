//! Unified error type for every layer that talks to Pixiv.

use std::time::Duration;

use thiserror::Error;

/// Everything that can go wrong between this application and Pixiv.
#[derive(Debug, Error)]
pub enum PixivError {
    /// The request never completed: DNS, TLS, timeout, connection reset.
    #[error("网络请求失败：{0}")]
    Network(String),

    /// A non-success HTTP status.
    #[error("服务器返回 {status}：{message}")]
    Http { status: u16, message: String },

    /// No usable session cookie is configured.
    ///
    /// Callers must surface this as "please sign in" rather than retrying, and
    /// must never substitute locally invented data for it.
    #[error("需要登录 Pixiv 才能执行此操作")]
    Unauthorized,

    /// Pixiv replied with `{"error": true, "message": "..."}`.
    #[error("Pixiv 接口错误：{0}")]
    Api(String),

    /// The server asked us to slow down.
    #[error("请求过于频繁，请在 {0:?} 后重试")]
    RateLimited(Duration),

    /// The payload did not match the expected shape.
    #[error("响应解析失败：{0}")]
    Decode(String),

    /// The requested resource does not exist or is not visible to this session.
    #[error("找不到资源：{0}")]
    NotFound(String),

    /// Local I/O failure (cache, database, downloads).
    #[error("本地 I/O 失败：{0}")]
    Io(String),

    /// The operation was cancelled by the user or by a superseding request.
    #[error("操作已取消")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

impl PixivError {
    /// Build an [`PixivError::Http`].
    pub fn http(status: u16, message: impl Into<String>) -> Self {
        Self::Http {
            status,
            message: message.into(),
        }
    }

    /// True when retrying the same request could plausibly succeed.
    ///
    /// [`PixivError::Unauthorized`] and [`PixivError::NotFound`] are excluded on
    /// purpose: retrying them only burns quota and delays the error reaching the
    /// user.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network(_) => true,
            Self::RateLimited(_) => true,
            Self::Http { status, .. } => *status == 429 || *status >= 500,
            Self::Api(_) | Self::Decode(_) | Self::Io(_) => false,
            Self::Unauthorized | Self::NotFound(_) | Self::Cancelled | Self::Other(_) => false,
        }
    }
}

impl From<std::io::Error> for PixivError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::PixivError;

    #[test]
    fn retryability_follows_the_documented_policy() {
        assert!(PixivError::Network("reset".into()).is_retryable());
        assert!(PixivError::http(503, "unavailable").is_retryable());
        assert!(PixivError::http(429, "slow down").is_retryable());
        assert!(!PixivError::Unauthorized.is_retryable());
        assert!(!PixivError::http(403, "forbidden").is_retryable());
    }
}
