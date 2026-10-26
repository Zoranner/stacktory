//! 进程入口：只解析命令行参数，启动流程和运维命令分别在 `startup`、`commands`。

use stacktory::{commands, startup};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None => startup::serve().await,
        Some("prune-legacy-layout") => commands::prune_legacy_layout(arguments),
        Some("invalidate-nuget-metadata") => commands::invalidate_nuget_metadata(arguments),
        Some(command) => {
            eprintln!("Stacktory: unknown command `{command}`");
            eprintln!(
                "usage: stacktory [prune-legacy-layout [--dry-run] | invalidate-nuget-metadata --entry <name> [--dry-run]]"
            );
            std::process::exit(2);
        }
    }
}
