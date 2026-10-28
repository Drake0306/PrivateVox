//! Runs the real worker with a real model. Needs `PRIVATEVOX_TEST_MODEL=/path/to/ggml-large-v3-turbo-q5_0.bin`.
use privatevox_engine::protocol::DecodeOptions;
use privatevox_engine::supervisor::{Engine, WorkerEngine};
use std::path::PathBuf;
use std::process::Command;

#[test]
#[ignore = "needs PRIVATEVOX_TEST_MODEL and a GPU"]
fn transcribes_hello_fixture() {
    let model = std::env::var("PRIVATEVOX_TEST_MODEL").expect("PRIVATEVOX_TEST_MODEL");
    let wav = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../privatevox-core/tests/fixtures/hello.wav");
    let mut r = hound::WavReader::open(wav).unwrap();
    let pcm: Vec<f32> = r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
    let pcm = privatevox_core::vad::pad_min(pcm, 1.25);
    let mut e = WorkerEngine::new(Command::new(env!("CARGO_BIN_EXE_privatevox-worker")));
    let load = e.load(std::path::Path::new(&model), true).unwrap();
    eprintln!("load {:?}", load);
    let t0 = std::time::Instant::now();
    let t = e.transcribe(&pcm, &DecodeOptions::default()).unwrap();
    eprintln!("decode {:?} text={:?} ctx={} reran={}", t0.elapsed(), t.text, t.audio_ctx, t.reran);
    assert!(t.text.to_lowercase().contains("hello"), "{}", t.text);
    assert_eq!(t.audio_ctx, 768);
    assert!(t0.elapsed().as_secs_f32() < 3.0);
}
