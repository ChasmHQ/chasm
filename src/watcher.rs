use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher, Config};
use std::path::PathBuf;
use tokio::sync::broadcast;
use std::sync::{Arc, Mutex};
use crate::compiler::Compiler;

pub async fn setup_watcher(
    path: PathBuf,
    tx: broadcast::Sender<String>,
    last_msg: Arc<Mutex<Option<String>>>,
    remappings: Arc<Mutex<Vec<String>>>,
) -> notify::Result<()> {
    let path_clone = path.clone();
    let tx_clone = tx.clone();
    let last_msg_clone = last_msg.clone();
    let remappings_clone = remappings.clone();

    tokio::task::spawn_blocking(move || {
        let mut watcher = RecommendedWatcher::new(move |res: notify::Result<Event>| {
            match res {
                Ok(event) => {
                    let is_sol = event.paths.iter().any(|p| p.extension().map_or(false, |ext| ext == "sol"));
                    if is_sol {
                        tracing::info!("Change detected in: {:?}", event.paths);

                        // Baca remappings terkini dari AppState
                        let current_remappings = remappings_clone
                            .lock()
                            .map(|g| g.clone())
                            .unwrap_or_default();

                        let compiler = Compiler::new(path_clone.clone(), current_remappings).unwrap();
                        match compiler.compile_to_json() {
                            Ok(json) => {
                                tracing::info!("Compilation successful");
                                if let Ok(mut lock) = last_msg_clone.lock() {
                                    *lock = Some(json.clone());
                                }
                                let _ = tx_clone.send(json);
                            }
                            Err(e) => {
                                tracing::error!("Compilation failed: {}", e);
                                let msg = serde_json::json!({
                                    "type": "compile_error",
                                    "error": e.to_string()
                                });
                                let _ = tx_clone.send(msg.to_string());
                            }
                        }
                    }
                },
                Err(e) => tracing::error!("watch error: {:?}", e),
            }
        }, Config::default()).unwrap();

        watcher.watch(path.as_path(), RecursiveMode::Recursive).unwrap();

        // Keep the watcher alive
        std::thread::park();
    });

    Ok(())
}
