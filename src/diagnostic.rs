use std::fmt;

use crate::error::Error;

/// Named diagnostic phrases; tests assert these exact strings (confine.rs convention).
pub const SELECTION: &str = "selection:";
pub const MATCHED_AGAINST: &str = "matched against:";
pub const DID_YOU_MEAN: &str = "did you mean:";
pub const REMEDY: &str = "remedy:";
pub const TO_DEBUG: &str = "to debug:";

pub struct SelectionDiagnostic {
    pub entry: String,
    pub matched_against: String,
    pub why: String,
    pub did_you_mean: Option<Vec<String>>,
    pub remedy: String,
    pub debug_hint: Option<String>,
}

impl SelectionDiagnostic {
    #[must_use]
    pub fn config(self) -> Error {
        Error::Config(self.to_string())
    }

    #[must_use]
    pub fn sync(self) -> Error {
        Error::Sync(self.to_string())
    }
}

impl fmt::Display for SelectionDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SELECTION} {} — {}", self.entry, self.why)?;
        write!(f, "\n{MATCHED_AGAINST} {}", self.matched_against)?;

        if let Some(suggestions) = self.did_you_mean.as_deref().filter(|s| !s.is_empty()) {
            write!(f, "\n{DID_YOU_MEAN} {}", suggestions.join(", "))?;
        }

        write!(f, "\n{REMEDY} {}", self.remedy)?;

        if let Some(hint) = self.debug_hint.as_deref().filter(|h| !h.is_empty()) {
            write!(f, "\n{TO_DEBUG} {hint}")?;
        }

        Ok(())
    }
}

/// `None` (not an empty Vec) when nothing is close, so a diagnostic omits its line.
#[must_use]
pub fn did_you_mean<'a>(
    entry: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<Vec<String>> {
    let bound = (entry.chars().count() / 3).max(2);
    let mut scored: Vec<(usize, &str)> = candidates
        .into_iter()
        .filter(|cand| *cand != entry)
        .filter_map(|cand| {
            let dist = strsim::levenshtein(entry, cand);
            (dist <= bound).then_some((dist, cand))
        })
        .collect();
    scored.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.truncate(3);
    (!scored.is_empty()).then(|| {
        scored
            .into_iter()
            .map(|(_, cand)| cand.to_owned())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::{
        DID_YOU_MEAN, MATCHED_AGAINST, REMEDY, SELECTION, SelectionDiagnostic, TO_DEBUG,
        did_you_mean,
    };
    use crate::error::Error;

    fn populated() -> SelectionDiagnostic {
        SelectionDiagnostic {
            entry: "nvim".to_string(),
            matched_against: "the offer set".to_string(),
            why: "not offered by any include".to_string(),
            did_you_mean: Some(vec!["neovim".to_string()]),
            remedy: "add `include = [\"nvim/**\"]` to the source".to_string(),
            debug_hint: Some("phora explain dotfiles src nvim".to_string()),
        }
    }

    #[test]
    fn renders_all_named_sections() {
        let rendered = populated().to_string();

        for phrase in [SELECTION, MATCHED_AGAINST, DID_YOU_MEAN, REMEDY, TO_DEBUG] {
            assert!(
                rendered.contains(phrase),
                "rendered diagnostic must carry the named phrase `{phrase}`; got:\n{rendered}"
            );
        }

        let d = populated();
        for value in [
            d.entry.as_str(),
            d.why.as_str(),
            d.matched_against.as_str(),
            "neovim",
            d.remedy.as_str(),
            "phora explain dotfiles src nvim",
        ] {
            assert!(
                rendered.contains(value),
                "rendered diagnostic must carry the field value `{value}`; got:\n{rendered}"
            );
        }
    }

    #[test]
    fn sections_render_in_canonical_order() {
        let rendered = populated().to_string();

        let position = |phrase: &str| {
            rendered
                .find(phrase)
                .unwrap_or_else(|| panic!("phrase `{phrase}` must appear in:\n{rendered}"))
        };

        let selection = position(SELECTION);
        let matched_against = position(MATCHED_AGAINST);
        let did_you_mean = position(DID_YOU_MEAN);
        let remedy = position(REMEDY);
        let to_debug = position(TO_DEBUG);

        assert!(
            selection < matched_against,
            "selection must precede matched-against; got:\n{rendered}"
        );
        assert!(
            matched_against < did_you_mean,
            "matched-against must precede did-you-mean; got:\n{rendered}"
        );
        assert!(
            did_you_mean < remedy,
            "did-you-mean must precede remedy; got:\n{rendered}"
        );
        assert!(
            remedy < to_debug,
            "remedy must precede to-debug; got:\n{rendered}"
        );
    }

    #[test]
    fn omits_did_you_mean_when_no_suggestions() {
        let mut d = populated();
        let suggestion = match &d.did_you_mean {
            Some(values) => values[0].clone(),
            None => panic!("the populated fixture must carry a suggestion to drop"),
        };
        d.did_you_mean = None;
        let rendered = d.to_string();

        assert!(
            !rendered.contains(DID_YOU_MEAN),
            "no `{DID_YOU_MEAN}` section may appear when there are no suggestions; got:\n{rendered}"
        );
        assert!(
            !rendered.contains(&suggestion),
            "the dropped suggestion value `{suggestion}` must not leak into the render; got:\n{rendered}"
        );
        assert!(
            rendered.contains(REMEDY),
            "the `{REMEDY}` section must still render without suggestions; got:\n{rendered}"
        );
    }

    #[test]
    fn omits_debug_hint_when_absent() {
        let mut d = populated();
        let hint = match &d.debug_hint {
            Some(value) => value.clone(),
            None => panic!("the populated fixture must carry a debug hint to drop"),
        };
        d.debug_hint = None;
        let rendered = d.to_string();

        assert!(
            !rendered.contains(TO_DEBUG),
            "no `{TO_DEBUG}` section may appear without a debug hint; got:\n{rendered}"
        );
        assert!(
            !rendered.contains(&hint),
            "the dropped debug-hint value `{hint}` must not leak into the render; got:\n{rendered}"
        );
        assert!(
            rendered.contains(REMEDY) && rendered.contains(MATCHED_AGAINST),
            "the remedy and matched-against sections must still render without a debug hint; \
             got:\n{rendered}"
        );
    }

    #[test]
    fn omits_to_debug_when_hint_is_empty_string() {
        let mut d = populated();
        d.debug_hint = Some(String::new());
        let rendered = d.to_string();

        assert!(
            !rendered.contains(TO_DEBUG),
            "an empty debug hint must drop the `{TO_DEBUG}` section, mirroring the \
             `did_you_mean` omission contract; got:\n{rendered}"
        );
    }

    #[test]
    fn config_wraps_into_config_variant() {
        let d = populated();
        let full = d.to_string();
        let remedy = d.remedy.clone();

        let err = d.config();
        let s = match &err {
            Error::Config(s) => s.clone(),
            other => panic!("config() must produce Error::Config, got {other:?}"),
        };
        assert_eq!(
            s, full,
            "config() must wrap the COMPLETE rendered diagnostic; got:\n{s}\nexpected:\n{full}"
        );
        assert!(
            err.to_string().contains(&remedy),
            "the Error's Display must surface the rendered remedy; got:\n{err}"
        );
    }

    #[test]
    fn sync_wraps_into_sync_variant() {
        let d = populated();
        let full = d.to_string();

        let err = d.sync();
        let s = match &err {
            Error::Sync(s) => s.clone(),
            other => panic!("sync() must produce Error::Sync, got {other:?}"),
        };
        assert_eq!(
            s, full,
            "sync() must wrap the COMPLETE rendered diagnostic; got:\n{s}\nexpected:\n{full}"
        );
    }

    #[test]
    fn entry_and_why_lead_the_message() {
        let d = populated();
        let entry = d.entry.clone();
        let why = d.why.clone();
        let rendered = d.to_string();

        let first_line = rendered
            .lines()
            .next()
            .unwrap_or_else(|| panic!("the render must have a first line; got:\n{rendered}"));
        assert!(
            first_line.contains(SELECTION),
            "the `{SELECTION}` lead must be the first line of the render; got first line:\n{first_line}"
        );

        let lead = rendered
            .lines()
            .find(|line| line.contains(SELECTION))
            .unwrap_or_else(|| panic!("a `{SELECTION}` lead line must exist; got:\n{rendered}"));

        assert!(
            lead.contains(&entry) && lead.contains(&why),
            "the `{SELECTION}` lead line must carry both the entry and the why so downstream \
             messages stay uniform; got lead line:\n{lead}"
        );
        let entry_pos = lead.find(&entry).expect("entry present in lead");
        let why_pos = lead.find(&why).expect("why present in lead");
        assert!(
            entry_pos < why_pos,
            "the entry must precede the why in the lead line (`<entry> — <why>`); got:\n{lead}"
        );
    }

    #[test]
    fn did_you_mean_lists_multiple_suggestions() {
        let mut d = populated();
        d.did_you_mean = Some(vec!["neovim".to_string(), "emacs".to_string()]);
        let rendered = d.to_string();

        let suggestion_line = rendered
            .lines()
            .find(|line| line.contains(DID_YOU_MEAN))
            .unwrap_or_else(|| panic!("a `{DID_YOU_MEAN}` line must exist; got:\n{rendered}"));

        assert!(
            suggestion_line.contains("neovim"),
            "the first suggestion must appear after the `{DID_YOU_MEAN}` phrase; got:\n{suggestion_line}"
        );
        assert!(
            suggestion_line.contains("emacs"),
            "the second suggestion must appear after the `{DID_YOU_MEAN}` phrase; got:\n{suggestion_line}"
        );
    }

    #[test]
    fn did_you_mean_orders_by_distance_then_lexically_and_caps_at_three() {
        let candidates = [
            "init.lua",
            "innit.lua",
            "keymaps.lua",
            "imit.lua",
            "irit.lua",
        ];
        let got = did_you_mean("init.lua", candidates).expect("close candidates exist");
        assert_eq!(
            got,
            vec![
                "imit.lua".to_string(),
                "innit.lua".to_string(),
                "irit.lua".to_string()
            ],
            "the three distance-1 candidates are returned lexically ordered and capped at three; \
             the self-match `init.lua` and the far `keymaps.lua` are excluded"
        );
    }

    #[test]
    fn did_you_mean_excludes_the_entry_itself_and_far_candidates_returning_none() {
        assert_eq!(
            did_you_mean("editor", ["editor", "wholly-different-name"]),
            None,
            "an exact self-match is not a suggestion and a far candidate is out of bound, so \
             nothing close remains -> None (omits the diagnostic line)"
        );
    }
}
