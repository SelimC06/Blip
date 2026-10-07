//! What field a role is in, read from its title, so roles clearly outside
//! the user's target fields are dropped before any LLM call. Deliberately
//! lenient: a title that names no field, or names any target field
//! alongside others ("Finance & Analytics"), is kept for the LLM to judge.

use regex::Regex;
use std::sync::LazyLock;

/// (id, label shown in Settings, title pattern)
pub const FIELDS: &[(&str, &str)] = &[
    ("ml-ai", "ML / AI"),
    ("software", "software"),
    ("data", "data"),
    ("hardware", "hardware"),
    ("quant", "quant"),
    ("product", "product"),
    ("business", "business & other"),
];

pub fn default_targets() -> Vec<String> {
    vec!["ml-ai".into(), "software".into(), "data".into()]
}

static PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    let p = |s: &str| Regex::new(&format!("(?i){s}")).unwrap();
    vec![
        ("ml-ai", p(r"\b(machine learning|ml|ai|a\.i\.|artificial intelligence|deep learning|computer vision|nlp|natural language|llms?|genai|generative|applied scien\w*|perception|autonomy|autonomous|robotics|reinforcement learning|neural)\b")),
        ("software", p(r"\b(software|swe|sde|developer|programmer|programming|backend|back-end|frontend|front-end|full[- ]?stack|mobile|ios|android|web|devops|sre|site reliability|platform|infrastructure|cloud|cyber ?security|security engineer|application development|systems engineer|compiler|game|gameplay|tools engineer|qa|swqa|sqa|sdet|quality assurance|test development|test automation|automation engineer)\b")),
        ("data", p(r"\b(data|analytics|analyst|business intelligence|bi|statistic\w*|insights)\b")),
        ("hardware", p(r"\b(electrical|electronics?|hardware|fpga|asic|rtl|circuit|pcb|firmware|rf|power systems?|semiconductor|silicon|verification|test equipment|embedded hardware)\b")),
        ("quant", p(r"\b(quant\w*|trading|trader)\b")),
        ("product", p(r"\b(product intern\w*|product manag\w*|associate product manager|apm|program manag\w*|ux|ui designer|product design\w*|user research)\b")),
        ("business", p(r"\b(marketing|finance|financial|accounting|accountant|tax|audit|sales|hr|human resources|recruiting|talent|legal|operations|supply chain|logistics|procurement|customer|corporate communications|marketing communications|public relations|consulting|mechanical|civil|chemical|manufacturing|quality|maintenance|safety|warehouse|facilities|real estate|investment banking|banking|underwriting|actuarial|insurance|claims|merchandising|retail|store|nursing|clinical|pharmac\w*|biolog\w*|chemistry|environmental|geolog\w*|journalism|editorial|design intern|graphic design)\b")),
    ]
});

/// Every field the title names (possibly several, possibly none).
pub fn fields_of(title: &str) -> Vec<&'static str> {
    PATTERNS.iter().filter(|(_, re)| re.is_match(title)).map(|(id, _)| *id).collect()
}

/// Keep unless the title names fields and none of them is a target.
pub fn in_targets(title: &str, targets: &[String]) -> bool {
    if targets.is_empty() {
        return true;
    }
    let named = fields_of(title);
    named.is_empty() || named.iter().any(|f| targets.iter().any(|t| t == f))
}

/// "ML / AI, software, data" — for the scoring prompt.
pub fn describe_targets(targets: &[String]) -> String {
    let labels: Vec<&str> = FIELDS
        .iter()
        .filter(|(id, _)| targets.iter().any(|t| t == id))
        .map(|(_, label)| *label)
        .collect();
    if labels.is_empty() { "any field".into() } else { labels.join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Vec<String> {
        default_targets()
    }

    #[test]
    fn drops_roles_clearly_outside_the_targets() {
        for drop in [
            "Electrical Engineer Co-op",
            "Mission Operations Intern - Summer 2027",
            "Test Equipment Engineering and Maintenance Intern",
            "Logistics Specialist - 2027 Internship",
            "Electrical Hardware Engineering Intern",
            "Precision Medicine Operations Co-op",
            "Early Career, Associate Product Manager (2027)",
            "Tax Intern - Other Tax - Americas Tax Technology Group",
            "Media Product Intern",
        ] {
            assert!(!in_targets(drop, &t()), "should drop {drop:?}: {:?}", fields_of(drop));
        }
    }

    #[test]
    fn keeps_target_mixed_and_unclear_roles() {
        for keep in [
            "Machine Learning Engineer Intern",
            "NVIDIA 2027 Ignite Internships: Software Engineering",
            "Data Science Intern",
            "AI/ML Engineer Intern - Mapping",
            "Software Development Engineer Intern - Mobile(iOS)",
            "Finance Transformation & Analytics Intern", // names data too: the LLM decides
            "Marketing Intern - Marketing Analytics",
            "Research Intern",                           // names no field: the LLM decides
            "Special Projects Developer Intern",
            "SWQA Test Development Intern, GPU Communications Libraries - 2027",
            "Summer 2027 Intern",
        ] {
            assert!(in_targets(keep, &t()), "should keep {keep:?}: {:?}", fields_of(keep));
        }
        assert!(in_targets("Electrical Engineer Co-op", &["hardware".to_string()]));
        assert!(in_targets("Electrical Engineer Co-op", &[]), "no targets = no limit");
    }
}
