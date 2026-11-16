use ai_config_core::sync::SyncStore;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tauri::Emitter;

pub struct SyncScheduler {
    stop: Arc<(Mutex<bool>, Condvar)>,
    join: Mutex<Option<JoinHandle<()>>>,
}
impl SyncScheduler {
    pub fn start(app: tauri::AppHandle, sync: Arc<SyncStore>) -> Self {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let flag = stop.clone();
        let join = thread::spawn(move || {
            let busy = Arc::new(AtomicBool::new(false));
            let mut cleanup_at = Instant::now();
            loop {
                if *flag.0.lock().expect("scheduler stop lock") {
                    break;
                }
                let cleanup = Instant::now() >= cleanup_at;
                let ai_changed = sync.poll_ai_change().unwrap_or(false);
                if (cleanup || ai_changed || sync.sync_due()) && !busy.swap(true, Ordering::SeqCst)
                {
                    if cleanup {
                        cleanup_at = Instant::now() + Duration::from_secs(60);
                    }
                    let (sync, app, busy, stop) =
                        (sync.clone(), app.clone(), busy.clone(), flag.clone());
                    thread::spawn(move || {
                        if cleanup {
                            if sync.cleanup().is_ok() {
                                let _ = app.emit("sync-changed", ());
                            }
                        }
                        if *stop.0.lock().expect("scheduler stop lock") {
                            busy.store(false, Ordering::SeqCst);
                            return;
                        }
                        let result = sync.run_scheduled();
                        if matches!(&result, Ok(Some(_)))
                            || result
                                .as_ref()
                                .is_err_and(|error| error.code != "SYNC_BUSY")
                        {
                            let _ = app.emit("sync-changed", ());
                            let _ = app.emit("config-changed", ());
                        }
                        busy.store(false, Ordering::SeqCst);
                    });
                }
                let guard = flag.0.lock().expect("scheduler stop lock");
                if *guard {
                    break;
                }
                let _ = flag.1.wait_timeout(guard, Duration::from_secs(1));
            }
        });
        Self {
            stop,
            join: Mutex::new(Some(join)),
        }
    }
    pub fn stop(&self) {
        if let Ok(mut flag) = self.stop.0.lock() {
            *flag = true;
            self.stop.1.notify_all();
        }
        if let Ok(mut join) = self.join.lock() {
            if let Some(join) = join.take() {
                let _ = join.join();
            }
        }
    }
}
impl Drop for SyncScheduler {
    fn drop(&mut self) {
        self.stop();
    }
}
