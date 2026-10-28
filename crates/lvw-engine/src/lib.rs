//! LVW engine: the `Engine` trait, the whisper backend, the worker process and its supervisor.

pub mod worker {
    /// Entry point of the engine worker process (`lvw --engine-worker`). Filled in by the engine task.
    pub fn run_worker() -> i32 {
        0
    }
}
