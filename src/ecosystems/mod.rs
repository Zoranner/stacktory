//! 各生态的协议适配器。
//!
//! 每个子模块只负责一种制品协议的路径解析、上游响应校验和本地缓存键，
//! 公共的来源访问、存储布局、审计和运行日志由 crate 根下的平台模块提供。

pub mod cargo;
pub mod docker;
pub mod npm;
pub mod nuget;
pub mod pypi;
pub mod rustup;
pub mod upm;
pub mod vscode;
pub mod winget;
