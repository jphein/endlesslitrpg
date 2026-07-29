//! Chapter artifacts on disk (spec §8).
//!
//! | File | Purpose | ~13 min |
//! |---|---|---|
//! | `NNNN.md` | canonical text, permanent | ~15 KB |
//! | `NNNN.json` | manifest: segments, voices, offsets | ~20 KB |
//! | `NNNN.mp3` | archive + podcast + Candela, permanent | ~6 MB |
//! | `NNNN.pcm` | watch playback, buffered chapters only | ~25 MB |
//!
//! `.pcm` is the **source** — both TTS plugins produce it — and `.mp3` is derived from it
//! with ffmpeg. Doing it the other way round would mean decoding a lossy file to serve the
//! watch, which is the one client that cannot decode anything.
//!
//! Writes go to a temporary file and are then renamed. A `rename` within a directory is
//! atomic, so a crash mid-write cannot leave a half-written `.pcm` that the watch would
//! happily stream as garbage — the file either exists complete or does not exist, and
//! "does not exist" is the case the resume path already handles.

use std::path::{Path, PathBuf};

use litrpg_core::Manifest;
use litrpg_tts::{Pcm16k, async_trait};

use crate::error::EngineError;
use crate::ports::Artifacts;

/// Chapter numbers are zero-padded to four digits so a directory listing sorts correctly.
pub fn chapter_stem(number: u32) -> String {
    format!("{number:04}")
}

/// Filesystem-backed artifacts, with mp3 encoding delegated to ffmpeg.
#[derive(Debug, Clone)]
pub struct FsArtifacts {
    media_dir: PathBuf,
    ffmpeg: String,
}

impl FsArtifacts {
    pub fn new(media_dir: impl Into<PathBuf>) -> Self {
        Self {
            media_dir: media_dir.into(),
            ffmpeg: "ffmpeg".to_string(),
        }
    }

    pub fn with_ffmpeg(mut self, ffmpeg: impl Into<String>) -> Self {
        self.ffmpeg = ffmpeg.into();
        self
    }

    pub fn media_dir(&self) -> &Path {
        &self.media_dir
    }

    pub fn path_for(&self, number: u32, ext: &str) -> PathBuf {
        self.media_dir
            .join(format!("{}.{ext}", chapter_stem(number)))
    }

    async fn ensure_dir(&self) -> Result<(), EngineError> {
        tokio::fs::create_dir_all(&self.media_dir)
            .await
            .map_err(|e| EngineError::Artifact {
                detail: format!("creating {}: {e}", self.media_dir.display()),
            })
    }

    /// Write via a sibling temp file, then rename.
    async fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<String, EngineError> {
        self.ensure_dir().await?;
        let tmp = path.with_extension("part");

        tokio::fs::write(&tmp, bytes)
            .await
            .map_err(|e| EngineError::Artifact {
                detail: format!("writing {}: {e}", tmp.display()),
            })?;

        tokio::fs::rename(&tmp, path)
            .await
            .map_err(|e| EngineError::Artifact {
                detail: format!("renaming {} to {}: {e}", tmp.display(), path.display()),
            })?;

        Ok(path.to_string_lossy().to_string())
    }
}

#[async_trait]
impl Artifacts for FsArtifacts {
    async fn write_text(&self, chapter: u32, text_md: &str) -> Result<String, EngineError> {
        self.write_atomic(&self.path_for(chapter, "md"), text_md.as_bytes())
            .await
    }

    async fn write_pcm(&self, chapter: u32, pcm: &Pcm16k) -> Result<String, EngineError> {
        self.write_atomic(&self.path_for(chapter, "pcm"), pcm.as_bytes())
            .await
    }

    async fn write_manifest(
        &self,
        chapter: u32,
        manifest: &Manifest,
    ) -> Result<String, EngineError> {
        let json = serde_json::to_vec_pretty(manifest).map_err(|e| EngineError::Artifact {
            detail: format!("serializing manifest: {e}"),
        })?;
        self.write_atomic(&self.path_for(chapter, "json"), &json)
            .await
    }

    async fn encode_mp3(&self, chapter: u32, pcm_path: &str) -> Result<String, EngineError> {
        self.ensure_dir().await?;
        let out = self.path_for(chapter, "mp3");
        let tmp = out.with_extension("part.mp3");

        // The input is headerless, so the format, rate and channel count must all be
        // stated explicitly -- ffmpeg cannot infer them, and guessing wrong yields audio
        // that plays at the wrong speed rather than an error.
        let status = tokio::process::Command::new(&self.ffmpeg)
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "s16le",
                "-ar",
                "16000",
                "-ac",
                "1",
                "-i",
                pcm_path,
                "-codec:a",
                "libmp3lame",
                "-qscale:a",
                "4",
            ])
            .arg(&tmp)
            .status()
            .await
            .map_err(|e| EngineError::Artifact {
                detail: format!("spawning {}: {e}", self.ffmpeg),
            })?;

        if !status.success() {
            return Err(EngineError::Artifact {
                detail: format!("{} exited with {status}", self.ffmpeg),
            });
        }

        tokio::fs::rename(&tmp, &out)
            .await
            .map_err(|e| EngineError::Artifact {
                detail: format!("renaming {} to {}: {e}", tmp.display(), out.display()),
            })?;

        Ok(out.to_string_lossy().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapter_stems_are_zero_padded_so_listings_sort() {
        assert_eq!(chapter_stem(1), "0001");
        assert_eq!(chapter_stem(42), "0042");
        assert_eq!(chapter_stem(1234), "1234");
        // Beyond four digits it simply grows rather than truncating.
        assert_eq!(chapter_stem(12345), "12345");
    }

    #[test]
    fn paths_land_in_the_media_dir_with_the_right_extensions() {
        let a = FsArtifacts::new("/srv/story");
        assert_eq!(
            a.path_for(7, "pcm").to_str().unwrap(),
            "/srv/story/0007.pcm"
        );
        assert_eq!(
            a.path_for(7, "mp3").to_str().unwrap(),
            "/srv/story/0007.mp3"
        );
        assert_eq!(
            a.path_for(7, "json").to_str().unwrap(),
            "/srv/story/0007.json"
        );
        assert_eq!(a.path_for(7, "md").to_str().unwrap(), "/srv/story/0007.md");
    }

    #[tokio::test]
    async fn writes_are_atomic_and_leave_no_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = FsArtifacts::new(dir.path());

        let path = a.write_text(3, "# Chapter 3\n").await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "# Chapter 3\n"
        );
        assert!(
            !dir.path().join("0003.part").exists(),
            "the temp file must be gone after a successful rename"
        );
    }

    #[tokio::test]
    async fn the_media_dir_is_created_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c");
        let a = FsArtifacts::new(&nested);
        a.write_pcm(1, &Pcm16k::silence_ms(5)).await.unwrap();
        assert!(nested.join("0001.pcm").exists());
    }

    #[tokio::test]
    async fn pcm_is_written_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let a = FsArtifacts::new(dir.path());
        let pcm = Pcm16k::silence_ms(10);
        let path = a.write_pcm(2, &pcm).await.unwrap();
        let read = tokio::fs::read(&path).await.unwrap();
        assert_eq!(read.len(), 10 * 32, "10 ms at 32 B/ms");
        assert_eq!(read, pcm.as_bytes());
    }

    #[tokio::test]
    async fn a_missing_ffmpeg_is_an_artifact_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let a = FsArtifacts::new(dir.path()).with_ffmpeg("definitely-not-a-real-binary-xyz");
        let pcm_path = a.write_pcm(1, &Pcm16k::silence_ms(5)).await.unwrap();
        let err = a.encode_mp3(1, &pcm_path).await.unwrap_err();
        assert!(matches!(err, EngineError::Artifact { .. }));
    }

    #[tokio::test]
    async fn the_manifest_round_trips_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let a = FsArtifacts::new(dir.path());
        let m = Manifest::new(5, vec![]);
        let path = a.write_manifest(5, &m).await.unwrap();
        let text = tokio::fs::read_to_string(&path).await.unwrap();
        let back: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back, m);
    }
}
