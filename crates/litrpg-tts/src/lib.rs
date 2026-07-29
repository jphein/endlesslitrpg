//! TTS plugin registry.
//!
//! Two first-class backends — sherpa-onnx (local, free, unmetered) and Azure
//! DragonHD — behind one trait. Every plugin returns **16 kHz mono s16le raw PCM**,
//! which is what makes mixing backends within a single chapter safe.
