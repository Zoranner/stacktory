//! 持久化底座：数据目录契约、原子写入、对象锁和访问时间。

pub mod access;
pub mod atomic;
pub mod layout;
pub mod locks;
