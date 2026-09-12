//! Scanning off the UI thread.
//!
//! The walk already saturates the machine through rayon, so all this adds is
//! one thread to own the call and a channel to hand back the finished tree.
//! Progress is read straight off the atomics the scanner is already updating,
//! which is why the UI can show live counts without any locking.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Instant;

use diskscope_core::{scan, KindTable, Progress, ScanOptions, Tree};

pub struct ScanJob {
    pub root: PathBuf,
    pub progress: Arc<Progress>,
    pub started: Instant,
    result: Receiver<io::Result<Tree>>,
}

impl ScanJob {
    pub fn spawn(root: PathBuf, opts: ScanOptions, kinds: Arc<KindTable>) -> Self {
        let (tx, result) = mpsc::channel();
        let progress = Arc::new(Progress::default());
        let thread_progress = Arc::clone(&progress);
        let thread_root = root.clone();
        std::thread::Builder::new()
            .name("diskscope-scan".into())
            .spawn(move || {
                let outcome = scan(&thread_root, &opts, &kinds, &thread_progress);
                thread_progress.finished.store(true, Ordering::Relaxed);
                // A closed receiver means the UI moved on; nothing to do.
                let _ = tx.send(outcome);
            })
            .expect("spawning one thread cannot fail on a healthy system");
        Self {
            root,
            progress,
            started: Instant::now(),
            result,
        }
    }

    /// The finished tree, once. `None` while the scan is still running.
    pub fn take_result(&self) -> Option<io::Result<Tree>> {
        self.result.try_recv().ok()
    }

    pub fn cancel(&self) {
        self.progress.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.progress.is_cancelled()
    }

    pub fn elapsed(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }
}
