use ai_config_core::agent::{self, AgentOp, RestRequest};
use ai_config_core::sync::SyncStore;
use ai_config_core::{Error, Store};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tauri::Emitter;

pub struct AgentServer {
    stop: Arc<AtomicBool>,
    join: Mutex<Option<JoinHandle<()>>>,
    socket: PathBuf,
    token: PathBuf,
}

impl AgentServer {
    pub fn start(
        app: tauri::AppHandle,
        store: Arc<Store>,
        sync: Arc<SyncStore>,
    ) -> Result<Self, Error> {
        let paths = agent::paths()?;
        // Bind first. `bind` refuses to displace a live listener, so a second
        // app instance cannot rotate the token out from under the first one.
        let listener = agent::bind(&paths.socket)?;
        if listener.set_nonblocking(true).is_err() {
            let _ = fs::remove_file(&paths.socket);
            return Err(protocol_error());
        }
        let token = agent::new_token();
        if let Err(error) = agent::write_token(&paths.token, &token) {
            let _ = fs::remove_file(&paths.socket);
            return Err(error);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let socket = paths.socket.clone();
        let join = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(20)));
                        let app = app.clone();
                        let store = store.clone();
                        let sync = sync.clone();
                        let token = token.clone();
                        thread::spawn(move || {
                            let handler = |op: AgentOp| {
                                let config_changed = matches!(
                                    &op,
                                    AgentOp::Save { .. }
                                        | AgentOp::SaveEndpoint { .. }
                                        | AgentOp::SyncRun
                                );
                                let sync_changed = !matches!(
                                    &op,
                                    AgentOp::Ping
                                        | AgentOp::Read
                                        | AgentOp::ReadDecrypted
                                        | AgentOp::ReadEndpoint { .. }
                                        | AgentOp::ReadEndpointDecrypted { .. }
                                        | AgentOp::TestEndpoint { .. }
                                        | AgentOp::CheckEndpoint { .. }
                                        | AgentOp::Check
                                        | AgentOp::History
                                        | AgentOp::Test
                                        | AgentOp::Save { .. }
                                        | AgentOp::SyncRead
                                        | AgentOp::SyncHistory
                                        | AgentOp::SyncTestRepository { .. }
                                );
                                let result = agent::dispatch(&store, &sync, op);
                                if config_changed {
                                    let _ = app.emit("config-changed", ());
                                }
                                if sync_changed {
                                    let _ = app.emit("sync-changed", ());
                                }
                                result
                            };
                            let rest_store = store.clone();
                            let rest_sync = sync.clone();
                            let rest = |request: RestRequest| {
                                let changed = request.method != "GET"
                                    && request.path.starts_with("/sync/")
                                    && request.path != "/sync/test";
                                let response = crate::rest_api::handle_rest(
                                    rest_store.as_ref(),
                                    rest_sync.as_ref(),
                                    &request,
                                );
                                if changed {
                                    let _ = app.emit("sync-changed", ());
                                    if request.path == "/sync/run" {
                                        let _ = app.emit("config-changed", ());
                                    }
                                }
                                response
                            };
                            let _ = agent::serve_stream_with_rest(stream, &token, &handler, &rest);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(100));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(200)),
                }
            }
            let _ = fs::remove_file(&socket);
        });
        Ok(Self {
            stop,
            join: Mutex::new(Some(join)),
            socket: paths.socket,
            token: paths.token,
        })
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut join) = self.join.lock() {
            if let Some(join) = join.take() {
                let _ = join.join();
            }
        }
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_file(&self.token);
    }
}

impl Drop for AgentServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn protocol_error() -> Error {
    Error {
        code: "AGENT_PROTOCOL".into(),
        message: "The local agent interface could not be started.".into(),
    }
}
