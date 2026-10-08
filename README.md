# Blip

**A quiet radar for your next role.** Blip sits in the top-right corner of your screen as a small pill. Every 30 minutes it checks job boards for new internships, co-ops, and new-grad roles, scores them against your resume with an AI model running on your own computer, and shows you the five best matches. Then it goes back to sleep.

Nothing about you leaves your machine. Your resume, the scoring, and your history all stay local. The only network traffic is fetching public job listings, plus the Anthropic API if you choose to use it.

Runs on macOS and Windows.

---

## What it does

- **Watches the job boards for you.** Blip reads two community-maintained internship lists, [SimplifyJobs](https://github.com/SimplifyJobs/Summer2027-Internships) and [vanshb03](https://github.com/vanshb03/Summer2027-Internships), which together cover hundreds of companies. It also reads SimplifyJobs' [new-grad list](https://github.com/SimplifyJobs/New-Grad-Positions) if you're looking for full-time roles, Amazon's own job site, federal internships from [USAJobs](https://www.usajobs.gov) (with a free API key), and the job boards of companies you choose on Greenhouse, Ashby, Lever, Workday, and Oracle Recruiting.
- **Remembers what it has seen.** It never shows you the same role twice, even when one job is listed on two sites under slightly different titles.
- **Scores against your resume.** A local model reads each promising posting's full description and gives it a 0–100 match score with a one-line reason and any red flags, like a citizenship requirement or the wrong location.
- **Shows only what's worth your time.** You get your top matches (5 by default). Roles outside your chosen fields, or under your strictness bar, are never shown, so an empty panel means nothing good turned up.
- **Logs your applications to Excel.** Press ✓ on a role and it's added as a row to your spreadsheet. Your own columns and edits are kept.
- **Filters what you can't apply to.** It can limit roles to the US or to places you choose, and hide roles that need a work authorization you don't have.

## The pill

The pill shows a one-word status:

| Status | Meaning |
|---|---|
| **Resting** | Waiting for the next scan. Click to scan now. |
| **Scanning** | Working. Click to stop. |
| **Complete** | New matches are ready. Click to open them again. |
| **Paused** | Automatic scans are off. Scan now still works. |
| **Error** | Something needs fixing. Click and Blip opens the settings tab that fixes it. |
| **Set up** | First-run setup isn't finished. Click to continue. |

When a scan finishes, the pill expands into the results panel. Each match shows its score, title, company, location, posting age, the reason it fits, and any red flags or deadlines. Click the title to open the posting, press **✓** if you applied, or **✕** to never see it again.

The **⚙** gear opens Settings. The **clock** button, in the results and in Settings, opens your History.

## Getting started

### Install

1. Get the `.dmg` (macOS) or the installer (Windows). These are attached to each run of the **build** workflow in this repo's Actions tab, or you can [build it yourself](#building-from-source).
2. Open Blip. The first time you open it on a Mac, right-click it and choose **Open**: builds aren't signed yet, so macOS asks first.

### First-run setup

Blip walks you through five steps inside the pill:

1. **Ollama.** Blip runs its AI model through [Ollama](https://ollama.com), which is free. Blip detects whether it's installed and running, starts it for you if it isn't running, or links to the download.
2. **Models.** Blip downloads two models, about 3.6 GB in total, with a progress bar:
   - `gemma3:4b` reads and scores postings.
   - `nomic-embed-text` matches postings to your resume.
3. **Your resume.** PDF, plain text, or Markdown. Blip reads it once to learn your skills, projects, graduation date, and work authorization.
4. **Your preferences.** What you're looking for, role types, season, location, and how recent postings should be.
5. **Start scanning.**

You can run setup again any time from **Settings → Log**.

## Settings

| Tab | What's there |
|---|---|
| **Profile** | Your resume, a "what you're looking for" note, the fields you want (ML / AI, software, data, hardware, quant, product, business), and how picky Blip should be (relaxed, normal, strict) |
| **Search** | Role types (internship, co-op, new grad), season, location (anywhere or US only, plus optional places like `NYC, Seattle, TX`), work authorization, max posting age, skip MS/PhD-only roles |
| **Cycle** | How often to scan, active hours, pause on low battery, pause automatic scans, start at login, notifications for strong matches |
| **Model** | Local Ollama model, or the Anthropic API with a key stored in your system keychain |
| **Sources** | Turn each job list on or off (the community lists, Amazon, and USAJobs, which needs your free API key), and manage your company watchlist. Add a Greenhouse, Ashby, or Lever company by name, or any company (including Workday and Oracle sites) by pasting a link to its careers page, optionally with the name first: `General Motors https://…`. Click a company's name to rename it. |
| **Log** | Which spreadsheet ✓ writes to, export the last 7 days as CSV, run setup again |

Changes save as you make them.

## How it works

Each cycle runs these steps:

1. **Fetch.** Pull current listings from the community lists and every company in your watchlist.
2. **Dedupe.** Each posting is fingerprinted by company, title, location, and season, and also matched by its job ID on the hiring platform. Anything already seen is skipped.
3. **Filter.** Drop anything that fails a hard filter: a title that's only about fields you didn't pick, role type, season, posting age, location, an MS/PhD requirement, or a work-authorization requirement stated in the description. These checks are free.
4. **Shortlist.** Compare each remaining posting to your resume using embeddings and keep the 20 closest.
5. **Read.** Fetch each shortlisted job's page so the model sees the real description. Ashby and Lever include descriptions in their feeds, and Workday and Oracle pages are read through the JSON behind them, since the pages themselves are JavaScript-only.
6. **Score.** The model doesn't pick a number. It answers narrow questions: is the role's main work in your fields, which of the posting's requirements you meet, and which you're missing. Blip computes the score from those answers, so a role the model calls off-field can't score high. Roles judged from a title alone are capped unless the title itself names one of your fields.
7. **Show.** Your top matches above the strictness bar (70 on "normal") appear in the panel. Scores are cached, so a role is never re-scored unless your resume, "looking for" note, or model changes.

**Deadlines.** Job feeds rarely publish deadlines, so Blip uses one only when the posting states it, or when the board lists it (some Greenhouse companies do). Roles closing within a week are flagged, and you get one reminder three days out.

**Work authorization.** Few job sources report sponsorship. The vanshb03 list marks roles that don't sponsor (🛂) or need US citizenship (🇺🇸), but SimplifyJobs' own data says "Other" for over 99% of listings. So Blip also reads each description for citizenship, security clearance, export-control (ITAR), and "won't sponsor" language, and filters based on your work authorization, which comes from your resume unless you override it.

## Your data

Everything Blip stores is in one folder:

- **macOS:** `~/Library/Application Support/Blip/`
- **Windows:** `%APPDATA%\Blip\`

| File | What it holds |
|---|---|
| `config.json` | Your settings |
| `profile.json` | What Blip learned from your resume |
| `blip.db` | Every posting seen, scores, and your shown, applied, and dismissed history (SQLite) |

Your applied roles are also written to `~/Documents/Applied.xlsx` unless you choose another file. API keys you add (Anthropic, USAJobs) live in the macOS Keychain or Windows Credential Manager, never in a file.

To start fresh, quit Blip and delete the folder.

## Command line

The scan pipeline also runs from a terminal, which is handy for testing and tuning:

```bash
cargo install --path crates/blip-cli

blip profile --resume ~/Documents/resume.pdf   # read a resume and build the profile
blip scan                                      # fetch and list postings Blip hasn't seen
blip scan --top 5                              # full cycle: fetch, filter, score, show the top 5
blip find "Jane Street"                        # look up a company's job board by name or link
```

`blip scan` also takes `--limit N` (how many new postings to list) and `--db PATH` (use a different database, useful for experiments).

## Building from source

**Requirements:** [Rust](https://rustup.rs) (stable) and [Ollama](https://ollama.com).

```bash
git clone https://github.com/SelimC06/Blip.git
cd Blip

cargo run -p blip-app          # run the app in development
cargo test -p blip-core        # run the tests

cargo install tauri-cli --version "^2" --locked
cd app/src-tauri && cargo tauri build   # .app and .dmg on macOS, installers on Windows
```

The interface is plain HTML, CSS, and JavaScript in `app/ui/`, with no Node or bundler needed.

The GitHub Actions workflow in `.github/workflows/build.yml` builds macOS and Windows releases when you push a `v*` tag or run it by hand.

> **The `.dmg` step fails or opens a Finder window?** Build with `CI=true cargo tauri build`. That skips the Finder automation that lays out the `.dmg` window, which can time out.

> **macOS linker error mentioning `unknown architecture` and `MacOSX27.0.sdk`?** Your Command Line Tools linker is older than the newest SDK. Point the build at an older SDK, for example `export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk`, or update the Command Line Tools.

## Project layout

```
crates/blip-core/        the pipeline, usable without any UI
  src/sources/           community lists, Greenhouse, Ashby, Lever, Workday, Oracle, company lookup
  src/score.rs           filters, embedding shortlist, LLM scoring, deadlines
  src/location.rs        location filter
  src/auth.rs            work-authorization filter
  src/store.rs           SQLite: postings, dedupe, score cache, history
  src/profile.rs         resume → profile
  src/llm.rs             Ollama and Anthropic API
  src/applied_log.rs     Excel applied log
crates/blip-cli/         the `blip` command
app/src-tauri/           the desktop app (Tauri 2): scheduler, commands, setup
app/ui/                  the pill: HTML, CSS, JS, fonts
```

## Known limits

- **USAJobs needs a key.** Request a free one at [developer.usajobs.gov](https://developer.usajobs.gov/apirequest/) and paste it, with the email you registered, in Settings → Sources.
- **Unsigned builds.** macOS shows a Gatekeeper prompt the first time you open Blip (right-click → Open), and Windows shows SmartScreen.
- **JavaScript-only job pages.** Workday and Oracle are handled, but other JavaScript-only career sites return no readable description, so those roles are scored from their title, company, and location alone.
- **Workday and Oracle are unofficial.** Blip reads the same endpoints their own career pages use. They aren't published APIs, so a company can block them or Workday and Oracle can change them. If that happens, the company's chip shows as down.
- **Rare deadlines.** Most postings don't state a deadline, so most roles won't have one.
- **Company boards by name.** Typing a name finds a Greenhouse, Ashby, or Lever board only when its URL name matches, for example "Anduril" lives at `andurilindustries`. Workday and Oracle sites always need a link.

## License

MIT. See [LICENSE](LICENSE).

## Credits

Fonts: [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) and [IBM Plex Sans](https://github.com/IBM/plex), both under the SIL Open Font License, included in `app/ui/fonts/`. Job data comes from [SimplifyJobs](https://github.com/SimplifyJobs), [vanshb03/Ouckah](https://github.com/vanshb03/Summer2027-Internships), the public Greenhouse, Ashby, and Lever job board APIs, amazon.jobs, the official USAJobs API, and the endpoints behind Workday and Oracle Recruiting career sites.
