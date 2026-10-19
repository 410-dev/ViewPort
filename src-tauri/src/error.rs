use serde::Serialize;

/// Error returned to the UI. `code` is stable and localized by the frontend;
/// `detail` carries structured data such as versions or query error spans.
#[derive(Debug, Serialize)]
pub struct CmdError {
    pub code: &'static str,
    pub message: String,
    pub detail: serde_json::Value,
}

impl CmdError {
    pub fn new(code: &'static str, message: impl ToString) -> Self {
        Self { code, message: message.to_string(), detail: serde_json::Value::Null }
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = detail;
        self
    }

    /// Adds the bundle folder actually used (e.g. after extracting an archive) to the detail.
    pub fn with_path(mut self, path: &std::path::Path) -> Self {
        if self.detail.is_null() {
            self.detail = serde_json::json!({});
        }
        if let Some(obj) = self.detail.as_object_mut() {
            obj.insert("path".into(), serde_json::Value::String(path.display().to_string()));
        }
        self
    }
}

pub type CmdResult<T> = Result<T, CmdError>;

impl From<vp_bundle::BundleError> for CmdError {
    fn from(e: vp_bundle::BundleError) -> Self {
        use vp_bundle::BundleError as B;
        let code = match &e {
            B::NotABundle(_) => "not_a_bundle",
            B::AlreadyExists(_) => "already_exists",
            B::TooNew { .. } => "bundle_too_new",
            B::ArchiveNotSupported => "archive_not_supported",
            B::InvalidInfo(_) => "invalid_bundle",
            B::Io(_) => "io",
        };
        let detail = match &e {
            B::TooNew { found, supported } => serde_json::json!({ "found": found, "supported": supported }),
            _ => serde_json::Value::Null,
        };
        CmdError::new(code, &e).with_detail(detail)
    }
}

impl From<vp_db::DbError> for CmdError {
    fn from(e: vp_db::DbError) -> Self {
        match e {
            vp_db::DbError::Query(q) => q.into(),
            other => CmdError::new("database", other),
        }
    }
}

impl From<vp_query::QueryError> for CmdError {
    fn from(e: vp_query::QueryError) -> Self {
        let detail = serde_json::to_value(&e).unwrap_or_default();
        // The span is in `detail`; the message is just the problem.
        CmdError::new("query", &e.kind).with_detail(detail)
    }
}

impl From<vp_driver::DriverError> for CmdError {
    fn from(e: vp_driver::DriverError) -> Self {
        CmdError::new("driver", e)
    }
}

impl From<vp_crypto::CryptoError> for CmdError {
    fn from(e: vp_crypto::CryptoError) -> Self {
        let code = match e {
            vp_crypto::CryptoError::WrongKey => "wrong_key",
            vp_crypto::CryptoError::Corrupt => "corrupt",
            _ => "crypto",
        };
        CmdError::new(code, e)
    }
}

impl From<vp_db::rusqlite_error::Error> for CmdError {
    fn from(e: vp_db::rusqlite_error::Error) -> Self {
        CmdError::new("database", e)
    }
}

impl From<std::io::Error> for CmdError {
    fn from(e: std::io::Error) -> Self {
        CmdError::new("io", e)
    }
}

impl From<serde_json::Error> for CmdError {
    fn from(e: serde_json::Error) -> Self {
        CmdError::new("json", e)
    }
}

impl From<vp_shard::ShardError> for CmdError {
    fn from(e: vp_shard::ShardError) -> Self {
        use vp_shard::ShardError as S;
        let code = match &e {
            S::NotAShard(_) => "not_a_shard",
            S::AlreadyExists(_) => "already_exists",
            S::TooNew { .. } => "shard_too_new",
            S::Locked => "shard_locked",
            S::WrongKey => "wrong_key",
            S::ReadOnly => "read_only",
            S::NotFound(_) => "not_found",
            S::InvalidPath(_) | S::Invalid(_) => "invalid",
            S::Cancelled => "cancelled",
            S::Crypto(_) => "crypto",
            S::Io(_) => "io",
            S::Json(_) => "json",
        };
        CmdError::new(code, e)
    }
}
