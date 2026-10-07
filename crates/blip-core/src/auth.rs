//! Work-authorization filter. No source publishes this reliably (Simplify's
//! own data says "Other" for 99.5% of listings), so Blip reads the job
//! description, which it already fetches for every role it scores.

use regex::Regex;
use std::sync::LazyLock;

/// What a posting requires, from strictest to loosest. Stored on the posting.
pub const CITIZENSHIP: &str = "citizenship"; // US citizens only (incl. clearance)
pub const US_PERSON: &str = "us_person"; // citizens or permanent residents (ITAR/EAR)
pub const NO_SPONSORSHIP: &str = "no_sponsorship"; // any status not needing a visa

static CITIZEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(u\.?s\.? citizenship (is |will be )?required|requires? (u\.?s\.? |united states )citizenship|must (be|hold) (a |an active )?(u\.?s\.?|united states) citizen|(u\.?s\.?|united states) citizens? only|only (u\.?s\.?|united states) citizens|requir\w*[^.\n]{0,40}\bclearance\b|\bclearance\b[^.\n]{0,25}\brequired\b|ability to obtain[^.\n]{0,40}\bclearance\b)",
    )
    .unwrap()
});

static US_PERSON_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\bitar\b|\bear\b controlled|export control(led)? (laws|regulations)|must (be|qualify as) a u\.?s\.? person|u\.?s\.? persons? only|(permanent resident|green card holder)s? (or|and) (u\.?s\.? )?citizens?|(u\.?s\.? )?citizens? or (lawful )?permanent residents?)",
    )
    .unwrap()
});

static NO_SPONSOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)((unable|not able|cannot|can't|will not|won't|do not|does not|don't|is not able|are not able) (to )?(provide |offer |support )?(any )?(employment |work |immigration )?(visa )?sponsor|sponsorship (is |will )?not (be )?(available|offered|provided|possible)|without (the need for )?(current or future |present or future |now or in the future )?(employer |visa |employment )?sponsorship|not eligible for (visa |immigration )?sponsorship|no (visa |immigration )?sponsorship|(do not|does not|don't|cannot|will not) sponsor)",
    )
    .unwrap()
});

static NEGATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(not|no|never|isn't|aren't|won't)\b").unwrap());

/// A match that isn't negated ("a clearance is not required" doesn't count).
fn states(re: &Regex, text: &str) -> bool {
    re.find_iter(text).any(|m| !NEGATION.is_match(m.as_str()))
}

/// The strictest requirement the description states, or "" for none found.
/// Simplify's 🇺🇸 / 🛂 title markers count too, if they ever reappear.
pub fn classify(title: &str, description: &str) -> &'static str {
    // "Citizens or permanent residents" must not read as "citizens only".
    let without_us_person = US_PERSON_RE.replace_all(description, " ");
    if title.contains('\u{1F1FA}') || states(&CITIZEN_RE, &without_us_person) {
        CITIZENSHIP
    } else if states(&US_PERSON_RE, description) {
        US_PERSON
    } else if title.contains('\u{1F6C2}') || NO_SPONSOR_RE.is_match(description) {
        NO_SPONSORSHIP
    } else {
        ""
    }
}

/// Normalize what the profile extraction or the setting says.
pub fn normalize_authorization(s: &str) -> &'static str {
    let s = s.to_lowercase();
    if s.contains("sponsor") || s.contains("visa") {
        "needs_sponsorship"
    } else if s.contains("permanent") || s.contains("green") {
        "permanent_resident"
    } else if s.contains("citizen") {
        "citizen"
    } else {
        "unknown"
    }
}

/// Whether a posting with requirement `block` is closed to someone with
/// `authorization`. Unknown authorization never blocks: better to show a
/// role than hide it on a guess.
pub fn blocks(block: &str, authorization: &str) -> bool {
    match normalize_authorization(authorization) {
        "needs_sponsorship" => !block.is_empty(),
        "permanent_resident" => block == CITIZENSHIP,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_requirements_from_descriptions() {
        assert_eq!(classify("SWE Intern", "We are unable to sponsor visas for this role."), NO_SPONSORSHIP);
        assert_eq!(classify("SWE Intern", "Candidates must be authorized to work without the need for current or future visa sponsorship."), NO_SPONSORSHIP);
        assert_eq!(classify("SWE Intern", "This position requires an active Secret clearance."), CITIZENSHIP);
        assert_eq!(classify("SWE Intern", "U.S. citizenship is required."), CITIZENSHIP);
        assert_eq!(classify("SWE Intern", "Must have the ability to obtain a DoD security clearance."), CITIZENSHIP);
        assert_eq!(classify("SWE Intern", "A clearance is not required. We require Rust."), "");
        assert_eq!(classify("SWE Intern", "Applicants must be U.S. citizens or lawful permanent residents due to ITAR."), US_PERSON);
        assert_eq!(classify("SWE Intern", "We offer visa sponsorship and relocation."), "");
        assert_eq!(classify("SWE Intern", "Build distributed systems in Rust."), "");
        assert_eq!(classify("SWE Intern 🛂", ""), NO_SPONSORSHIP);
    }

    #[test]
    fn blocks_by_authorization() {
        assert!(blocks(NO_SPONSORSHIP, "needs_sponsorship"));
        assert!(blocks(US_PERSON, "needs_sponsorship"));
        assert!(blocks(CITIZENSHIP, "permanent_resident"));
        assert!(!blocks(US_PERSON, "permanent_resident"));
        assert!(!blocks(NO_SPONSORSHIP, "permanent_resident"));
        assert!(!blocks(CITIZENSHIP, "citizen"));
        assert!(!blocks(CITIZENSHIP, "unknown"));
        assert!(!blocks("", "needs_sponsorship"));
    }
}
