//! Consulta las sesiones multimedia que Windows expone, sin usar la selección del notch.
//! Ejecutar con `cargo run --example media_sessions` mientras las apps reproducen audio.

use windows::core::Result;
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;

fn main() -> Result<()> {
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.join()?;
    let sessions = manager.GetSessions()?;
    println!("Sesiones expuestas por Windows: {}", sessions.Size()?);
    for session in sessions {
        println!("App: {:?}", session.SourceAppUserModelId());
        println!(
            "Estado: {:?}",
            session
                .GetPlaybackInfo()
                .and_then(|info| info.PlaybackStatus())
        );
        // No imprime títulos ni otros metadatos del contenido reproducido.
        println!(
            "Metadatos accesibles: {:?}",
            session
                .TryGetMediaPropertiesAsync()
                .and_then(|op| op.join())
                .map(|_| ())
        );
    }
    Ok(())
}
