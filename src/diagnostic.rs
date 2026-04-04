use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceSpan {
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Diagnostic {
    pub id: String,
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub span: SourceSpan,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn error(
        id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
        span: SourceSpan,
    ) -> Self {
        Self {
            id: id.into(),
            severity: Severity::Error,
            code: code.into(),
            message: message.into(),
            span,
            notes: Vec::new(),
        }
    }

    pub fn warning(
        id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
        span: SourceSpan,
    ) -> Self {
        Self {
            id: id.into(),
            severity: Severity::Warning,
            code: code.into(),
            message: message.into(),
            span,
            notes: Vec::new(),
        }
    }
}

pub fn stable_fingerprint(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
