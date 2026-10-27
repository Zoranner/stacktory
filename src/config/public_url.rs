//! 客户端可见的公开地址列表。
//!
//! 第一项是默认地址；其余地址用于同一进程同时服务多个入口协议或域名。

use serde::{Deserialize, Deserializer, de};
use url::Url;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicUrls {
    urls: Vec<Url>,
}

impl PublicUrls {
    pub fn from_primary(url: Url) -> Self {
        Self { urls: vec![url] }
    }

    pub fn with_aliases(mut self, aliases: impl IntoIterator<Item = Url>) -> Self {
        self.urls.extend(aliases);
        self
    }

    pub(crate) fn from_vec(urls: Vec<Url>) -> Self {
        debug_assert!(!urls.is_empty(), "public URL list must not be empty");
        Self { urls }
    }

    pub fn primary(&self) -> &Url {
        self.urls
            .first()
            .expect("public URL list must not be empty")
    }

    pub fn iter(&self) -> impl Iterator<Item = &Url> {
        self.urls.iter()
    }

    /// 用于启动标记和日志的稳定文本；顺序变化视为地址变化。
    pub fn as_config_value(&self) -> String {
        self.urls
            .iter()
            .map(Url::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl From<Url> for PublicUrls {
    fn from(url: Url) -> Self {
        Self::from_primary(url)
    }
}

impl<'de> Deserialize<'de> for PublicUrls {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(Url),
            Many(Vec<Url>),
        }

        match Raw::deserialize(deserializer)? {
            Raw::One(url) => Ok(Self::from_primary(url)),
            Raw::Many(urls) if !urls.is_empty() => Ok(Self::from_vec(urls)),
            Raw::Many(_) => Err(de::Error::custom(
                "server.public_url must contain at least one URL",
            )),
        }
    }
}
