//! INV-1: presentation stays at the CLI edge.

use std::path::Path;

const DOMAIN_DIRS: [&str; 4] = ["src/sync", "src/config", "src/projection", "src/source"];
const BANNED_CRATES: [&str; 4] = ["indicatif", "anstream", "owo_colors", "console"];
const BANNED_MACROS: [&str; 4] = ["println!", "eprintln!", "print!", "eprint!"];

fn domain_sources() -> Vec<std::path::PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in DOMAIN_DIRS {
        let mut pending = vec![root.join(dir)];
        while let Some(path) = pending.pop() {
            for entry in std::fs::read_dir(&path).expect("read domain dir") {
                let entry = entry.expect("read domain entry");
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
    }
    assert!(!files.is_empty(), "domain source scan found no files");
    files
}

#[test]
fn the_domain_imports_no_presentation_crate() {
    let mut offenders = Vec::new();
    for file in domain_sources() {
        let text = std::fs::read_to_string(&file).expect("read domain source");
        for (number, line) in text.lines().enumerate() {
            let Some(rest) = line.trim_start().strip_prefix("use ") else {
                continue;
            };
            for krate in BANNED_CRATES {
                if rest.starts_with(&format!("{krate}::")) || rest.trim_end() == format!("{krate};")
                {
                    offenders.push(format!(
                        "{}:{}: {}",
                        file.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "presentation crates must stay at the CLI edge:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_domain_writes_to_no_terminal() {
    let mut offenders = Vec::new();
    for file in domain_sources() {
        let text = std::fs::read_to_string(&file).expect("read domain source");
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("///") {
                continue;
            }
            for macro_name in BANNED_MACROS {
                if code.contains(macro_name) {
                    offenders.push(format!("{}:{}: {}", file.display(), number + 1, code));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the domain reports through ProgressSink, never a print macro:\n{}",
        offenders.join("\n")
    );
}
