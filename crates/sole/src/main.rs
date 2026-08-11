use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("run") => {
            let Some(path) = args.get(2) else {
                eprintln!("用法: sole run <文件>");
                return ExitCode::from(2);
            };
            let source = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("无法读取 {}: {}", path, e);
                    return ExitCode::from(2);
                }
            };
            match sole::run_source(&source) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    eprintln!("错误: {}", msg);
                    ExitCode::from(1)
                }
            }
        }
        Some("--version") | Some("-V") => {
            println!("sole {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("Sole {} — AI 友好的编程语言", env!("CARGO_PKG_VERSION"));
            eprintln!("用法:");
            eprintln!("  sole run <文件>   运行 .sole 脚本");
            eprintln!("  sole --version    显示版本");
            ExitCode::from(2)
        }
    }
}
