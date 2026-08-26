use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, write_settings, ModelUnloadTimeout};
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, State};

#[derive(Serialize, Type)]
pub struct ModelLoadStatus {
    is_loaded: bool,
    current_model: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub fn set_model_unload_timeout(app: AppHandle, timeout: ModelUnloadTimeout) {
    let mut settings = get_settings(&app);
    settings.model_unload_timeout = timeout;
    write_settings(&app, settings);
}

#[tauri::command]
#[specta::specta]
pub fn get_model_load_status(
    transcription_manager: State<TranscriptionManager>,
) -> Result<ModelLoadStatus, String> {
    Ok(ModelLoadStatus {
        is_loaded: transcription_manager.is_model_loaded(),
        current_model: transcription_manager.get_current_model(),
    })
}

#[tauri::command]
#[specta::specta]
pub fn unload_model_manually(
    transcription_manager: State<TranscriptionManager>,
) -> Result<(), String> {
    transcription_manager
        .unload_model()
        .map_err(|e| format!("Failed to unload model: {}", e))
}

#[tauri::command]
#[specta::specta]
pub fn change_transcription_backend_setting(app: AppHandle, backend: String) -> Result<(), String> {
    let parsed = match backend.as_str() {
        "local" => crate::settings::TranscriptionBackend::Local,
        "websocket_proxy" => crate::settings::TranscriptionBackend::WebSocketProxy,
        other => return Err(format!("Unknown transcription backend: {other}")),
    };
    let mut settings = get_settings(&app);
    settings.transcription_backend = parsed;
    write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_websocket_proxy_url_setting(app: AppHandle, url: String) {
    let mut settings = get_settings(&app);
    settings.websocket_proxy_url = url;
    write_settings(&app, settings);
}
