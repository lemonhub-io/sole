use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut lang: Option<sole::Lang> = None;
    let mut path: Option<String> = None;
    let mut cmd: Option<String> = None;
    let mut pos: Vec<String> = Vec::new();
    let mut fmt_check = false;
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
            "--check" => {
                if cmd.as_deref() == Some("fmt") {
                    fmt_check = true;
                    i += 1;
                } else {
                    eprintln!("error: unknown argument `--check`");
                    return ExitCode::from(2);
                }
            }
            "run" | "test" | "fmt" => {
                cmd = Some(args[i].clone());
                i += 1;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown argument `{}`", other);
                return ExitCode::from(2);
            }
            other => {
                if path.is_some() && cmd.as_deref() != Some("fmt") {
                    eprintln!("error: unexpected extra argument `{}`", other);
                    return ExitCode::from(2);
                }
                if path.is_none() {
                    path = Some(other.to_string());
                }
                pos.push(other.to_string());
                i += 1;
            }
        }
    }
    if let Some(l) = lang {
        sole::set_lang(l);
    }
    if cmd.as_deref() == Some("fmt") {
        return fmt_main(&pos, fmt_check);
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
            match cmd.as_deref() {
                Some("test") => match sole::run_tests_at(&source, &path) {
                    Ok(results) => {
                        let mut failed = 0;
                        for (name, outcome) in &results {
                            match outcome {
                                Ok(()) => println!("PASS {}", name),
                                Err(e) => {
                                    failed += 1;
                                    println!("FAIL {}", name);
                                    eprintln!("  error: {}", e);
                                }
                            }
                        }
                        if failed > 0 || results.is_empty() {
                            ExitCode::from(1)
                        } else {
                            ExitCode::SUCCESS
                        }
                    }
                    Err(msg) => {
                        eprintln!("error: {}", msg);
                        ExitCode::from(1)
                    }
                },
                Some("fmt") => fmt_main(&pos, fmt_check),
                _ => match sole::run_file(&path) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(msg) => {
                        eprintln!("error: {}", msg);
                        ExitCode::from(1)
                    }
                },
            }
        }
        None => {
            eprintln!(
                "Sole {} — an AI-friendly programming language",
                env!("CARGO_PKG_VERSION")
            );
            eprintln!("usage:");
            eprintln!("  sole run [--lang en|zh] <file>    run a .sole script");
            eprintln!("  sole test [--lang en|zh] <file>   run the `test` blocks");
            eprintln!("  sole fmt [--check] <file|dir>...  format .sole files");
            eprintln!("  sole --version                    print version");
            eprintln!("(error messages default to English; SOLE_LANG env var also works)");
            ExitCode::from(2)
        }
    }
}

/// `sole fmt [--check] <path>...`: formats .sole files in place (or reports
/// when `--check` is given). Directories are walked recursively.
fn fmt_main(paths: &[String], check: bool) -> ExitCode {
    for a in paths {
        if a.starts_with('-') {
            eprintln!("error: unknown argument `{}`", a);
            return ExitCode::from(2);
        }
    }
    if paths.is_empty() {
        eprintln!("error: `sole fmt` needs at least one file or directory");
        return ExitCode::from(2);
    }
    let mut files = Vec::new();
    for p in paths {
        let path = std::path::Path::new(p);
        if path.is_dir() {
            collect_sole_files(path, &mut files);
        } else {
            files.push(p.clone());
        }
    }
    let mut any_diff = false;
    let mut any_err = false;
    for f in files {
        match std::fs::read_to_string(&f) {
            Ok(src) => match sole::fmt::format_source(&src) {
                Ok(formatted) => {
                    if formatted != src {
                        if check {
                            println!("would reformat {}", f);
                            any_diff = true;
                        } else {
                            if std::fs::write(&f, formatted).is_err() {
                                eprintln!("error: cannot write {}", f);
                                any_err = true;
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error: {}: {}", f, e);
                    any_err = true;
                }
            },
            Err(e) => {
                eprintln!("error: cannot read {}: {}", f, e);
                any_err = true;
            }
        }
    }
    if any_err || (check && any_diff) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn collect_sole_files(dir: &std::path::Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            collect_sole_files(&path, files);
        } else if path.extension().is_some_and(|x| x == "sole") {
            files.push(path.to_string_lossy().to_string());
        }
    }
}
