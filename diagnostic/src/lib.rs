//! Stable Vex error categories. Messages are explanatory, never classifiers.
use std::fmt;
pub mod output;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Category {
    Internal,
    Usage,
    Resolution,
    Compiler,
    Environment,
    Timeout,
    Cancelled,
}
impl Category {
    pub fn code(self) -> i32 {
        match self {
            Self::Internal => 1,
            Self::Usage => 2,
            Self::Resolution => 3,
            Self::Compiler => 4,
            Self::Environment => 5,
            Self::Timeout => 124,
            Self::Cancelled => 130,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Usage => "usage",
            Self::Resolution => "resolution",
            Self::Compiler => "compiler",
            Self::Environment => "environment",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub category: Category,
    message: String,
    pub context_fields: Vec<(String, String)>,
    pub cause: Option<Box<Error>>,
}
impl Error {
    pub fn new(category: Category, message: impl fmt::Display) -> Self {
        Self {
            category,
            message: message.to_string(),
            context_fields: Vec::new(),
            cause: None,
        }
    }
    pub fn internal(message: impl fmt::Display) -> Self {
        Self::new(Category::Internal, message)
    }
    pub fn usage(message: impl fmt::Display) -> Self {
        Self::new(Category::Usage, message)
    }
    pub fn resolution(message: impl fmt::Display) -> Self {
        Self::new(Category::Resolution, message)
    }
    pub fn compiler(message: impl fmt::Display) -> Self {
        Self::new(Category::Compiler, message)
    }
    pub fn environment(message: impl fmt::Display) -> Self {
        Self::new(Category::Environment, message)
    }
    pub fn context(self, message: impl fmt::Display) -> Self {
        let mut outer = Self::new(self.category, format!("{message}: {self}"));
        outer.cause = Some(Box::new(self));
        outer
    }
    pub fn with_field(mut self, key: impl Into<String>, value: impl fmt::Display) -> Self {
        self.context_fields.push((key.into(), value.to_string()));
        self
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_deref().map(|e| e as &dyn std::error::Error)
    }
}
impl AsRef<str> for Error {
    fn as_ref(&self) -> &str {
        &self.message
    }
}
impl std::ops::Deref for Error {
    type Target = str;
    fn deref(&self) -> &str {
        &self.message
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::environment(error)
    }
}
