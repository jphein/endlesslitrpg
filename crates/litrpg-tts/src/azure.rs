//! The Azure DragonHD plugin — a first-class backend, not a fallback (spec §D6).
//!
//! Two things make this path cheap: DragonHD serves
//! `raw-16khz-16bit-mono-pcm` directly (measured 200 OK by Morpheus 2026-07-27),
//! so there is **no decode and no resample**; and one SSML document can carry a
//! `<voice>` element per segment, so a whole scene is one HTTP request.
//!
//! Credentials come from `~/.config/speech-to-cli/config.json` — the same custody
//! as `speech-to-cli`, so a key or region change there propagates here and no
//! parallel secret store is invented (spec §7.6). No key is ever logged, `Debug`
//! is redacted, and no key is committed.

use crate::backend::{Availability, CostClass, Gender, RenderRequest, TtsBackend, VoiceDesc};
use crate::error::{Result, TtsError};
use crate::pcm::Pcm16k;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

/// 16 kHz mono s16le, raw and headerless — already the plugin-boundary contract,
/// which is why the Azure path has no resampler.
pub const OUTPUT_FORMAT: &str = "raw-16khz-16bit-mono-pcm";

/// DragonHD is region-limited; `eastus` is where JP's deployment lives.
pub const DEFAULT_REGION: &str = "eastus";

const DEFAULT_VOICE: &str = "en-US-Ava:DragonHDLatestNeural";

/// Concurrent in-flight requests in `render_batch`. Deliberately modest: reqwest
/// multiplexes these over one HTTP/2 connection, and Azure answers a burst with
/// 429s rather than more throughput.
pub const BATCH_CONCURRENCY: usize = 4;

/// Characters allowed in an SSML attribute value. Mirrors `speech-to-cli`'s
/// `_SSML_SAFE_RE`, which is the same guard on the same voice strings.
fn is_attr_safe(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '+' | '%' | '(' | ')' | ' ')
}

/// Escape the five XML predefined entities.
///
/// `&` **must** go first, or `<` becomes `&lt;` and then its `&` becomes
/// `&amp;lt;`. Apostrophes and double quotes are guaranteed to appear in
/// dialogue — `"Don't,"` is the most ordinary line in the book — and unescaped
/// they produce invalid SSML and a 400 from Azure.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

const SPEAK_OPEN: &str =
    r#"<speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" xml:lang="en-US">"#;

/// One `<voice>` element per segment, in order, in a single `<speak>` document.
///
/// This is the request body Azure's `render_joined` sends: one HTTP request for a
/// whole scene, voices switching inside a single returned audio stream. Blank
/// segments are skipped rather than emitted as empty `<voice>` elements, which
/// Azure rejects.
pub fn build_multi_voice_ssml(reqs: &[RenderRequest]) -> String {
    let mut out = String::from(SPEAK_OPEN);
    for r in reqs {
        if r.is_blank() {
            continue;
        }
        // A voice name that failed validation cannot reach here through
        // `AzureBackend`, which checks every request up front; escape defensively
        // so this function is safe to call directly.
        out.push_str("<voice name=\"");
        out.push_str(&xml_escape(&r.voice.remainder));
        out.push_str("\">");
        out.push_str(&xml_escape(&r.text));
        out.push_str("</voice>");
    }
    out.push_str("</speak>");
    out
}

/// Single-segment SSML — the same builder with one element, so escaping and
/// document shape cannot drift between the two paths.
pub fn build_ssml(req: &RenderRequest) -> String {
    build_multi_voice_ssml(std::slice::from_ref(req))
}

/// Only the fields this plugin needs, so an unrelated `speech-to-cli` setting
/// changing shape cannot break story rendering.
#[derive(Debug, Deserialize)]
struct SpeechToCliConfig {
    key: Option<String>,
    tts_key: Option<String>,
    region: Option<String>,
    tts_region: Option<String>,
    voice: Option<String>,
}

/// Resolved Azure credentials.
#[derive(Clone)]
pub struct AzureConfig {
    /// Subscription key. **Never logged, never printed** — see the `Debug` impl.
    pub key: String,
    /// TTS region (`tts_region`, falling back to `region`).
    pub region: String,
    /// Voice used when a request carries no usable remainder.
    pub default_voice: String,
}

impl core::fmt::Debug for AzureConfig {
    /// Redacts the key. A `dbg!` or a `tracing::debug!` on a config struct is the
    /// most likely way a subscription key ends up in a log file.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AzureConfig")
            .field("key", &format_args!("<redacted {} chars>", self.key.len()))
            .field("region", &self.region)
            .field("default_voice", &self.default_voice)
            .finish()
    }
}

impl AzureConfig {
    /// `~/.config/speech-to-cli/config.json`.
    pub fn config_path() -> PathBuf {
        if let Ok(p) = std::env::var("SPEECH_TO_CLI_CONFIG") {
            return PathBuf::from(p);
        }
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(home).join(".config/speech-to-cli/config.json")
    }

    /// Load from the environment, then the shared config file.
    ///
    /// `AZURE_SPEECH_KEY` wins when set, so a deployment on `familiar` can be
    /// provisioned from Vaultwarden without a config file at all.
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        let from_file = match std::fs::read_to_string(&path) {
            Ok(s) => Some(Self::from_json_str(&s).map_err(|e| match e {
                TtsError::ConfigParse { source, .. } => TtsError::ConfigParse {
                    path: path.display().to_string(),
                    source,
                },
                other => other,
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(TtsError::ConfigRead {
                    path: path.display().to_string(),
                    source,
                });
            }
        };

        let env_key = std::env::var("AZURE_SPEECH_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        let env_region = std::env::var("AZURE_SPEECH_REGION")
            .ok()
            .filter(|r| !r.is_empty());

        match from_file {
            // The file is present: env vars override individual fields.
            Some(Ok(mut cfg)) => {
                if let Some(k) = env_key {
                    cfg.key = k;
                }
                if let Some(r) = env_region {
                    cfg.region = r;
                }
                Ok(cfg)
            }
            // The file is present but unusable. An env key still gets us running;
            // otherwise report the file's own error, which is more informative
            // than "missing credential".
            Some(Err(file_err)) => match env_key {
                Some(key) => Ok(Self {
                    key,
                    region: env_region.unwrap_or_else(|| DEFAULT_REGION.to_string()),
                    default_voice: DEFAULT_VOICE.to_string(),
                }),
                None => Err(file_err),
            },
            None => match env_key {
                Some(key) => Ok(Self {
                    key,
                    region: env_region.unwrap_or_else(|| DEFAULT_REGION.to_string()),
                    default_voice: DEFAULT_VOICE.to_string(),
                }),
                None => Err(TtsError::MissingCredential(format!(
                    "neither AZURE_SPEECH_KEY nor {}",
                    path.display()
                ))),
            },
        }
    }

    /// Parse a `speech-to-cli` config document.
    ///
    /// `tts_key` falls back to `key`, and `tts_region` to `region` — DragonHD is
    /// only in `eastus` while STT runs in `westus`, so the two regions genuinely
    /// differ and picking the wrong one yields a confusing 404.
    pub fn from_json_str(s: &str) -> Result<Self> {
        let raw: SpeechToCliConfig =
            serde_json::from_str(s).map_err(|source| TtsError::ConfigParse {
                path: "<config json>".to_string(),
                source,
            })?;

        let key = raw
            .tts_key
            .filter(|k| !k.is_empty())
            .or(raw.key.filter(|k| !k.is_empty()))
            .ok_or_else(|| {
                TtsError::MissingCredential("config has neither 'tts_key' nor 'key'".into())
            })?;

        Ok(Self {
            key,
            region: raw
                .tts_region
                .filter(|r| !r.is_empty())
                .or(raw.region.filter(|r| !r.is_empty()))
                .unwrap_or_else(|| DEFAULT_REGION.to_string()),
            default_voice: raw
                .voice
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_VOICE.to_string()),
        })
    }

    /// The `cognitiveservices/v1` synthesis endpoint for this region.
    pub fn endpoint(&self) -> String {
        format!(
            "https://{}.tts.speech.microsoft.com/cognitiveservices/v1",
            self.region
        )
    }

    /// Reject a voice name that could break out of the `name="…"` attribute.
    ///
    /// Refused rather than silently replaced with a default: `speech-to-cli`
    /// substitutes because a mispronounced notification is harmless, but shipping
    /// a chapter in the wrong narrator's voice is not.
    pub fn validate_voice_name(name: &str) -> Result<()> {
        if name.is_empty() || name.len() > 96 || !name.chars().all(is_attr_safe) {
            return Err(TtsError::InvalidVoiceName(name.to_string()));
        }
        Ok(())
    }
}

/// The curated Azure character pool: **20 voices, all verified live**, balanced
/// 10 female / 10 male and 4 en-GB / 16 en-US.
///
/// Balance is deliberate, not cosmetic: the engine's cast assigner interleaves gender
/// and accent groups, so a lopsided pool defeats it and characters start doubling up
/// on one side. The pool was 6 usable character voices, and a four-character chapter
/// already had two characters drawing the same voice.
///
/// ## Verified two ways, every entry
///
/// 1. **Catalog membership** — `GET /cognitiveservices/voices/list` (one request,
///    validates the whole table). See `AzureBackend::fetch_catalog`.
/// 2. **Live synthesis** — a one-word render per voice, 34 candidates checked,
///    34 passed. Gender and locale below are Azure's **own** catalog metadata, not a
///    guess from the name.
///
/// Both checks are `#[ignore]`d tests in `tests/azure_live.rs`. Run them after editing
/// this table — an unverified entry here previously cost a chapter's audio.
///
/// ## What is deliberately excluded
///
/// - **`en-GB-OllieMultilingual:DragonHDLatestNeural`** — never existed. It was
///   `en-GB-OllieMultilingualNeural` (a real *non*-DragonHD voice) spliced onto the
///   DragonHD suffix, and Azure answers a made-up name with HTTP 400. The real voice
///   is `en-GB-Ollie:DragonHDLatestNeural`, present below. **A 400 from Azure means
///   "check the name" before it means "voice unavailable".**
/// - **`-Preview` and numeric-suffix variants** (`Andrew-Preview`, `Andrew2`,
///   `Andrew3`, `Ava-Preview`, `Ava3`, `Emma2`, `Serena-Preview`). All verified
///   working, all excluded: two near-identical voices are worse than a smaller pool,
///   because the assigner will confidently hand both out and they read as one person.
///   Distinguishing them needs a listening test, which is not something this crate
///   can do.
/// - **`DragonHDFlash` and `DragonHDOmni`** — separate model series. All 8 verified
///   working (`Jimmie`/`Tiana`/`Tyler` Flash; `Andrew`/`Caleb`/`Dana`/`Lewis`/`Phoebe`
///   Omni), but kept out of the default pool: mixing series inside one chapter is a
///   timbre-and-loudness consistency risk of exactly the kind the fused-`loudnorm`
///   defect turned out to be. Add them via `AzureBackend::with_voices` once a chapter
///   has been listened to end to end.
/// - **`en-US-Davis`** — reserved as JP's orchestrator voice (`agent-orchestration`).
///   Not a technical limit: hearing it narrate a character would muddy the by-voice
///   classification JP relies on when agents speak. Mirror-image reasoning keeps
///   **`en-GB-Ada`** available as the narrator — it is Ember's roster voice, so the
///   local model narrating in it preserves the accent signature.
/// - **`en-Multitalker`** and the lowercase Omni novelty ids (`en-us-jelly`,
///   `en-us-blushzephyr`, …) — not single-character voices.
///
/// Verified spares, available by editing this list: female `Evelyn`, `Jenny`, `Mila`,
/// `Serena`, `Tiana`; male `Jimmie`. All returned 200 with real audio.
const DRAGONHD_VOICES: &[(&str, &str, &str, Gender)] = &[
    (
        "en-GB-Ada:DragonHDLatestNeural",
        "Ada (DragonHD)",
        "en-GB",
        Gender::Female,
    ),
    (
        "en-GB-Sonia:DragonHDLatestNeural",
        "Sonia (DragonHD)",
        "en-GB",
        Gender::Female,
    ),
    (
        "en-GB-Ollie:DragonHDLatestNeural",
        "Ollie (DragonHD)",
        "en-GB",
        Gender::Male,
    ),
    (
        "en-GB-Ryan:DragonHDLatestNeural",
        "Ryan (DragonHD)",
        "en-GB",
        Gender::Male,
    ),
    (
        "en-US-Aria:DragonHDLatestNeural",
        "Aria (DragonHD)",
        "en-US",
        Gender::Female,
    ),
    (
        "en-US-Ava:DragonHDLatestNeural",
        "Ava (DragonHD)",
        "en-US",
        Gender::Female,
    ),
    (
        "en-US-Bree:DragonHDLatestNeural",
        "Bree (DragonHD)",
        "en-US",
        Gender::Female,
    ),
    (
        "en-US-Emma:DragonHDLatestNeural",
        "Emma (DragonHD)",
        "en-US",
        Gender::Female,
    ),
    (
        "en-US-Jane:DragonHDLatestNeural",
        "Jane (DragonHD)",
        "en-US",
        Gender::Female,
    ),
    (
        "en-US-Nova:DragonHDLatestNeural",
        "Nova (DragonHD)",
        "en-US",
        Gender::Female,
    ),
    (
        "en-US-Phoebe:DragonHDLatestNeural",
        "Phoebe (DragonHD)",
        "en-US",
        Gender::Female,
    ),
    (
        "en-US-Tessa:DragonHDLatestNeural",
        "Tessa (DragonHD)",
        "en-US",
        Gender::Female,
    ),
    (
        "en-US-Adam:DragonHDLatestNeural",
        "Adam (DragonHD)",
        "en-US",
        Gender::Male,
    ),
    (
        "en-US-Alloy:DragonHDLatestNeural",
        "Alloy (DragonHD)",
        "en-US",
        Gender::Male,
    ),
    (
        "en-US-Andrew:DragonHDLatestNeural",
        "Andrew (DragonHD)",
        "en-US",
        Gender::Male,
    ),
    (
        "en-US-Brian:DragonHDLatestNeural",
        "Brian (DragonHD)",
        "en-US",
        Gender::Male,
    ),
    (
        "en-US-Juno:DragonHDLatestNeural",
        "Juno (DragonHD)",
        "en-US",
        Gender::Male,
    ),
    (
        "en-US-Steffan:DragonHDLatestNeural",
        "Steffan (DragonHD)",
        "en-US",
        Gender::Male,
    ),
    (
        "en-US-Tyler:DragonHDLatestNeural",
        "Tyler (DragonHD)",
        "en-US",
        Gender::Male,
    ),
    (
        "en-US-Vance:DragonHDLatestNeural",
        "Vance (DragonHD)",
        "en-US",
        Gender::Male,
    ),
];

/// Same-locale, same-gender substitutes, used only when a fallback is explicitly
/// enabled (see [`AzureConfig::fallback_on_reject`]).
const FALLBACK_BY_LOCALE: &[(&str, Gender, &str)] = &[
    ("en-GB", Gender::Female, "en-GB-Ada:DragonHDLatestNeural"),
    ("en-GB", Gender::Male, "en-GB-Ollie:DragonHDLatestNeural"),
    ("en-US", Gender::Female, "en-US-Ava:DragonHDLatestNeural"),
    ("en-US", Gender::Male, "en-US-Andrew:DragonHDLatestNeural"),
];

/// The verdict of an Azure-side cast preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoicePreflight {
    /// Azure's catalog lists this voice in this region.
    Ok { voice_ref: String },
    /// Well-formed and attribute-safe, but **Azure has never heard of it**. This is
    /// the `en-GB-OllieMultilingual` failure; it renders as HTTP 400 at synthesis.
    NotInCatalog {
        voice_ref: String,
        region: String,
        /// Closest catalog entry, when one is obviously similar.
        suggestion: Option<String>,
    },
    /// The `voice_ref` names a different backend.
    WrongBackend { voice_ref: String, backend: String },
    /// Unparseable, or a name that could inject SSML markup.
    Malformed { voice_ref: String, reason: String },
}

impl VoicePreflight {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    pub fn voice_ref(&self) -> &str {
        match self {
            Self::Ok { voice_ref }
            | Self::NotInCatalog { voice_ref, .. }
            | Self::WrongBackend { voice_ref, .. }
            | Self::Malformed { voice_ref, .. } => voice_ref,
        }
    }
}

impl core::fmt::Display for VoicePreflight {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Ok { voice_ref } => write!(f, "{voice_ref}: ok"),
            Self::NotInCatalog {
                voice_ref,
                region,
                suggestion,
            } => {
                write!(f, "{voice_ref}: not in the Azure catalog for {region}")?;
                if let Some(s) = suggestion {
                    write!(f, " (did you mean '{s}'?)")?;
                }
                Ok(())
            }
            Self::WrongBackend { voice_ref, backend } => {
                write!(f, "{voice_ref}: belongs to backend '{backend}', not azure")
            }
            Self::Malformed { voice_ref, reason } => write!(f, "{voice_ref}: {reason}"),
        }
    }
}

/// Closest catalog entry by shared prefix — enough to turn "not in the catalog" into
/// "you wrote `en-GB-OllieMultilingual:…`, Azure has `en-GB-Ollie:…`".
fn nearest_name(name: &str, catalog: &[String]) -> Option<String> {
    let shared = |a: &str, b: &str| a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    catalog
        .iter()
        .map(|c| (shared(name, c), c))
        .filter(|(n, _)| *n >= 8)
        .max_by_key(|(n, _)| *n)
        .map(|(_, c)| c.clone())
}

/// The Azure DragonHD backend.
pub struct AzureBackend {
    config: AzureConfig,
    client: reqwest::Client,
    voices: Vec<VoiceDesc>,
    /// Substitute a known-good voice when Azure **rejects** one (HTTP 4xx).
    ///
    /// **Off by default, deliberately.** Voices are per-character persisted state
    /// (spec §7.3) — that persistence is what makes a cast feel like continuity
    /// rather than a lottery. Silently recasting a character for one chapter breaks
    /// exactly the property the design exists to protect, and does it invisibly.
    ///
    /// With per-segment isolation and [`AzureBackend::preflight`], a bad voice now
    /// costs one segment and is catchable at cast-assignment time, so this is a third
    /// line of defence rather than the main one. Enable it when finishing a chapter
    /// matters more than keeping a character's voice stable; every substitution is
    /// logged to stderr.
    fallback_on_reject: bool,
}

impl AzureBackend {
    /// Build from an already-resolved config.
    pub fn new(config: AzureConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .unwrap_or_default();
        let voices = Self::default_voices(&config);
        Self {
            config,
            client,
            voices,
            fallback_on_reject: false,
        }
    }

    /// Enable same-locale, same-gender voice substitution on an Azure 4xx.
    /// See [`AzureBackend::fallback_on_reject`] for why this is opt-in.
    #[must_use]
    pub fn with_fallback_on_reject(mut self, enabled: bool) -> Self {
        self.fallback_on_reject = enabled;
        self
    }

    /// Build from `AZURE_SPEECH_KEY` / `~/.config/speech-to-cli/config.json`.
    pub fn from_default_config() -> Result<Self> {
        Ok(Self::new(AzureConfig::load()?))
    }

    /// Replace the advertised voice list.
    #[must_use]
    pub fn with_voices(mut self, voices: Vec<VoiceDesc>) -> Self {
        self.voices = voices;
        self
    }

    pub fn config(&self) -> &AzureConfig {
        &self.config
    }

    /// Whether a rejected voice falls back to a substitute. Off unless opted in.
    pub fn fallback_on_reject(&self) -> bool {
        self.fallback_on_reject
    }

    /// Every voice name Azure itself reports for this region, from
    /// `GET /cognitiveservices/voices/list`.
    ///
    /// **This is the Azure-side preflight.** sherpa cannot ship a bad cast because
    /// [`crate::sherpa::SherpaConfig::preflight`] refuses to load an incomplete model
    /// up front; Azure had no equivalent, so a made-up voice name was only discovered
    /// at render time, mid-chapter. One request validates every name a cast could
    /// draw — far cheaper than N synthesis calls, and authoritative rather than
    /// curated.
    ///
    /// Costs one metered-tier-free catalog request and no synthesis.
    pub async fn fetch_catalog(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Entry {
            #[serde(rename = "ShortName")]
            short_name: String,
        }
        let url = format!(
            "https://{}.tts.speech.microsoft.com/cognitiveservices/voices/list",
            self.config.region
        );
        let resp = self
            .client
            .get(&url)
            .header("Ocp-Apim-Subscription-Key", &self.config.key)
            .header("User-Agent", "litrpg-tts")
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(TtsError::HttpStatus {
                status: status.as_u16(),
                body: body.chars().take(300).collect(),
            });
        }
        let entries: Vec<Entry> = resp.json().await?;
        Ok(entries.into_iter().map(|e| e.short_name).collect())
    }

    /// Check cast voice refs against Azure's catalog **before the first render**.
    ///
    /// Returns one verdict per input, in order. The engine should call this at
    /// cast-assignment time: a bad voice then is a config fix, whereas the same bad
    /// voice at render time is a lost chapter.
    pub async fn preflight(&self, voice_refs: &[&str]) -> Result<Vec<VoicePreflight>> {
        let catalog = self.fetch_catalog().await?;
        Ok(voice_refs
            .iter()
            .map(|vr| self.preflight_one(vr, &catalog))
            .collect())
    }

    /// [`AzureBackend::preflight`] against an already-fetched catalog, so a whole
    /// cast costs one request.
    pub fn preflight_one(&self, voice_ref: &str, catalog: &[String]) -> VoicePreflight {
        let name = match litrpg_core::VoiceRef::parse(voice_ref) {
            Ok(v) if v.backend == "azure" => v.remainder,
            Ok(v) => {
                return VoicePreflight::WrongBackend {
                    voice_ref: voice_ref.to_string(),
                    backend: v.backend,
                };
            }
            Err(e) => {
                return VoicePreflight::Malformed {
                    voice_ref: voice_ref.to_string(),
                    reason: e.to_string(),
                };
            }
        };
        if AzureConfig::validate_voice_name(&name).is_err() {
            return VoicePreflight::Malformed {
                voice_ref: voice_ref.to_string(),
                reason: "voice name is not attribute-safe".into(),
            };
        }
        if catalog.iter().any(|c| c == &name) {
            VoicePreflight::Ok {
                voice_ref: voice_ref.to_string(),
            }
        } else {
            // The exact failure that cost a chapter: a plausible-looking name that
            // Azure has never heard of.
            VoicePreflight::NotInCatalog {
                voice_ref: voice_ref.to_string(),
                region: self.config.region.clone(),
                suggestion: nearest_name(&name, catalog),
            }
        }
    }

    /// A same-locale, same-gender substitute drawn from the curated table, or `None`.
    fn fallback_for(&self, voice_ref: &str) -> Option<String> {
        let name = litrpg_core::VoiceRef::parse(voice_ref).ok()?.remainder;
        let desc = DRAGONHD_VOICES.iter().find(|(n, ..)| *n == name);
        let (lang, gender) = match desc {
            Some((_, _, lang, gender)) => (*lang, *gender),
            // Unknown voice: infer the locale from the name prefix, guess female.
            None => (
                if name.starts_with("en-GB") {
                    "en-GB"
                } else {
                    "en-US"
                },
                Gender::Female,
            ),
        };
        FALLBACK_BY_LOCALE
            .iter()
            .find(|(l, g, sub)| *l == lang && *g == gender && *sub != name)
            .or_else(|| FALLBACK_BY_LOCALE.iter().find(|(_, _, sub)| *sub != name))
            .map(|(_, _, sub)| (*sub).to_string())
    }

    fn default_voices(config: &AzureConfig) -> Vec<VoiceDesc> {
        let mut out: Vec<VoiceDesc> = DRAGONHD_VOICES
            .iter()
            .map(|(name, label, lang, gender)| VoiceDesc {
                voice_ref: format!("azure:{name}"),
                label: (*label).to_string(),
                lang: (*lang).to_string(),
                gender: *gender,
                cost_class: CostClass::Metered,
            })
            .collect();
        // Whatever `speech-to-cli` is configured to use is assignable here too.
        let configured = format!("azure:{}", config.default_voice);
        if !out.iter().any(|v| v.voice_ref == configured) {
            out.push(VoiceDesc {
                voice_ref: configured,
                label: format!("{} (configured)", config.default_voice),
                lang: "en-US".to_string(),
                gender: Gender::Unknown,
                cost_class: CostClass::Metered,
            });
        }
        out
    }

    /// Validate every voice name before spending anything.
    fn check(reqs: &[RenderRequest]) -> Result<()> {
        for r in reqs {
            AzureConfig::validate_voice_name(&r.voice.remainder)?;
        }
        Ok(())
    }

    /// Render one segment, optionally retrying a **rejected voice** with a substitute.
    ///
    /// The retry fires only on HTTP 4xx — Azure saying "that voice is wrong" — never
    /// on a 5xx or a network error, where the voice is fine and the right response is
    /// to retry the same voice later rather than silently recast a character.
    async fn render_one(&self, req: &RenderRequest) -> Result<Pcm16k> {
        match self.post_ssml(build_ssml(req)).await {
            Err(TtsError::HttpStatus { status, body }) if (400..500).contains(&status) => {
                let Some(sub) = self
                    .fallback_on_reject
                    .then(|| self.fallback_for(&req.voice.to_string()))
                    .flatten()
                else {
                    return Err(TtsError::HttpStatus { status, body });
                };
                let mut retry = req.clone();
                retry.voice = litrpg_core::VoiceRef {
                    backend: "azure".into(),
                    remainder: sub.clone(),
                };
                // Loud on purpose: a voice substitution is a story-continuity event,
                // not a transparent recovery. See `AzureConfig::fallback_on_reject`.
                eprintln!(
                    "litrpg-tts: azure rejected voice '{}' ({status}) for segment {}; \
                     falling back to '{sub}'. The cast table should be fixed.",
                    req.voice.remainder, req.idx
                );
                self.post_ssml(build_ssml(&retry)).await
            }
            other => other,
        }
    }

    /// POST one SSML document and return the raw PCM body.
    async fn post_ssml(&self, ssml: String) -> Result<Pcm16k> {
        let resp = self
            .client
            .post(self.config.endpoint())
            .header("Ocp-Apim-Subscription-Key", &self.config.key)
            .header("Content-Type", "application/ssml+xml")
            .header("X-Microsoft-OutputFormat", OUTPUT_FORMAT)
            .header("User-Agent", "litrpg-tts")
            .body(ssml.into_bytes())
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            // Truncated, and Azure error bodies never echo the key.
            let body = resp.text().await.unwrap_or_default();
            return Err(TtsError::HttpStatus {
                status: status.as_u16(),
                body: body.chars().take(300).collect(),
            });
        }

        let bytes = resp.bytes().await?;
        // An odd length means a truncated stream — surface it rather than let a
        // half sample shift every subsequent segment by one byte. Then align to a
        // whole millisecond, same as the sherpa path, so no caller has to know
        // which backend produced a buffer. (Measured Azure responses happen to land
        // on whole milliseconds already, so this is usually a no-op — but relying on
        // that would be relying on an undocumented server behaviour.)
        Ok(Pcm16k::new(bytes.to_vec())?.padded_to_whole_ms())
    }
}

#[async_trait]
impl TtsBackend for AzureBackend {
    fn id(&self) -> &str {
        "azure"
    }

    fn available(&self) -> Availability {
        if self.config.key.is_empty() {
            Availability::missing("no Azure subscription key resolved")
        } else {
            Availability::Ready
        }
    }

    fn voices(&self) -> Vec<VoiceDesc> {
        self.voices.clone()
    }

    async fn render(&self, req: &RenderRequest) -> Result<Pcm16k> {
        AzureConfig::validate_voice_name(&req.voice.remainder)?;
        if req.is_blank() {
            return Ok(Pcm16k::empty());
        }
        self.render_one(req).await
    }

    /// Per-segment PCM, rendered with bounded concurrency over one pooled HTTP/2
    /// connection.
    ///
    /// **Why not one multi-voice request here:** a `Vec<Pcm16k>` needs segment
    /// boundaries, and the `cognitiveservices/v1` REST endpoint returns audio
    /// bytes with no boundary metadata — `<bookmark>` marks are delivered as SDK
    /// websocket events, not in the REST body. So the single-request shape cannot
    /// answer "where does segment 4 end", and anything writing a manifest needs
    /// exactly that. The batching win Azure *can* deliver here is concurrency;
    /// the one-request shape lives in [`TtsBackend::render_joined`].
    async fn render_batch_partial(&self, reqs: &[RenderRequest]) -> Vec<Result<Pcm16k>> {
        let mut out: Vec<Option<Result<Pcm16k>>> = (0..reqs.len()).map(|_| None).collect();

        let positions: Vec<usize> = (0..reqs.len()).collect();
        for chunk in positions.chunks(BATCH_CONCURRENCY) {
            let mut set = tokio::task::JoinSet::new();
            for &pos in chunk {
                let r = &reqs[pos];
                // Per-segment validation, so one unsafe voice name costs one segment
                // rather than the whole batch.
                if let Err(e) = AzureConfig::validate_voice_name(&r.voice.remainder) {
                    out[pos] = Some(Err(e));
                    continue;
                }
                if r.is_blank() {
                    out[pos] = Some(Ok(Pcm16k::empty()));
                    continue;
                }
                let this = self.clone_handle();
                let req = r.clone();
                set.spawn(async move { (pos, this.render_one(&req).await) });
            }
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok((pos, result)) => out[pos] = Some(result),
                    // A panicked task must not poison the rest of the batch.
                    Err(e) => {
                        if let Some(slot) = out.iter_mut().find(|s| s.is_none()) {
                            *slot = Some(Err(TtsError::Worker(e.to_string())));
                        }
                    }
                }
            }
        }

        out.into_iter()
            .enumerate()
            .map(|(i, slot)| {
                slot.unwrap_or_else(|| {
                    Err(TtsError::Worker(format!("segment {i} produced no outcome")))
                })
            })
            .collect()
    }

    /// **One** multi-voice SSML request for the whole batch (spec §7.2).
    ///
    /// Voices switch inside a single returned stream, so N segments cost one
    /// round trip instead of N. Per-segment boundaries are not recoverable from
    /// the response — use `render_batch` when timings are needed.
    async fn render_joined(&self, reqs: &[RenderRequest]) -> Result<Pcm16k> {
        Self::check(reqs)?;
        if reqs.iter().all(RenderRequest::is_blank) {
            return Ok(Pcm16k::empty());
        }
        self.post_ssml(build_multi_voice_ssml(reqs)).await
    }
}

impl AzureBackend {
    /// A cheap clone for spawned tasks — `reqwest::Client` is an `Arc` internally,
    /// so this shares the connection pool rather than duplicating it.
    fn clone_handle(&self) -> Self {
        Self {
            config: self.config.clone(),
            client: self.client.clone(),
            voices: Vec::new(),
            fallback_on_reject: self.fallback_on_reject,
        }
    }
}
