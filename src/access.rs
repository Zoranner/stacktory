use std::env;

use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use thiserror::Error;

use crate::AccessConfig;

#[derive(Debug)]
pub struct AccessGate {
    expected_authorization: Option<HeaderValue>,
}

#[derive(Debug, Error)]
pub enum AccessError {
    #[error("read token environment variable `{0}` is not available")]
    MissingReadToken(String),
    #[error("read token from environment variable `{0}` is empty")]
    EmptyReadToken(String),
    #[error("read token from environment variable `{0}` cannot be used in an HTTP header")]
    InvalidReadToken(String),
}

impl AccessGate {
    pub fn from_config(config: &AccessConfig) -> Result<Self, AccessError> {
        if config.allow_anonymous {
            return Ok(Self::anonymous());
        }

        let variable = config
            .read_token_env
            .as_deref()
            .expect("validated configuration must contain read_token_env");
        let token =
            env::var(variable).map_err(|_| AccessError::MissingReadToken(variable.to_owned()))?;
        if token.is_empty() {
            return Err(AccessError::EmptyReadToken(variable.to_owned()));
        }
        Self::try_with_bearer_token(&token)
            .map_err(|_| AccessError::InvalidReadToken(variable.to_owned()))
    }

    pub fn anonymous() -> Self {
        Self {
            expected_authorization: None,
        }
    }

    pub fn with_bearer_token(token: &str) -> Self {
        Self::try_with_bearer_token(token).expect("test bearer token must be valid")
    }

    pub fn authorizes(&self, headers: &HeaderMap) -> bool {
        match &self.expected_authorization {
            Some(expected) => headers
                .get(AUTHORIZATION)
                .is_some_and(|actual| actual == expected),
            None => true,
        }
    }

    fn try_with_bearer_token(token: &str) -> Result<Self, axum::http::header::InvalidHeaderValue> {
        let expected_authorization = HeaderValue::from_str(&format!("Bearer {token}"))?;
        Ok(Self {
            expected_authorization: Some(expected_authorization),
        })
    }
}
