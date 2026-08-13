//! Framework-free export domain: the zips a Snapchat "My Data" dump arrives as and the json they
//! hold, read off disk and turned into types the rest of the crate can trust.
//!
//! [`self::zip`] finds the `mydata~*` parts and unpacks them. [`schema`] transcribes the wire,
//! [`model`] validates it. [`ExportJson`] is the whole `json/` dir in one value; the six files
//! phases 2-4 build on arrive as `model` types, the other thirteen as typed [`schema`]
//! passthroughs until a screen needs more from them. [`env`] covers the other half of what a run
//! depends on: the optional tools installed and the room left on disk. [`memories`] joins the
//! media on disk to the entries `memories_history.json` names and enrolls the result in
//! [`manifest`].
//!
//! Phase 2's local-fix leg builds on that: [`overlay`] composites a memory's caption layer back
//! over it, [`timezone`] turns its coordinates into the offset local clocks were at, [`exif`]
//! writes the result into the image (and owns the guard type that keeps `little_exif` on its one
//! safe path), [`video`] does the same job for an MP4's container metadata (and owns the guard type
//! that keeps `mp4ameta` off its chapter legs), [`ffmpeg`] is the only thing that touches video
//! pixels, and [`local_fix`] is the pass that drives all of them and records the outcome in
//! [`manifest`].
//!
//! Phase 3 starts at [`chat_media`], which does for a `chat_media` dir what [`memories`] does for a
//! `memories` one and shares the directory walk both need. What it does NOT share is the join: a
//! chat-media filename carries an id, so the pairing is a stem match and the history join is a
//! string equality, with none of the date bucketing memories has to fall back on. [`chat_fix`] then
//! answers where each file lands and what goes into it, filling the same [`local_fix`] `Plan` the
//! memories planner does, and [`chat_run`] composes the whole leg for a screen exactly as
//! [`memories_run`] composes the other one.
//!
//! Phase 4 starts at [`history`]: chat and snap merge into one per-conversation timeline rendered
//! by four writers over one document model. [`history_run`] is the whole history export in one
//! call — it plans the documents into the same `chat/` tree through [`chat_fix`]'s directory
//! machinery, enrolls one directory-claim row per conversation, and lands the four files beside the
//! media.

pub mod chat_fix;
pub mod chat_media;
pub mod chat_run;
pub mod env;
pub mod exif;
pub mod ffmpeg;
pub mod history;
pub mod history_run;
pub mod local_fix;
pub mod manifest;
pub mod memories;
pub mod memories_run;
pub mod model;
pub mod overlay;
pub mod schema;
pub mod timezone;
pub mod video;
mod walk;
pub mod zip;

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::error::Category;

use crate::export::model::ParseError;

/// The union of every schema filename seen in a real export's `json/` dir, across two observed
/// exports (2026-07-26 and 2026-08-04).
///
/// Each export held 19 files, but the SETS differed by two names: the second dropped
/// `memories_history.json` and added `in_app_reports.json`, both present below. Membership is
/// decided by which data categories the user ticked when requesting the export, so this list is a
/// union of observations, never a contract, and a third export can both drop a name already here
/// and add one neither export has shown. [`read_schema`] already treats every file as optional, so
/// a name from this list missing off a given export's `json/` dir is expected, not a failure. It is
/// mirrored by the redactor's `test_every_real_export_schema_filename_survives_verbatim`, which
/// pins the same union. `tests/export.rs`'s
/// `schema_files_and_the_redactors_real_schema_filenames_agree` cross-checks the two lists against
/// each other, since a pin on each side alone does not catch a name landing on only one of them.
pub const SCHEMA_FILES: [&str; 20] = [
    "account.json",
    "account_history.json",
    "bitmoji.json",
    "chat_history.json",
    "custom_sticker.json",
    "email_campaign_history.json",
    "feature_emails.json",
    "friends.json",
    "in_app_reports.json",
    "location_history.json",
    "memories_history.json",
    "ranking.json",
    "snap_ads.json",
    "snap_history.json",
    "snap_pro.json",
    "snapchat_ai.json",
    "snapchat_plus.json",
    "story_history.json",
    "terms_history.json",
    "user_profile.json",
];

/// Something went wrong getting a file off disk and into a type: the export did not arrive as
/// expected. Distinct from [`ParseError`], which means the export arrived and one of its values
/// is unusable.
#[derive(Debug)]
pub enum LoadError {
    Io { file: &'static str, source: io::Error },
    Json { file: &'static str, source: serde_json::Error },
    Invalid { file: &'static str, source: ParseError },
}

/// What replaces the contents of a redacted run, so the elision is visible rather than silent.
const REDACTED: char = '…';

/// Drops the CONTENTS of every delimited run in a `serde_json` message, keeping everything around
/// them and leaving a marker where each one was.
///
/// `invalid type: string "the message body", expected a sequence at line 1 column 43` becomes
/// `invalid type: string "…", expected a sequence at line 1 column 43`. The expectation and the
/// position survive — that is the half a user needs to diagnose a schema drift — and the value does
/// not, because for `chat_history.json` that value is a message body and this string reaches a
/// footer alert.
///
/// **The delimiters stay and the contents become [`REDACTED`], on every path.** Emitting nothing
/// would render `string, expected a sequence`, which a reader cannot tell from serde having reported
/// no value at all — an absence dressed as a clean result, which is the one failure shape this
/// redaction must not introduce while removing another.
///
/// # Why a delimiter rule and not a phrasing rule
///
/// Matching `invalid type: `, splitting on `, expected `, or enumerating `Unexpected`'s wordings
/// would all pin serde's PROSE, which changes without ceremony and would fail open and silently on a
/// bump. Every data-carrying variant is delimited instead, and that is a far smaller surface:
/// `serde_core` 1.0.229 `de/mod.rs:405-410` renders `Str` as `string {:?}` (double-quoted) and
/// `Bool`/`Unsigned`/`Signed`/`Float`/`Char` inside backticks, while every remaining variant writes
/// a bare constant holding no payload. Versions are the ones `Cargo.lock` pins and the binary links
/// — `serde`/`serde_core` 1.0.229, `serde_json` 1.0.151 — because three serde_core and three
/// serde_json trees sit unpacked in the registry at once and reading the wrong one is one glob away.
///
/// # Quotes first, then backticks, and the order is load-bearing
///
/// `{:?}` does not escape a backtick, so a string value containing one puts a live backtick inside
/// the quoted run. Redacting backticks first would let that stray delimiter pair with a real one
/// further along and mis-slice the message; taking quoted runs out first removes it before the
/// backtick pass can see it.
///
/// # What the ordering costs, and the ceiling it hides
///
/// The quote pass runs over the WHOLE message, so **any message carrying an odd number of quotes is
/// truncated from the first unpaired one**, wherever it sits — including inside a backtick run. That
/// is a property of the two-pass composition, not of the markers: the delimiter-dropping form this
/// replaced cut in exactly the same place and simply left no visible residue to notice it by.
///
/// Nothing reachable produces such a message on the arm that is still redacted. `Unexpected::Str`
/// renders through `{:?}`, which always balances its quotes, and the shape that would carry a lone
/// one — `Unexpected::Char('"')` rendering `` character `"` `` — is unreachable twice over: no type
/// in this crate deserializes a `char`, and `serde_json` 1.0.151's `de.rs` contains no `visit_char`
/// call at all.
///
/// **Trigger, named because this is reachability rather than structure: the first `char` field added
/// to the schema makes an odd-quote message possible on the `Data` arm.** Its symptom is a truncated
/// error, not a failing test. Same shape as the `deny_unknown_fields` and `Deserialize`-enum
/// triggers below, and as the `Unexpected::Other` residual.
///
/// # The escape handling is the whole bypass, not a refinement
///
/// `{:?}` renders an embedded `"` as `\"`, so a value holding a quote puts THREE quotes in the
/// message and a naive first-to-second scan closes early and leaves the tail on screen. Measured:
/// `zqx\"payloadzqx` redacts to `…"payloadzqx"` under a naive loop, leaking `payloadzqx`. JSON
/// strings carry escaped quotes routinely and `chat_history.json` is message text, so this is the
/// common case rather than a corner. Skipping the escaped pair is still a claim about delimiters —
/// it is simply the correct one.
///
/// # Ceiling, with its two triggers named
///
/// A blanket backtick rule also empties `unknown field \`x\`, expected one of \`a\`, \`b\``, where
/// the expected list is OUR schema's field names and is the load-bearing half of that message.
/// Unreachable in this crate today: no struct carries `deny_unknown_fields` (see [`schema`]'s module
/// doc) and no enum derives `Deserialize`, so neither `unknown field` nor `unknown variant` can be
/// raised. **Adding either one makes the gutting live**, and it would present as a message too vague
/// to diagnose rather than as a red test. Whoever adds the first one owns splitting this rule.
///
/// # Residual, stated narrowly because the wide version is false
///
/// It is tempting to write "no undelimited data reaches the message". The honest claim is narrower:
/// **no undelimited data reaches it, and only because serde chose to delimit inside the one variant
/// that formats bare.** `Unexpected::Other` writes its argument unwrapped
/// (`serde_core` 1.0.229 `de/mod.rs:422`), and `serde_core`'s own `visit_i128`/`visit_u128` defaults
/// hand it real values — `de/mod.rs:1410` and `:1472` build `integer \`{}\` as i128` with the value
/// in it. The backtick rule catches those, but on serde's internal formatting rather than on
/// anything contractual, so a release that dropped those backticks would reopen this without a red
/// test.
///
/// **That route is unreachable from THIS crate, and the reason is the target type rather than the
/// input.** `serde_json` 1.0.151 reaches those visitors only from `do_deserialize_i128` /
/// `do_deserialize_u128` (`de.rs:356` and `:388`, wired in at `:1514-1515`), which run when the
/// field being deserialized IS 128-bit — not when the number in the file happens to be large. No
/// type in this crate declares `i128` or `u128`, and an over-`u64` literal against any of our fields
/// overflows to `f64` and arrives as `Unexpected::Float`. Measured, not read: a battery case
/// asserting on the file's digits failed its own control with
/// `floating point \`1.7014118346046923e+38\``, which is what established this.
///
/// **Three enumeration corrections stacked on this one claim, all recorded with their mechanism
/// because the SHAPE is what recurs: each was a correct grep answering a question one scope narrower
/// than the one being asked.**
///
/// 1. Read `serde_json`'s two `Other` sites (`de.rs:137`, `number.rs:812`, both the constant
///    `"number"`) and concluded the VARIANT cannot carry data. Scope asked: what does this variant
///    ever hold. Scope answered: what does *this crate* put in it. `serde_core` constructs it too.
/// 2. Read `serde_core`'s data-carrying sites (`de/mod.rs:1410`, `:1472`) and concluded the route is
///    reachable. Scope asked: can this run here. Scope answered: does this code exist.
/// 3. Read the enclosing functions and found `do_deserialize_i128`/`do_deserialize_u128`, which
///    dispatch on the TARGET type rather than on the input's magnitude. Scope asked: what triggers
///    this. Scope answered, finally, the whole question — and the answer is that nothing in this
///    crate can, because no field is 128-bit.
///
/// None of the three was a careless grep; each was exact about a slightly wrong question. The habit
/// that catches it is stating the scope of the claim in the same breath as the evidence, which is
/// why the paragraphs above name call sites, enclosing functions AND the dispatch condition rather
/// than concluding from any one of them.
///
/// The redactor handles the shape regardless, and the unit tests below drive it directly, so the
/// rule stays right for whoever adds the first 128-bit field.
fn strip_delimited(message: &impl fmt::Display) -> String {
    let rendered = message.to_string();
    // Escape-aware first (see above), then the raw pass over what is left.
    strip_runs(&strip_runs(&rendered, '"', true), '`', false)
}

/// One redaction pass for one delimiter.
///
/// `escaped` says whether a backslash inside the run escapes the next character, which is true of
/// the `{:?}`-rendered quote runs and false of the backtick ones serde writes with a plain `{}`.
///
/// Fails CLOSED, and the mechanism is the INNER loop rather than the `break` that follows it: on an
/// unterminated run that loop consumes the iterator to exhaustion looking for a delimiter, so by the
/// time control reaches the `else` there is nothing left to emit. Measured — replacing the `break`
/// with `out.extend(chars.by_ref())`, which would emit any remaining tail, changes no output on any
/// input. The `break` is belt-and-braces over a property the loop above already provides, kept
/// because the next reader should not have to re-derive that to know the tail cannot escape.
fn strip_runs(text: &str, delimiter: char, escaped: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character != delimiter {
            out.push(character);
            continue;
        }
        // The delimiters STAY and the contents become an ellipsis, so the message says a value was
        // here and was removed. Emitting nothing would leave `string, expected a sequence`, which
        // reads as serde having reported no value at all — an absence that looks like a clean
        // result, which is the failure shape this whole redaction exists inside of.
        out.push(delimiter);
        out.push(REDACTED);
        let mut closed = false;
        while let Some(inner) = chars.next() {
            if escaped && inner == '\\' {
                // The backslash and whatever it escapes are both payload. Treating the `"` of a
                // `\"` as the closing delimiter is the bypass this exists to prevent.
                let _ = chars.next();
                continue;
            }
            if inner == delimiter {
                closed = true;
                break;
            }
        }
        if closed {
            out.push(delimiter);
        } else {
            // Unterminated: the opener and the marker are already out, and everything after them is
            // payload this function could not account for, so nothing further is emitted.
            break;
        }
    }
    out
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { file, source } => {
                write!(f, "could not read {file} from the export's json dir: {source}")
            }
            // `serde_json::Error` covers two failures with opposite fixes. Broken bytes are worth
            // re-extracting; a shape this build does not expect never is, and telling someone to
            // re-unzip a perfectly good file sends them in a circle.
            //
            // **Only the `Data` arm is redacted, and the split is serde's own rather than ours.**
            // `classify()` puts exactly one `ErrorCode` in `Data` — `Message`, the variant carrying
            // caller-supplied text — so `Data` already IS the set of messages that can hold file
            // content, and running the redactor over `Syntax` too would draw a second,
            // differently-shaped boundary over one that already fits.
            //
            // **Two of serde_json 1.0.151's 25 `ErrorCode` arms are NOT constants**, and an earlier
            // version of this comment claimed all of them were. `error.rs:349-387` is the whole
            // `Display` impl and the enum at `:236-311` declares 25 variants; the two exceptions are
            // `Message` (`:352`, an arbitrary string) and `Io` (`:353`, an `io::Error`). The other
            // 23 are constants. `Message` lands in `Data` and is covered by the redaction that
            // stayed — which is `classify()` drawing the line in the right place, checked rather
            // than assumed. `Io` lands in `Category::Io`, which this match routes into the
            // UN-redacted arm below, and is unreachable only because [`read_schema`] parses with
            // `from_slice`: a byte slice performs no IO, so no `ErrorCode::Io` can be constructed.
            //
            // **Trigger, fourth of four on this path: switching that `from_slice` to `from_reader`
            // routes a non-constant `Display` into the un-redacted arm.** A reader is the obvious
            // move for anyone trying to stop holding a large `memories_history.json` in memory, it
            // is a one-identifier diff, and no test would notice. The other three triggers live on
            // [`strip_delimited`].
            //
            // That overlap is not free: four syntax constants wrap PUNCTUATION in backticks —
            // `ExpectedColon`, `ExpectedListCommaOrEnd`, `ExpectedObjectCommaOrEnd` and
            // `ExpectedDoubleQuote` — so redacting here turned `expected `:`` into `expected `…``
            // and, for the last of those, truncated the message outright (its payload is a quote
            // nested in a backtick run, which the quote-first pass opens and never closes). Four of
            // the commonest malformed-json diagnostics, paid for redacting a constant.
            //
            // **Residual, same shape as the `Unexpected::Other` one on [`strip_delimited`]**: this
            // rests on serde keeping data out of the non-`Message` codes. A future release adding a
            // data-bearing code that classifies as `Syntax` would leak here with no red test.
            // `a_syntax_error_carries_its_position_and_none_of_the_input` is what would notice, and
            // only for the shapes it drives — not the `Io` case above, which no fixture can reach
            // while the parse is `from_slice`.
            Self::Json { file, source } => match source.classify() {
                Category::Syntax | Category::Eof | Category::Io => {
                    write!(f, "{file} is not valid json ({source}); re-extract the export part holding json/")
                }
                Category::Data => write!(
                    f,
                    "{file} is valid json in a shape this build does not know, at line {} column {} ({}); \
                     the export's schema has moved, so this needs a parser update rather than another extraction",
                    source.line(),
                    source.column(),
                    strip_delimited(source)
                ),
            },
            Self::Invalid { file, source } => write!(f, "{file}: {source}"),
        }
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Invalid { source, .. } => Some(source),
        }
    }
}

/// A whole `mydata~<id>/json/` dir, parsed.
///
/// Every field is optional because a file Snapchat omits for a given user (nobody has a
/// `snap_ads.json` worth shipping without a business account) must not fail the load. A file
/// that is present and broken still does.
#[derive(Debug)]
pub struct ExportJson {
    // Modelled: the files phases 2-4 build on.
    pub account: Option<model::Account>,
    pub chat_history: Option<model::ChatHistory>,
    pub friends: Option<model::Friends>,
    pub memories: Option<model::Memories>,
    pub snap_history: Option<model::SnapHistory>,
    pub user_profile: Option<model::UserProfile>,

    // Typed passthroughs: parsed and held, no domain type until a screen needs one.
    pub account_history: Option<schema::AccountHistory>,
    pub bitmoji: Option<schema::Bitmoji>,
    pub custom_sticker: Option<schema::CustomSticker>,
    pub email_campaign_history: Option<schema::EmailCampaignHistory>,
    pub feature_emails: Option<schema::FeatureEmails>,
    pub location_history: Option<schema::LocationHistory>,
    pub ranking: Option<schema::Ranking>,
    pub snap_ads: Option<schema::SnapAds>,
    pub snap_pro: Option<schema::SnapPro>,
    pub snapchat_ai: Option<schema::SnapchatAi>,
    pub snapchat_plus: Option<schema::SnapchatPlus>,
    pub story_history: Option<schema::StoryHistory>,
    pub terms_history: Option<schema::TermsHistory>,
}

impl ExportJson {
    /// Reads and parses every file in `json_dir`.
    ///
    /// Fail-fast: the first present-but-unusable file stops the load and the error names it. A
    /// missing file is not a failure, it lands as `None`.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] when a file cannot be read, does not hold json, or holds a value
    /// [`model`] cannot validate.
    pub fn load_dir(json_dir: impl AsRef<Path>) -> Result<Self, LoadError> {
        let dir = json_dir.as_ref();
        Ok(Self {
            account: read_model::<schema::Account, _>(dir, "account.json")?,
            chat_history: read_model::<schema::ChatHistory, _>(dir, "chat_history.json")?,
            friends: read_model::<schema::Friends, _>(dir, "friends.json")?,
            memories: read_model::<schema::MemoriesHistory, _>(dir, "memories_history.json")?,
            snap_history: read_model::<schema::SnapHistory, _>(dir, "snap_history.json")?,
            user_profile: read_model::<schema::UserProfile, _>(dir, "user_profile.json")?,

            account_history: read_schema(dir, "account_history.json")?,
            bitmoji: read_schema(dir, "bitmoji.json")?,
            custom_sticker: read_schema(dir, "custom_sticker.json")?,
            email_campaign_history: read_schema(dir, "email_campaign_history.json")?,
            feature_emails: read_schema(dir, "feature_emails.json")?,
            location_history: read_schema(dir, "location_history.json")?,
            ranking: read_schema(dir, "ranking.json")?,
            snap_ads: read_schema(dir, "snap_ads.json")?,
            snap_pro: read_schema(dir, "snap_pro.json")?,
            snapchat_ai: read_schema(dir, "snapchat_ai.json")?,
            snapchat_plus: read_schema(dir, "snapchat_plus.json")?,
            story_history: read_schema(dir, "story_history.json")?,
            terms_history: read_schema(dir, "terms_history.json")?,
        })
    }
}

fn read_schema<T: DeserializeOwned>(dir: &Path, file: &'static str) -> Result<Option<T>, LoadError> {
    let bytes = match fs::read(dir.join(file)) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(LoadError::Io { file, source }),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|source| LoadError::Json { file, source })
}

fn read_model<S, M>(dir: &Path, file: &'static str) -> Result<Option<M>, LoadError>
where
    S: DeserializeOwned,
    M: TryFrom<S, Error = ParseError>,
{
    read_schema::<S>(dir, file)?.map(|raw| M::try_from(raw).map_err(|source| LoadError::Invalid { file, source })).transpose()
}

#[cfg(test)]
mod tests {
    use super::strip_delimited;

    /// The redactor's own contract, exercised on the message SHAPES serde produces rather than
    /// through the loader — the loader-level property lives in `tests/export.rs`.
    ///
    /// Two of these shapes cannot be reached through this crate's own schema (see the ceiling on
    /// [`strip_delimited`]), which is exactly why they are driven directly: the rule has to be right
    /// for them before someone adds the `deny_unknown_fields` or the `Deserialize` enum that makes
    /// them live.
    #[test]
    fn every_delimited_run_loses_its_contents_and_nothing_else() {
        let cases = [
            // The canonical `Category::Data` message, and the one the coordinator specified.
            (
                r#"invalid type: string "the message body", expected a sequence at line 1 column 43"#,
                "invalid type: string \"…\", expected a sequence at line 1 column 43",
            ),
            // Backtick-delimited payloads: every numeric and char variant.
            ("invalid type: integer `42`, expected a string", "invalid type: integer `…`, expected a string"),
            ("invalid type: floating point `48.858844`, expected a string", "invalid type: floating point `…`, expected a string"),
            ("invalid type: character `x`, expected a string", "invalid type: character `…`, expected a string"),
            ("invalid type: boolean `true`, expected a string", "invalid type: boolean `…`, expected a string"),
            // `Unexpected::Other` carrying data: bare variant, backticked payload.
            (
                "invalid type: integer `170141183460469231731687303715884105728` as u128, expected a string",
                "invalid type: integer `…` as u128, expected a string",
            ),
            // The shapes this crate cannot currently raise, kept honest for whoever makes them live.
            ("unknown field `secret`, expected one of `From`, `Media Type`", "unknown field `…`, expected one of `…`, `…`"),
            ("unknown variant `secret`, expected one of `IMAGE`, `VIDEO`", "unknown variant `…`, expected one of `…`, `…`"),
            // Nothing delimited: untouched, including the position a reader needs.
            ("EOF while parsing a value at line 3 column 1", "EOF while parsing a value at line 3 column 1"),
        ];
        for (raw, expected) in cases {
            assert_eq!(strip_delimited(&raw), expected, "{raw}");
        }
    }

    /// **The bypass, and the reason a quote-free battery proves nothing.**
    ///
    /// `{:?}` renders an embedded `"` as `\"`, so a value holding a quote puts three quotes in the
    /// message and a first-to-second scan closes early and leaves the tail on screen. Every other
    /// marker in this file is quote-free and passes with the escape handling deleted; only this one
    /// dies. A json string carries escaped quotes routinely and `chat_history.json` is message text.
    #[test]
    fn an_escaped_quote_inside_a_value_does_not_end_the_run_early() {
        let message = r#"invalid type: string "zqx\"payloadzqx", expected a sequence at line 1 column 43"#;
        let stripped = strip_delimited(&message);
        assert_eq!(stripped, "invalid type: string \"…\", expected a sequence at line 1 column 43");
        assert!(!stripped.contains("payloadzqx"), "the tail after the escaped quote leaked: {stripped}");

        // A trailing backslash-quote pair with nothing after it must not re-open the run either.
        assert_eq!(strip_delimited(&r#"got string "a\"" here"#), "got string \"…\" here");
    }

    /// A run this function cannot close is payload it cannot account for, so everything from the
    /// opener is dropped. Fail-closed is the only safe direction for a redaction gate: the cost of
    /// over-redacting is a vaguer message, and the cost of under-redacting is a message body on a
    /// terminal.
    #[test]
    fn an_unterminated_run_drops_its_tail_rather_than_passing_it_through() {
        assert_eq!(strip_delimited(&r#"invalid type: string "zqxpayloadzqx"#), "invalid type: string \"…");
        assert_eq!(strip_delimited(&"unknown field `zqxpayloadzqx"), "unknown field `…");
        // An escape consuming the closing quote leaves the run open — still closed, not leaked.
        assert_eq!(strip_delimited(&r#"invalid type: string "zqxpayloadzqx\""#), "invalid type: string \"…");
        // A backtick INSIDE a quoted value is removed by the quote pass, so it can never pair with
        // a real delimiter later in the message. This is why quotes are redacted first.
        assert_eq!(strip_delimited(&r#"string "a`b", expected integer `7`"#), "string \"…\", expected integer `…`");
    }
}
