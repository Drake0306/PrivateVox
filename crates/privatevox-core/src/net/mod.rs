//! Host mode and the remote interface (design §8.6): wire types, the blocking client, and test fakes.

pub mod protocol;

pub use protocol::{decode_pcm, encode_pcm, DictationReply, DictationRequest, ErrorReply, GpuInfo, HistoryReply, ModelsReply, PolishInfo, PolishReply, PolishRequest, RemoteModel, RemoteRow, SelectModelRequest, StatusReply, WhisperInfo, DEFAULT_HOST_PORT, MAX_PCM_SAMPLES};
