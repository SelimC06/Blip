//! Location filter. Postings say where they are in free text ("NYC",
//! "Toronto, ON, Canada", "New York, New York, USA", "Remote in USA",
//! "In-Office"), often several places in one string. Rule of thumb: drop a
//! posting only when it clearly names somewhere outside the user's scope;
//! when the text is ambiguous, keep it.

use regex::Regex;
use std::sync::LazyLock;

static US_STATE_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:,|\s)\s*(AL|AK|AZ|AR|CA|CO|CT|DE|DC|FL|GA|HI|ID|IL|IN|IA|KS|KY|LA|ME|MD|MA|MI|MN|MS|MO|MT|NE|NV|NH|NJ|NM|NY|NC|ND|OH|OK|OR|PA|RI|SC|SD|TN|TX|UT|VT|VA|WA|WV|WI|WY)\b",
    )
    .unwrap()
});

static US_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(usa|u\.s\.a?\.?|united states|america|nyc|new york|sf|san francisco|bay area|silicon valley|los angeles|seattle|boston|chicago|austin|atlanta|denver|washington|dc|philadelphia|pittsburgh|san diego|san jose|palo alto|mountain view|menlo park|sunnyvale|redmond|houston|dallas|miami|detroit|minneapolis|salt lake city|portland|raleigh|columbus|phoenix|alabama|alaska|arizona|arkansas|california|colorado|connecticut|delaware|florida|georgia|hawaii|idaho|illinois|indiana|iowa|kansas|kentucky|louisiana|maine|maryland|massachusetts|michigan|minnesota|mississippi|missouri|montana|nebraska|nevada|new hampshire|new jersey|new mexico|north carolina|north dakota|ohio|oklahoma|oregon|pennsylvania|rhode island|south carolina|south dakota|tennessee|texas|utah|vermont|virginia|wisconsin|wyoming)\b",
    )
    .unwrap()
});

static NON_US: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(canada|toronto|vancouver|montreal|ottawa|waterloo|calgary|uk|u\.k\.|united kingdom|england|scotland|london|manchester|cambridge, uk|edinburgh|ireland|dublin|spain|madrid|barcelona|germany|berlin|munich|hamburg|france|paris|netherlands|amsterdam|belgium|brussels|switzerland|zurich|geneva|austria|vienna|italy|milan|rome|portugal|lisbon|poland|warsaw|krakow|czech|prague|romania|bucharest|sweden|stockholm|norway|oslo|denmark|copenhagen|finland|helsinki|estonia|tallinn|greece|athens|israel|tel aviv|india|bangalore|bengaluru|hyderabad|pune|mumbai|delhi|gurgaon|gurugram|noida|chennai|singapore|japan|tokyo|china|beijing|shanghai|shenzhen|hong kong|taiwan|taipei|korea|seoul|australia|sydney|melbourne|new zealand|auckland|brazil|são paulo|sao paulo|mexico|mexico city|argentina|buenos aires|colombia|bogota|bogotá|chile|santiago|uae|dubai|abu dhabi|saudi|riyadh|south africa|cape town|nigeria|lagos|kenya|nairobi|philippines|manila|vietnam|indonesia|jakarta|malaysia|thailand|bangkok|emea|apac|latam)\b",
    )
    .unwrap()
});

// Canadian province codes look like US state codes ("Toronto, ON").
static CA_PROVINCE_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r",\s*(ON|BC|QC|AB|MB|SK|NS|NB|NL|PE)\b").unwrap());

static REMOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bremote\b").unwrap());

fn mentions_us(loc: &str) -> bool {
    // "Georgia" and "Washington" are ambiguous; a non-US hit nearby decides.
    US_STATE_CODE.is_match(loc) || US_WORDS.is_match(loc)
}

fn mentions_non_us(loc: &str) -> bool {
    NON_US.is_match(loc) || CA_PROVINCE_CODE.is_match(loc)
}

pub fn is_remote(loc: &str) -> bool {
    REMOTE.is_match(loc)
}

/// Inside the user's location scope. "us" keeps anything that names a US
/// place (even alongside foreign ones) and anything ambiguous; it drops only
/// postings that name foreign places and no US place.
pub fn in_scope(loc: &str, scope: &str) -> bool {
    match scope {
        "us" => mentions_us(loc) || !mentions_non_us(loc),
        _ => true,
    }
}

/// Shorthands people type, mapped to how postings spell them.
fn expand(place: &str) -> Vec<String> {
    let p = place.trim().to_lowercase();
    let aliases: &[&str] = match p.as_str() {
        "nyc" | "new york" | "new york city" | "ny" => &["nyc", "new york", ", ny"],
        "sf" | "san francisco" | "bay area" | "sf bay area" | "silicon valley" => &[
            "sf", "san francisco", "bay area", "palo alto", "mountain view", "menlo park",
            "sunnyvale", "san jose", "santa clara", "redwood city", "san mateo", "oakland",
        ],
        "la" | "los angeles" => &["los angeles", "la", "santa monica", "culver city", "irvine"],
        "dc" | "washington dc" | "washington, dc" => &["washington, dc", "dc", "arlington", "mclean", "reston"],
        "seattle" => &["seattle", "bellevue", "redmond", "kirkland"],
        "boston" => &["boston", "cambridge, ma", "somerville"],
        _ => return vec![p],
    };
    aliases.iter().map(|s| s.to_string()).collect()
}

const STATE_NAMES: &[(&str, &str)] = &[
    ("al", "alabama"), ("ak", "alaska"), ("az", "arizona"), ("ar", "arkansas"), ("ca", "california"),
    ("co", "colorado"), ("ct", "connecticut"), ("de", "delaware"), ("fl", "florida"), ("ga", "georgia"),
    ("hi", "hawaii"), ("id", "idaho"), ("il", "illinois"), ("in", "indiana"), ("ia", "iowa"),
    ("ks", "kansas"), ("ky", "kentucky"), ("la", "louisiana"), ("me", "maine"), ("md", "maryland"),
    ("ma", "massachusetts"), ("mi", "michigan"), ("mn", "minnesota"), ("ms", "mississippi"),
    ("mo", "missouri"), ("mt", "montana"), ("ne", "nebraska"), ("nv", "nevada"), ("nh", "new hampshire"),
    ("nj", "new jersey"), ("nm", "new mexico"), ("nc", "north carolina"), ("nd", "north dakota"),
    ("oh", "ohio"), ("ok", "oklahoma"), ("or", "oregon"), ("pa", "pennsylvania"), ("ri", "rhode island"),
    ("sc", "south carolina"), ("sd", "south dakota"), ("tn", "tennessee"), ("tx", "texas"), ("ut", "utah"),
    ("vt", "vermont"), ("va", "virginia"), ("wa", "washington"), ("wv", "west virginia"),
    ("wi", "wisconsin"), ("wy", "wyoming"),
];

fn place_matches(loc: &str, place: &str) -> bool {
    let place = place.trim();
    if place.is_empty() {
        return false;
    }
    // A two-letter state code: match ", TX" in the posting or the state's name.
    if place.len() == 2 {
        let code = place.to_lowercase();
        if let Some((_, name)) = STATE_NAMES.iter().find(|(c, _)| *c == code) {
            let re = Regex::new(&format!(r"(?:,|\s)\s*{}\b", place.to_uppercase())).unwrap();
            if re.is_match(loc) || loc.to_lowercase().contains(name) {
                return true;
            }
        }
    }
    let lower = loc.to_lowercase();
    expand(place).iter().any(|alias| {
        // Word-boundary match so "la" doesn't hit "Atlanta".
        Regex::new(&format!(r"(?i)(^|[^a-z]){}($|[^a-z])", regex::escape(alias)))
            .map(|re| re.is_match(&lower))
            .unwrap_or(false)
    })
}

/// When the user listed places, keep postings in any of them, plus remote
/// ones (which are reachable from anywhere in scope). No places = no limit.
pub fn near_places(loc: &str, places: &[String]) -> bool {
    let places: Vec<&String> = places.iter().filter(|p| !p.trim().is_empty()).collect();
    places.is_empty() || is_remote(loc) || places.iter().any(|p| place_matches(loc, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_scope_drops_only_clearly_foreign_postings() {
        for keep in [
            "NYC", "SF", "New York, New York, USA", "Remote in USA", "Austin, TX",
            "Toronto, ON, Canada, New York, NY", // mixed: a US option exists
            "In-Office", "Remote", "", "Hybrid", "Seattle, WA",
        ] {
            assert!(in_scope(keep, "us"), "should keep {keep:?}");
        }
        for drop in [
            "Madrid, Spain", "Berlin, Germany", "London, UK", "Toronto, ON, Canada",
            "Remote in Canada", "Bengaluru, India", "Singapore", "Waterloo, ON",
        ] {
            assert!(!in_scope(drop, "us"), "should drop {drop:?}");
        }
        assert!(in_scope("Madrid, Spain", "anywhere"));
    }

    #[test]
    fn places_match_shorthands_states_and_remote() {
        let places = vec!["NYC".to_string(), "TX".to_string(), "Bay Area".to_string()];
        assert!(near_places("New York, New York, USA", &places));
        assert!(near_places("Austin, TX", &places));
        assert!(near_places("Dallas, Texas", &places));
        assert!(near_places("Mountain View, CA", &places));
        assert!(near_places("Remote in USA", &places));
        assert!(!near_places("Chicago, IL", &places));
        assert!(!near_places("Atlanta, GA", &vec!["LA".to_string()]), "LA must not match Atlanta");
        assert!(near_places("Chicago, IL", &[]), "no places = no limit");
        assert!(near_places("Chicago, IL", &vec!["  ".to_string()]));
    }
}
