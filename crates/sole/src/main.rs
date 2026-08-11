use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut lang: Option<sole::Lang> = None;
    let mut path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--lang" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("error: `--lang` requires a value (en | zh)");
                    return ExitCode::from(2);
                };
                match sole::Lang::parse(value) {
                    Some(l) => {
                        lang = Some(l);
                        i += 2;
                    }
                    None => {
                        eprintln!("error: unknown language `{}` (supported: en, zh)", value);
                        return ExitCode::from(2);
                    }
                }
            }
            "--version" | "-V" => {
                println!("sole {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "run" => {
                i += 1;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown argument `{}`", other);
                return ExitCode::from(2);
            }
            other => {
                if path.is_some() {
                    eprintln!("error: unexpected extra argument `{}`", other);
                    return ExitCode::from(2);
                }
                path = Some(other.to_string());
                i += 1;
            }
        }
    }
    if let Some(l) = lang {
        sole::set_lang(l);
    }
    match path {
        Some(path) => {
            let source = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: cannot read {}: {}", path, e);
                    return ExitCode::from(2);
                }
            };
            match sole::run_source(&source) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    eprintln!("error: {}", msg);
                    ExitCode::from(1)
                }
            }
        }
        None => {
            eprintln!(
                "Sole {} — an AI-friendly programming language",
                env!("CARGO_PKG_VERSION")
            );
            eprintln!("usage:");
            eprintln!("  sole run [--lang en|zh] <file>    run a .sole script");
            eprintln!("  sole --version                    print version");
            eprintln!("(error messages default to English; SOLE_LANG env var also works)");
            ExitCode::from(2)
        }
    }
}
