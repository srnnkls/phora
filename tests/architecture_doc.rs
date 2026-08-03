use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

const DOD_ITEM_COUNT: u32 = 20;
const MIN_DISTINCT_TASKS: usize = 8;
const MIN_DISTINCT_TEST_FILES: usize = 5;
const SELF_TEST_FILE: &str = "tests/architecture_doc.rs";

fn doc_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/architecture.md")
}

fn read_doc() -> String {
    fs::read_to_string(doc_path()).unwrap_or_default()
}

fn squeeze(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

fn heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    Some(trimmed.chars().take_while(|&c| c == '#').count())
}

fn sections(doc: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in doc.lines() {
        if heading_level(line).is_some() {
            if !current.is_empty() {
                out.push(current.join("\n"));
            }
            current = vec![line];
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        out.push(current.join("\n"));
    }
    out
}

fn section_containing(doc: &str, pred: impl Fn(&str) -> bool) -> String {
    sections(doc)
        .into_iter()
        .find(|s| pred(s))
        .unwrap_or_default()
}

fn contains_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    haystack.match_indices(word).any(|(idx, _)| {
        let before_ok = idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric();
        let after = idx + word.len();
        let after_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
        before_ok && after_ok
    })
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

const NEGATORS: &[&str] = &["no", "not", "never", "without", "non", "cannot"];

fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

const NEGATION_WINDOW: usize = 5;
const NEGATION_BOUNDARIES: &[&str] = &["and", "but"];

fn negated_before(text: &str, idx: usize) -> bool {
    let before = &text[..idx];
    let boundary = before
        .rfind([',', '.', ':', ';', '\n'])
        .map_or(0, |p| p + 1);
    let toks: Vec<&str> = before[boundary..]
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    toks.iter()
        .rev()
        .take(NEGATION_WINDOW)
        .take_while(|t| !NEGATION_BOUNDARIES.contains(&t.to_ascii_lowercase().as_str()))
        .any(|t| NEGATORS.contains(&t.to_ascii_lowercase().as_str()))
}

fn positive_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(word).any(|(idx, _)| {
        let before_ok = idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric();
        let after = idx + word.len();
        let after_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
        before_ok && after_ok && !negated_before(text, idx)
    })
}

fn positive_phrase(text: &str, needle: &str) -> bool {
    text.match_indices(needle)
        .any(|(idx, _)| !negated_before(text, idx))
}

fn positive_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| positive_phrase(text, n))
}

const NO_IO_PHRASES: &[&str] = &[
    "i/o-free",
    "io-free",
    "no i/o",
    "no io",
    "i/o free",
    "io free",
    "free of i/o",
    "free of io",
    "without i/o",
    "without io",
];

fn states_projection_purity(lower: &str) -> bool {
    if contains_word(lower, "impure") {
        return false;
    }
    positive_word(lower, "pure") || positive_any(lower, NO_IO_PHRASES)
}

fn responsibility_matches(lower: &str, module: &str) -> bool {
    match module {
        "source" => {
            if positive_word(lower, "mutable") {
                return false;
            }
            positive_any(lower, &["obtain", "read", "fetch", "acquire", "retriev"])
                && (contains_word(lower, "immutable")
                    || contains_any(lower, &["content", "snapshot", "bytes"]))
        }
        "projection" => {
            states_projection_purity(lower)
                && contains_any(
                    lower,
                    &[
                        "calculat",
                        "comput",
                        "transform",
                        "deriv",
                        "desired",
                        "structure",
                        "deterministic",
                    ],
                )
        }
        "sync" => {
            positive_any(lower, &["sole", "owner", "own"])
                && (contains_word(lower, "target") || contains_word(lower, "machine"))
        }
        _ => false,
    }
}

fn unit_ties_module(unit: &str, module: &str) -> bool {
    let lower = unit.to_lowercase();
    contains_word(&lower, module) && responsibility_matches(&lower, module)
}

fn placement_units(region: &str) -> Vec<&str> {
    region.split(['\n', ';', '.']).collect()
}

fn placement_ties_each_module(region: &str) -> bool {
    let units = placement_units(region);
    let modules = ["source", "projection", "sync"];
    let ties: Vec<Vec<usize>> = modules
        .iter()
        .map(|m| {
            units
                .iter()
                .enumerate()
                .filter_map(|(i, unit)| unit_ties_module(unit, m).then_some(i))
                .collect()
        })
        .collect();
    for &a in &ties[0] {
        for &b in &ties[1] {
            if b == a {
                continue;
            }
            for &c in &ties[2] {
                if c != a && c != b {
                    return true;
                }
            }
        }
    }
    false
}

fn inv1_states_purity(region: &str) -> bool {
    region.split(['\n', '.', ';']).any(|clause| {
        let lower = clause.to_lowercase();
        contains_word(&lower, "projection") && states_projection_purity(&lower)
    })
}

fn inv2_states_prohibition(region: &str) -> bool {
    let prohibitions = [
        "no",
        "not",
        "never",
        "without",
        "forbid",
        "forbids",
        "forbidden",
        "cannot",
    ];
    let verbs = [
        "import",
        "imports",
        "importing",
        "know",
        "knows",
        "knowing",
        "reference",
        "references",
        "referencing",
        "depend",
        "depends",
        "depending",
    ];
    let nouns = ["target", "projection", "manifest", "registry", "template"];
    let adverbs = [
        "ever",
        "directly",
        "strictly",
        "simply",
        "then",
        "also",
        "actually",
        "currently",
        "longer",
        "freely",
    ];
    let bare_negators = ["no", "none", "nothing", "never"];
    region.split(['\n', '.', ';']).any(|clause| {
        let toks = tokens(clause);
        if !toks.iter().any(|t| nouns.contains(&t.as_str())) {
            return false;
        }
        let negator_governs_verb = toks.iter().enumerate().any(|(i, t)| {
            prohibitions.contains(&t.as_str())
                && toks[i + 1..]
                    .iter()
                    .find(|next| !adverbs.contains(&next.as_str()))
                    .is_some_and(|next| verbs.contains(&next.as_str()))
        });
        let verb_then_negator = toks
            .windows(2)
            .any(|w| verbs.contains(&w[0].as_str()) && bare_negators.contains(&w[1].as_str()));
        negator_governs_verb || verb_then_negator
    })
}

fn traceability_region(doc: &str) -> String {
    let mut level: Option<usize> = None;
    let mut region: Vec<&str> = Vec::new();
    for line in doc.lines() {
        match heading_level(line) {
            Some(lv) => match level {
                None => {
                    if line.to_lowercase().contains("traceab") {
                        level = Some(lv);
                    }
                }
                Some(open) if lv <= open => break,
                Some(_) => region.push(line),
            },
            None => {
                if level.is_some() {
                    region.push(line);
                }
            }
        }
    }
    region.join("\n")
}

fn table_rows(region: &str) -> Vec<Vec<String>> {
    region
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('|') && line.matches('|').count() >= 2)
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|cells| {
            !cells.iter().all(|cell| {
                !cell.is_empty()
                    && cell
                        .chars()
                        .all(|c| c == '-' || c == ':' || c.is_whitespace())
            })
        })
        .collect()
}

fn leading_number(cell: &str) -> Option<u32> {
    if cell
        .chars()
        .take_while(|c| !c.is_ascii_digit())
        .any(|c| c.is_alphabetic() && c != 'T' && c != '#')
    {
        return None;
    }
    let digits: String = cell
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

// `scopes/` is untracked, so the valid task-id space cannot be read at runtime.
fn valid_task_ids() -> BTreeSet<u32> {
    (1..=31).chain(32..=33).collect()
}

fn task_numbers(text: &str) -> Vec<u32> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if b[i] == b'T'
            && i + 3 < n
            && b[i + 1].is_ascii_digit()
            && b[i + 2].is_ascii_digit()
            && b[i + 3].is_ascii_digit()
        {
            let before_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
            let after = i + 4;
            let after_ok = after >= n || !b[after].is_ascii_alphanumeric();
            if before_ok && after_ok {
                let num = u32::from(b[i + 1] - b'0') * 100
                    + u32::from(b[i + 2] - b'0') * 10
                    + u32::from(b[i + 3] - b'0');
                out.push(num);
                i = after;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn rs_path_at(cell: &str, idx: usize) -> Option<String> {
    let bytes = cell.as_bytes();
    let end = idx + 3;
    if end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
        return None;
    }
    let is_path_byte =
        |c: u8| c.is_ascii_alphanumeric() || c == b'/' || c == b'_' || c == b'-' || c == b'.';
    let mut start = idx;
    while start > 0 && is_path_byte(bytes[start - 1]) {
        start -= 1;
    }
    let token = &cell[start..end];
    if token == ".rs" || token.starts_with('/') || token.contains("..") {
        return None;
    }
    if !token.starts_with("tests/") || token == SELF_TEST_FILE {
        return None;
    }
    Some(token.to_string())
}

fn rs_paths(cell: &str) -> Vec<String> {
    cell.match_indices(".rs")
        .filter_map(|(idx, _)| rs_path_at(cell, idx))
        .collect()
}

fn rs_path(cell: &str) -> Option<String> {
    rs_paths(cell).into_iter().next()
}

fn referenced_test_file(row: &[String]) -> Option<PathBuf> {
    for cell in row {
        if let Some(rel) = rs_path(cell) {
            return Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel));
        }
    }
    None
}

fn referenced_test_paths(row: &[String]) -> Vec<String> {
    row.iter().flat_map(|cell| rs_paths(cell)).collect()
}

fn traceability_rows_by_item() -> BTreeMap<u32, Vec<String>> {
    table_rows(&traceability_region(&read_doc()))
        .into_iter()
        .filter_map(|row| {
            let item = row.first().and_then(|cell| leading_number(cell))?;
            (1..=DOD_ITEM_COUNT).contains(&item).then_some((item, row))
        })
        .collect()
}

fn relevant_task_ids(item: u32) -> &'static [u32] {
    match item {
        1 => &[6, 11, 20, 31],
        2 => &[30],
        3 => &[7, 9],
        4 => &[5, 8, 9],
        5 => &[5, 13, 16, 29],
        6 => &[12],
        7 => &[13, 30],
        8 => &[15, 16],
        9 => &[8, 9, 20],
        10 => &[17, 18],
        11 => &[18],
        12 => &[20, 21],
        13 => &[18, 19],
        14 => &[17, 25],
        15 => &[22, 24],
        16 => &[27, 28],
        17 => &[2, 3, 4, 10, 16, 21, 24, 26, 28],
        18 => &[5, 31],
        19 | 20 => &[31],
        _ => &[],
    }
}

fn relevant_test_paths(item: u32) -> &'static [&'static str] {
    match item {
        1 | 2 | 19 => &["tests/final_architecture.rs"],
        3..=5 | 18 => &["tests/arch_check.rs"],
        6 => &[
            "tests/source_snapshot_contract.rs",
            "tests/source_snapshot_gate.rs",
            "tests/source_compat_contract.rs",
        ],
        7 => &["tests/source_layout.rs"],
        8 => &["tests/compat_staging.rs", "tests/stage_deletion_gate.rs"],
        9 => &[
            "tests/projection_contract.rs",
            "tests/projection_contract_gate.rs",
        ],
        10 | 11 | 13 => &[
            "tests/reconcile_matrix_contract.rs",
            "tests/reconcile_matrix_gate.rs",
        ],
        12 => &["tests/compat_serialized.rs", "tests/orchestration_gate.rs"],
        14 => &["tests/state_store_contract.rs", "tests/state_store_gate.rs"],
        15 => &[
            "tests/compat_recovery.rs",
            "tests/deploy_relocation_gate.rs",
        ],
        16 => &["tests/sync_request_contract.rs", "tests/compat_cli.rs"],
        17 | 20 => &["tests/compat_serialized.rs"],
        _ => &[],
    }
}

fn required_test_paths(item: u32) -> &'static [&'static str] {
    match item {
        17 => &[
            "tests/compat_serialized.rs",
            "tests/compat_recovery.rs",
            "tests/compat_cli.rs",
        ],
        20 => &[
            "tests/compat_serialized.rs",
            "tests/compat_staging.rs",
            "tests/compat_recovery.rs",
            "tests/compat_cli.rs",
        ],
        _ => &[],
    }
}

fn fenced_blocks(doc: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    let mut in_fence = false;
    for line in doc.lines() {
        if line.trim_start().starts_with("```") {
            if in_fence {
                blocks.push(current.join("\n"));
                current.clear();
            }
            in_fence = !in_fence;
        } else if in_fence {
            current.push(line);
        }
    }
    blocks
}

struct Traceability {
    covered: BTreeSet<u32>,
    distinct_valid_tasks: BTreeSet<u32>,
    distinct_test_files: BTreeSet<PathBuf>,
}

fn analyze_traceability() -> Traceability {
    let valid = valid_task_ids();
    let doc = read_doc();
    let region = traceability_region(&doc);
    let rows = table_rows(&region);

    let mut covered = BTreeSet::new();
    let mut distinct_valid_tasks = BTreeSet::new();
    let mut distinct_test_files = BTreeSet::new();
    for row in &rows {
        if row.len() < 3 {
            continue;
        }
        let Some(item) = row.first().and_then(|cell| leading_number(cell)) else {
            continue;
        };
        if !(1..=DOD_ITEM_COUNT).contains(&item) {
            continue;
        }
        let row_tasks: Vec<u32> = row
            .iter()
            .flat_map(|cell| task_numbers(cell))
            .filter(|n| valid.contains(n))
            .collect();
        let test_file = referenced_test_file(row).filter(|p| p.is_file());
        if let (false, Some(test_file)) = (row_tasks.is_empty(), test_file) {
            covered.insert(item);
            distinct_valid_tasks.extend(row_tasks);
            distinct_test_files.insert(test_file);
        }
    }
    Traceability {
        covered,
        distinct_valid_tasks,
        distinct_test_files,
    }
}

#[test]
fn architecture_doc_exists() {
    let path = doc_path();
    assert!(
        path.is_file(),
        "docs/architecture.md must exist at {}",
        path.display()
    );
}

#[test]
fn documents_source_projection_sync_decision() {
    let doc = read_doc();
    let section = section_containing(&doc, |s| {
        let sq = squeeze(s);
        sq.contains("source\u{2192}projection\u{2192}sync")
            || sq.contains("source->projection->sync")
    });
    assert!(
        !section.is_empty(),
        "docs/architecture.md must contain a section documenting the source \u{2192} projection \u{2192} sync capability chain"
    );
    assert!(
        section.to_lowercase().contains("decision"),
        "the section carrying the source \u{2192} projection \u{2192} sync chain must frame it as the architectural decision"
    );
}

#[test]
fn documents_inv1_projection_purity_placement() {
    let doc = read_doc();
    let region = section_containing(&doc, |s| s.contains("INV-1"));
    assert!(
        !region.is_empty(),
        "docs/architecture.md must name the INV-1 projection-purity invariant"
    );
    assert!(
        inv1_states_purity(&region),
        "the INV-1 region must state its substance in positive polarity: a single clause must tie \
         `projection` to a purity/no-I/O claim (e.g. \"projection performs no I/O\" or \"projection \
         is a pure calculation\") \u{2014} \"projection is impure\" and \"projection is not pure / \
         performs I/O\" must not satisfy it"
    );
}

#[test]
fn documents_inv2_source_knows_no_target_types() {
    let doc = read_doc();
    let region = section_containing(&doc, |s| s.contains("INV-2"));
    assert!(
        !region.is_empty(),
        "docs/architecture.md must name the INV-2 source-boundary invariant"
    );
    assert!(
        region.to_lowercase().contains("source") && inv2_states_prohibition(&region),
        "the INV-2 region must state its substance as a prohibition: a single clause must pair a \
         prohibition word (no/not/never/without/forbid) with an import/reference verb and a \
         forbidden noun (projection/target/manifest/registry) \u{2014} the affirmative \
         \"source imports target types\" must not satisfy it"
    );
}

#[test]
fn documents_module_placement_rule() {
    let doc = read_doc();
    let region = section_containing(&doc, |s| s.to_lowercase().contains("placement"));
    assert!(
        !region.is_empty(),
        "docs/architecture.md must document the module-placement rule under a section naming `placement`"
    );
    assert!(
        placement_ties_each_module(&region),
        "the placement rule must tie each of `source`, `projection`, and `sync` to a responsibility on a distinct line, not merely name them"
    );
}

#[test]
fn t031_documents_the_capability_chain_in_an_architecture_diagram() {
    let has_diagram = fenced_blocks(&read_doc()).into_iter().any(|block| {
        let compact: String = block
            .to_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let source_to_projection = [
            "source->projection",
            "source-->projection",
            "source→projection",
            "source──►projection",
        ]
        .iter()
        .any(|edge| compact.contains(edge));
        let projection_to_sync = [
            "projection->sync",
            "projection-->sync",
            "projection→sync",
            "projection──►sync",
        ]
        .iter()
        .any(|edge| compact.contains(edge));
        source_to_projection && projection_to_sync
    });

    assert!(
        has_diagram,
        "docs/architecture.md must contain a fenced text or Mermaid architecture diagram with \
         directed source -> projection and projection -> sync edges; prose naming the modules is \
         not a durable substitute for the requested end-state diagram"
    );
}

#[test]
fn traceability_table_maps_every_dod_item_to_task_and_test() {
    let analysis = analyze_traceability();
    let missing: Vec<u32> = (1..=DOD_ITEM_COUNT)
        .filter(|item| !analysis.covered.contains(item))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/architecture.md must contain a DoD -> task -> test traceability table whose rows key \
         each refactor-plan section 16 DoD item (1-{DOD_ITEM_COUNT}) in the first column, cite a \
         valid scope task id (T001-T033), and reference a test file that exists on disk; DoD items \
         with no complete, real mapping: {missing:?}"
    );
}

#[test]
fn traceability_table_references_multiple_distinct_tasks() {
    let analysis = analyze_traceability();
    assert!(
        analysis.distinct_valid_tasks.len() >= MIN_DISTINCT_TASKS,
        "the traceability table's fully-valid rows must reference at least {MIN_DISTINCT_TASKS} \
         distinct real task ids (the 20 DoD items span 12 PRs); found {}: {:?}",
        analysis.distinct_valid_tasks.len(),
        analysis.distinct_valid_tasks
    );
}

#[test]
fn traceability_table_cites_multiple_distinct_test_files() {
    let analysis = analyze_traceability();
    assert!(
        analysis.distinct_test_files.len() >= MIN_DISTINCT_TEST_FILES,
        "the traceability table's fully-valid rows must cite at least {MIN_DISTINCT_TEST_FILES} \
         distinct existing test files under tests/ (excluding this self-referential test); a table \
         whose rows all point at one trivially-existing path proves no real coverage; found {}: {:?}",
        analysis.distinct_test_files.len(),
        analysis.distinct_test_files
    );
}

#[test]
fn t031_traceability_rows_cite_relevant_tasks_and_pinning_tests() {
    let rows = traceability_rows_by_item();
    let mut problems = Vec::new();
    for item in 1..=DOD_ITEM_COUNT {
        let Some(row) = rows.get(&item) else {
            problems.push(format!("DoD {item}: row missing"));
            continue;
        };

        let cited_tasks = row
            .iter()
            .flat_map(|cell| task_numbers(cell))
            .collect::<BTreeSet<_>>();
        let expected_tasks = relevant_task_ids(item);
        if !expected_tasks.iter().any(|task| cited_tasks.contains(task)) {
            problems.push(format!(
                "DoD {item}: tasks {cited_tasks:?} do not include a delivering task from {expected_tasks:?}"
            ));
        }

        let cited_paths = referenced_test_paths(row);
        let expected_paths = relevant_test_paths(item);
        let missing_cited_on_disk: Vec<&str> = cited_paths
            .iter()
            .map(String::as_str)
            .filter(|path| {
                !PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join(*path)
                    .is_file()
            })
            .collect();
        if !missing_cited_on_disk.is_empty() {
            problems.push(format!(
                "DoD {item}: cited test paths do not exist on disk: {missing_cited_on_disk:?}"
            ));
        }

        let nonexistent_accepted: Vec<&str> = expected_paths
            .iter()
            .chain(required_test_paths(item).iter())
            .copied()
            .filter(|path| {
                !PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join(*path)
                    .is_file()
            })
            .collect();
        if !nonexistent_accepted.is_empty() {
            problems.push(format!(
                "DoD {item}: relevance oracle accepts test paths absent from disk: {nonexistent_accepted:?}"
            ));
        }

        if !cited_paths
            .iter()
            .any(|path| expected_paths.contains(&path.as_str()))
        {
            problems.push(format!(
                "DoD {item}: tests {cited_paths:?} do not include a relevant pin from {expected_paths:?}"
            ));
        }

        let missing_required: Vec<&str> = required_test_paths(item)
            .iter()
            .copied()
            .filter(|required| !cited_paths.iter().any(|path| path == *required))
            .collect();
        if !missing_required.is_empty() {
            problems.push(format!(
                "DoD {item}: broad compatibility requirement is missing test paths {missing_required:?}"
            ));
        }

        if item == 20 {
            let text = row.join(" ").to_lowercase();
            let missing_commands: Vec<&str> = [
                ("cargo test", "cargo test"),
                ("cargo clippy", "cargo clippy"),
                ("cargo fmt --check", "cargo fmt --check"),
                ("integration suites", "integration"),
            ]
            .into_iter()
            .filter_map(|(label, needle)| (!text.contains(needle)).then_some(label))
            .collect();
            if !missing_commands.is_empty() {
                problems.push(format!(
                    "DoD 20: verification gate is missing {missing_commands:?}"
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "the DoD traceability table must cite tests that actually pin each requirement, not \
         merely any existing tests/*.rs path:\n{}",
        problems.join("\n")
    );
}

#[test]
fn selftest_inv1_purity_accepts_positive_and_rejects_negations() {
    assert!(inv1_states_purity(
        "### INV-1: projection purity\nProjection performs no I/O; it is a pure calculation."
    ));
    assert!(inv1_states_purity(
        "projection is a pure, i/o-free calculation"
    ));
    assert!(inv1_states_purity("projection performs no I/O"));
    assert!(inv1_states_purity(
        "projection, without shortcuts, is a pure calculation"
    ));
    assert!(!inv1_states_purity("projection is impure"));
    assert!(!inv1_states_purity("projection is not pure / performs I/O"));
    assert!(!inv1_states_purity("the sync layer performs no I/O"));
    assert!(!inv1_states_purity("projection is not a pure calculation"));
    assert!(!inv1_states_purity("projection is not i/o-free"));
    assert!(!inv1_states_purity("projection is not io-free"));
}

#[test]
fn selftest_inv2_prohibition_requires_negator_governing_import() {
    assert!(inv2_states_prohibition(
        "the source must never import target types"
    ));
    assert!(inv2_states_prohibition(
        "source does not import projection types"
    ));
    assert!(inv2_states_prohibition("source imports no target types"));
    assert!(!inv2_states_prohibition(
        "source imports target types freely"
    ));
    assert!(!inv2_states_prohibition(
        "source does not avoid importing target types"
    ));
    assert!(!inv2_states_prohibition("no target"));
}

#[test]
fn selftest_source_responsibility_rejects_mutable_polarity() {
    assert!(unit_ties_module(
        "source obtains immutable content",
        "source"
    ));
    assert!(unit_ties_module("source reads content", "source"));
    assert!(unit_ties_module(
        "source, without any dependency on targets, obtains immutable content",
        "source"
    ));
    assert!(unit_ties_module(
        "source is not mutable and obtains immutable content",
        "source"
    ));
    assert!(!unit_ties_module(
        "source obtains mutable content",
        "source"
    ));
    assert!(!unit_ties_module(
        "source does not obtain immutable content",
        "source"
    ));
}

#[test]
fn selftest_projection_responsibility_rejects_impure_polarity() {
    assert!(unit_ties_module(
        "projection is a pure i/o-free calculation",
        "projection"
    ));
    assert!(!unit_ties_module(
        "projection performs impure calculation",
        "projection"
    ));
    assert!(!unit_ties_module(
        "projection performs I/O then computes",
        "projection"
    ));
    assert!(!unit_ties_module(
        "projection is not a pure calculation",
        "projection"
    ));
    assert!(!unit_ties_module(
        "projection is not i/o-free but computes",
        "projection"
    ));
}

#[test]
fn selftest_sync_responsibility_requires_target_state() {
    assert!(unit_ties_module(
        "sync owns the target machine state",
        "sync"
    ));
    assert!(unit_ties_module(
        "sync, never delegating ownership elsewhere, owns the target machine state",
        "sync"
    ));
    assert!(!unit_ties_module("sync owns irrelevant state", "sync"));
    assert!(!unit_ties_module("sync does not own target state", "sync"));
    assert!(!unit_ties_module(
        "sync is not the sole owner of the target machine state",
        "sync"
    ));
}

#[test]
fn selftest_placement_accepts_valid_and_rejects_opposite_adjectives() {
    let good = "source obtains immutable content.\nprojection is a pure i/o-free calculation.\nsync owns the target machine state.";
    assert!(placement_ties_each_module(good));
    let attacked = "source obtains mutable content.\nprojection performs impure calculation.\nsync owns irrelevant state.";
    assert!(!placement_ties_each_module(attacked));
    assert!(!placement_ties_each_module(
        "source projection sync are the three modules here."
    ));
}
