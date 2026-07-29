//! Backend registry — the engine's only entry point into the plugin layer.

use crate::backend::{Availability, RenderRequest, TtsBackend, VoiceDesc};
use crate::error::{Result, TtsError};
use crate::pcm::Pcm16k;
use litrpg_core::VoiceRef;

/// Holds the registered plugins and routes a `voice_ref` to its owner.
///
/// Registration order is preserved and is the order voices appear in
/// [`TtsRegistry::all_voices`], which feeds the daemon's `GET /api/voices`.
#[derive(Default)]
pub struct TtsRegistry {
    backends: Vec<Box<dyn TtsBackend>>,
}

impl TtsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin, refusing a duplicate id — two plugins claiming
    /// `"sherpa"` would make dispatch silently order-dependent.
    pub fn register(&mut self, backend: Box<dyn TtsBackend>) -> Result<()> {
        if self.backends.iter().any(|b| b.id() == backend.id()) {
            return Err(TtsError::DuplicateBackend(backend.id().to_string()));
        }
        self.backends.push(backend);
        Ok(())
    }

    /// Builder-style [`TtsRegistry::register`]. Panics on a duplicate id, which
    /// is a startup-wiring bug, not a runtime condition.
    #[must_use]
    pub fn with(mut self, backend: Box<dyn TtsBackend>) -> Self {
        let id = backend.id().to_string();
        self.register(backend)
            .unwrap_or_else(|_| panic!("duplicate TTS backend id '{id}'"));
        self
    }

    pub fn len(&self) -> usize {
        self.backends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }

    pub fn ids(&self) -> Vec<&str> {
        self.backends.iter().map(|b| b.id()).collect()
    }

    /// Look up a backend by id, without checking availability.
    pub fn get(&self, id: &str) -> Option<&dyn TtsBackend> {
        self.backends
            .iter()
            .find(|b| b.id() == id)
            .map(|b| b.as_ref())
    }

    /// Resolve a `voice_ref` string to its owning backend.
    ///
    /// Three distinct failures, all typed: the reference is malformed, no plugin
    /// claims that backend id, or the plugin is registered but unusable.
    pub fn resolve(&self, voice_ref: &str) -> Result<&dyn TtsBackend> {
        self.resolve_parsed(&VoiceRef::parse(voice_ref).map_err(TtsError::VoiceRef)?)
    }

    /// [`TtsRegistry::resolve`] for an already-parsed reference.
    pub fn resolve_parsed(&self, voice: &VoiceRef) -> Result<&dyn TtsBackend> {
        let backend = self
            .get(&voice.backend)
            .ok_or_else(|| TtsError::UnknownBackend(voice.backend.clone()))?;
        match backend.available() {
            Availability::Ready => Ok(backend),
            Availability::Missing { reason } => Err(TtsError::BackendUnavailable {
                id: backend.id().to_string(),
                reason,
            }),
        }
    }

    /// Every voice from every **available** plugin, in registration order. This
    /// is the catalog behind `GET /api/voices`; an unavailable plugin contributes
    /// nothing so no unusable voice can be assigned to a character.
    pub fn all_voices(&self) -> Vec<VoiceDesc> {
        self.backends
            .iter()
            .filter(|b| b.available().is_ready())
            .flat_map(|b| b.voices())
            .collect()
    }

    /// Per-plugin availability, **including** unavailable ones — this is the
    /// diagnostic view, so it must show what is broken and why.
    pub fn availability(&self) -> Vec<(String, Availability)> {
        self.backends
            .iter()
            .map(|b| (b.id().to_string(), b.available()))
            .collect()
    }

    /// Render one segment through its owning plugin.
    pub async fn render(&self, req: &RenderRequest) -> Result<Pcm16k> {
        self.resolve_parsed(&req.voice)?.render(req).await
    }

    /// Render a whole mixed-backend chapter.
    ///
    /// Groups requests by backend so each plugin gets **one** `render_batch` call
    /// for all of its segments — that is what lets Azure batch its HTTP work and
    /// sherpa fill its worker pool — then restores the original request order so
    /// the caller can build a contiguous manifest.
    pub async fn render_all(&self, reqs: &[RenderRequest]) -> Result<Vec<Pcm16k>> {
        if reqs.is_empty() {
            return Ok(Vec::new());
        }

        // Group by backend id, preserving first-seen order for determinism, and
        // remember each request's original position.
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (pos, r) in reqs.iter().enumerate() {
            match groups.iter_mut().find(|(id, _)| *id == r.voice.backend) {
                Some((_, idxs)) => idxs.push(pos),
                None => groups.push((r.voice.backend.clone(), vec![pos])),
            }
        }

        // Resolve every backend before rendering anything: an unknown or
        // unavailable backend should cost nothing, not half a chapter of Azure
        // quota. This is the same "fail at assignment time" rule as §7.3.
        let mut resolved = Vec::with_capacity(groups.len());
        for (id, idxs) in &groups {
            let backend = self
                .get(id)
                .ok_or_else(|| TtsError::UnknownBackend(id.clone()))?;
            match backend.available() {
                Availability::Ready => resolved.push((backend, idxs)),
                Availability::Missing { reason } => {
                    return Err(TtsError::BackendUnavailable {
                        id: id.clone(),
                        reason,
                    });
                }
            }
        }

        let mut out: Vec<Option<Pcm16k>> = vec![None; reqs.len()];
        for (backend, idxs) in resolved {
            let shard: Vec<RenderRequest> = idxs.iter().map(|&i| reqs[i].clone()).collect();
            let rendered = backend.render_batch(&shard).await?;
            if rendered.len() != shard.len() {
                return Err(TtsError::Worker(format!(
                    "backend '{}' returned {} buffers for {} requests",
                    backend.id(),
                    rendered.len(),
                    shard.len()
                )));
            }
            for (&pos, pcm) in idxs.iter().zip(rendered) {
                out[pos] = Some(pcm);
            }
        }

        Ok(out.into_iter().map(|p| p.unwrap_or_default()).collect())
    }

    /// Render a mixed-backend chapter with **per-segment failure isolation**.
    ///
    /// Returns exactly one outcome per request, in order. Unlike
    /// [`TtsRegistry::render_all`], nothing is discarded because something else
    /// failed: an unknown backend, an unavailable backend, or a voice one plugin
    /// rejects costs **that segment** and no more.
    ///
    /// This is what spec §10's "a TTS failure degrades" actually requires. The
    /// strict version made one bad voice out of ten cost all ten segments' audio.
    /// The engine can now ship the nine, mark the tenth `has_audio = false`, and
    /// retry it later.
    pub async fn render_all_partial(&self, reqs: &[RenderRequest]) -> Vec<Result<Pcm16k>> {
        if reqs.is_empty() {
            return Vec::new();
        }

        // Group by backend id, preserving first-seen order.
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (pos, r) in reqs.iter().enumerate() {
            match groups.iter_mut().find(|(id, _)| *id == r.voice.backend) {
                Some((_, idxs)) => idxs.push(pos),
                None => groups.push((r.voice.backend.clone(), vec![pos])),
            }
        }

        let mut out: Vec<Option<Result<Pcm16k>>> = (0..reqs.len()).map(|_| None).collect();
        for (id, idxs) in groups {
            // A resolution failure is charged to that backend's segments only.
            let backend = match self.get(&id) {
                Some(b) => match b.available() {
                    Availability::Ready => b,
                    Availability::Missing { reason } => {
                        for pos in idxs {
                            out[pos] = Some(Err(TtsError::BackendUnavailable {
                                id: id.clone(),
                                reason: reason.clone(),
                            }));
                        }
                        continue;
                    }
                },
                None => {
                    for pos in idxs {
                        out[pos] = Some(Err(TtsError::UnknownBackend(id.clone())));
                    }
                    continue;
                }
            };

            let shard: Vec<RenderRequest> = idxs.iter().map(|&i| reqs[i].clone()).collect();
            let rendered = backend.render_batch_partial(&shard).await;
            if rendered.len() != shard.len() {
                // A plugin breaking the one-outcome-per-request contract must not
                // silently shift results onto the wrong segments.
                for pos in idxs {
                    out[pos] = Some(Err(TtsError::Worker(format!(
                        "backend '{id}' returned {} outcomes for {} requests",
                        rendered.len(),
                        shard.len()
                    ))));
                }
                continue;
            }
            for (&pos, res) in idxs.iter().zip(rendered) {
                out[pos] = Some(res);
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
}
