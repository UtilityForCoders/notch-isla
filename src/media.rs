//! Lectura de la música que suena en el sistema vía SMTC (Windows.Media.Control).
//!
//! Todo funciona por eventos, sin sondeo. Los eventos de WinRT llegan en hilos del sistema,
//! así que cada cambio se envía a la ventana con `PostMessageW(WM_MEDIA)`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tiny_skia::{IntSize, Pixmap};
use windows::core::{Error, Result};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapBounds, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat,
    BitmapTransform, ColorManagementMode, ExifOrientationMode,
};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as Properties,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::appicon;
use crate::island::Button;

/// Mensaje propio: `lparam` lleva un `Box<Option<NowPlaying>>` que la ventana debe liberar con `take`.
pub const WM_MEDIA: u32 = WM_APP + 1;

/// Lado de la carátula decodificada, en píxeles: alcanza para 56 px lógicos hasta 300 % de escala.
const COVER_PX: u32 = 168;

#[derive(Clone, PartialEq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    /// Álbum (puede venir vacío): ayuda a encontrar la letra exacta.
    pub album: String,
    pub playing: bool,
    pub cover: Option<Cover>,
    /// Avance de la canción; `None` si la app no lo informa (pasa con algunos navegadores).
    pub timeline: Option<Timeline>,
    /// Ícono de la app que suena (Spotify, Chrome...), si se pudo obtener.
    pub app_icon: Option<AppIcon>,
    /// Identificador de la app en SMTC ("Spotify.exe", "Chrome"...): con él se busca su volumen.
    pub app_id: String,
    /// (posición de la sesión mostrada, cantidad de apps con música). Con 2 o más, un clic en
    /// la carátula pasa a la siguiente.
    pub sessions: (usize, usize),
}

/// Ícono de una app en RGBA premultiplicado. Se compara por identidad, como `Cover`.
#[derive(Clone)]
pub struct AppIcon(pub Arc<Pixmap>);

impl PartialEq for AppIcon {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Identifica una sesión: el id de la app y, si una misma app tiene varias, cuál de ellas.
type SessionKey = (String, usize);

/// Posición de la canción en un instante. SMTC no avisa cada segundo: entre avisos la
/// posición se calcula sumando el tiempo transcurrido mientras suena.
#[derive(Clone, Copy, PartialEq)]
pub struct Timeline {
    /// Segundos desde el inicio, medidos en `read_at`.
    pub position: f32,
    /// Duración total en segundos (siempre > 0).
    pub duration: f32,
    pub read_at: Instant,
}

impl Timeline {
    /// Fracción reproducida (0 a 1) ahora mismo.
    pub fn progress(&self, playing: bool) -> f32 {
        let mut position = self.position;
        if playing {
            position += self.read_at.elapsed().as_secs_f32();
        }
        (position / self.duration).clamp(0.0, 1.0)
    }

    /// Posición actual en segundos (0 a duration).
    pub fn current_position(&self, playing: bool) -> f32 {
        let mut position = self.position;
        if playing {
            position += self.read_at.elapsed().as_secs_f32();
        }
        position.clamp(0.0, self.duration)
    }
}

/// Unidades de los tiempos de WinRT: 100 ns.
const TICKS_PER_SEC: f64 = 10_000_000.0;
/// Diferencia entre la época de WinRT (1601) y la de Unix (1970), en ticks de 100 ns.
const EPOCH_DIFF_TICKS: i64 = 116_444_736_000_000_000;

/// Carátula cuadrada en RGBA premultiplicado (el formato de tiny-skia).
/// Se compara por identidad: cada decodificación produce una carátula distinta.
#[derive(Clone)]
pub struct Cover {
    pub image: Arc<Pixmap>,
    /// Color dominante aclarado, para teñir las barras de audio de la isla contraída.
    pub accent: [u8; 3],
}

impl PartialEq for Cover {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.image, &other.image)
    }
}

/// Promedio de color de la carátula, aclarado para que se lea sobre el negro de la isla.
fn accent_of(pixmap: &Pixmap) -> [u8; 3] {
    let mut sum = [0u64; 3];
    let mut count = 0u64;
    // Una muestra de cada 4 píxeles basta para un promedio.
    for px in pixmap.pixels().iter().step_by(4) {
        let c = px.demultiply();
        sum[0] += c.red() as u64;
        sum[1] += c.green() as u64;
        sum[2] += c.blue() as u64;
        count += 1;
    }
    let avg = sum.map(|v| (v / count.max(1)) as f32);
    // Lleva el canal más alto a ~230 (conserva el tono) y lo mezcla un poco con blanco.
    let max = avg.iter().cloned().fold(1.0, f32::max);
    avg.map(|v| {
        let lifted = v * (230.0 / max);
        (lifted * 0.75 + 255.0 * 0.25) as u8
    })
}

struct Watcher {
    manager: Manager,
    /// Sesión mostrada y los tokens de sus eventos (para desuscribirse al cambiar de sesión).
    session: Option<(Session, [i64; 3])>,
    session_key: Option<SessionKey>,
    /// Sesión elegida a mano con clic en la carátula. `None` = seguir la que Windows considera
    /// actual (la última que empezó a sonar).
    selected: Option<SessionKey>,
    /// (posición de la sesión mostrada, cantidad de sesiones).
    sessions: (usize, usize),
    /// Última carátula y la canción a la que pertenece (`song_key`): no se decodifica de nuevo
    /// en cada play/pausa, y si un evento llega sin carátula para esa misma canción se conserva.
    cover: Option<(String, Cover)>,
    /// Canción (título y artista) cuya carátula no se pudo leer, y cuántas veces se intentó.
    cover_retry: Option<(String, u32)>,
    /// Íconos ya buscados, por id de app (también los que no se encontraron).
    icons: HashMap<String, IconEntry>,
    /// HWND guardado como número porque `HWND` no es `Send`.
    hwnd: isize,
}

/// Resultado de buscar el ícono de una app. Si falla se reintenta: justo al iniciar Windows,
/// o cuando la app recién abre, el shell todavía puede no conocerla.
enum IconEntry {
    Found(AppIcon),
    Missing { attempts: u32, retry_at: Instant },
}

/// Reintentos tras no encontrar un ícono, con espera creciente (5 s, 10 s, 20 s...).
const ICON_ATTEMPTS: u32 = 5;
const ICON_RETRY: Duration = Duration::from_secs(5);

/// Las búsquedas de íconos van de a una: al aparecer una app llegan varios eventos a la vez y
/// sin esto cada uno buscaría el mismo ícono en paralelo.
static ICON_LOADING: Mutex<()> = Mutex::new(());

/// Reintentos de carátula para una misma canción: a los 0,5 s, 1 s, 2 s, 4 s, 8 s y 16 s.
/// Después de un anuncio, Spotify puede tardar varios segundos en tenerla lista.
const COVER_ATTEMPTS: u32 = 6;
const COVER_RETRY: Duration = Duration::from_millis(500);

/// Lecturas fallidas seguidas de la sesión (Windows no respondió). Se reintenta unas pocas
/// veces en lugar de dar la música por terminada.
static READ_FAILURES: AtomicU32 = AtomicU32::new(0);
const READ_ATTEMPTS: u32 = 5;

/// `publish` corre de a uno (ver allí).
static PUBLISHING: Mutex<()> = Mutex::new(());

static WATCHER: Mutex<Option<Watcher>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Watcher>> {
    WATCHER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Recién iniciada la sesión de Windows, el servicio de música puede no estar listo: pedirlo
/// falla o no responde. Se espera un tiempo y se reintenta con espera creciente (1 s, 2 s,
/// 4 s... hasta 30 s), sin rendirse: si no, la isla no vería la música hasta reabrirla.
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
const INIT_RETRY: Duration = Duration::from_secs(1);
const INIT_RETRY_MAX: Duration = Duration::from_secs(30);

/// Empieza a vigilar la música en un hilo aparte para no retrasar la aparición de la isla.
pub fn start(hwnd: HWND) {
    let hwnd = hwnd.0 as isize;
    std::thread::spawn(move || {
        let mut wait = INIT_RETRY;
        while init(hwnd).is_err() {
            post(hwnd, None);
            std::thread::sleep(wait);
            wait = (wait * 2).min(INIT_RETRY_MAX);
        }
    });
}

/// Pide el administrador de SMTC, sin esperar para siempre si Windows no contesta.
fn request_manager() -> Result<Manager> {
    let op = Manager::RequestAsync()?;
    let (tx, rx) = mpsc::channel();
    op.when(move |manager| {
        let _ = tx.send(manager);
    })?;
    match rx.recv_timeout(INIT_TIMEOUT) {
        Ok(manager) => manager,
        Err(_) => {
            let _ = op.Cancel();
            Err(Error::from(E_FAIL))
        }
    }
}

fn init(hwnd: isize) -> Result<()> {
    let manager = request_manager()?;
    // Otra app empezó a sonar: la isla la sigue, aunque antes se hubiera elegido otra a mano.
    manager.CurrentSessionChanged(&TypedEventHandler::new(|_, _| {
        if let Some(w) = lock().as_mut() {
            w.selected = None;
        }
        resubscribe();
        Ok(())
    }))?;
    // Se abrió o cerró una app con música: cambian los puntos y quizá la sesión mostrada.
    manager.SessionsChanged(&TypedEventHandler::new(|_, _| {
        resubscribe();
        Ok(())
    }))?;
    *lock() = Some(Watcher {
        manager,
        session: None,
        session_key: None,
        selected: None,
        sessions: (0, 0),
        cover: None,
        cover_retry: None,
        icons: HashMap::new(),
        hwnd,
    });
    resubscribe();
    Ok(())
}

fn app_id(session: &Session) -> String {
    session
        .SourceAppUserModelId()
        .map(|id| id.to_string_lossy())
        .unwrap_or_default()
}

/// Todas las sesiones de música abiertas, con su clave.
fn list_sessions(manager: &Manager) -> Vec<(Session, SessionKey)> {
    let Ok(view) = manager.GetSessions() else {
        return Vec::new();
    };
    let mut out: Vec<(Session, SessionKey)> = Vec::new();
    for session in view {
        let id = app_id(&session);
        let nth = out.iter().filter(|(_, (other, _))| *other == id).count();
        out.push((session, (id, nth)));
    }
    out
}

/// Elige qué sesión mostrar y, si cambió, mueve las suscripciones a ella.
fn resubscribe() {
    let changed = {
        let mut guard = lock();
        let Some(w) = guard.as_mut() else { return };
        let sessions = list_sessions(&w.manager);

        // La elegida a mano si sigue abierta; si no, la actual de Windows; si no, la primera.
        let chosen = w
            .selected
            .as_ref()
            .and_then(|sel| sessions.iter().find(|(_, key)| key == sel))
            .cloned();
        if chosen.is_none() {
            w.selected = None;
        }
        let target = chosen
            .or_else(|| {
                let id = app_id(&w.manager.GetCurrentSession().ok()?);
                sessions.iter().find(|(_, key)| key.0 == id).cloned()
            })
            .or_else(|| sessions.first().cloned());

        let index = target
            .as_ref()
            .and_then(|(_, key)| sessions.iter().position(|(_, other)| other == key))
            .unwrap_or(0);
        w.sessions = (index, sessions.len());

        let key = target.as_ref().map(|(_, key)| key.clone());
        if key == w.session_key && target.is_some() == w.session.is_some() {
            false
        } else {
            if let Some((old, [props_token, playback_token, timeline_token])) = w.session.take() {
                let _ = old.RemoveMediaPropertiesChanged(props_token);
                let _ = old.RemovePlaybackInfoChanged(playback_token);
                let _ = old.RemoveTimelinePropertiesChanged(timeline_token);
            }
            w.session = target.and_then(|(session, _)| subscribe(session));
            w.session_key = key;
            true
        }
    };
    // Si no cambió la sesión, igual se publica: puede haber cambiado la cantidad de apps.
    publish(changed);
}

fn subscribe(session: Session) -> Option<(Session, [i64; 3])> {
    let props_token = session.MediaPropertiesChanged(&TypedEventHandler::new(|_, _| {
        publish(true);
        Ok(())
    }));
    // Play/pausa no cambia la carátula: se reutiliza la guardada.
    let playback_token = session.PlaybackInfoChanged(&TypedEventHandler::new(|_, _| {
        publish(false);
        Ok(())
    }));
    // Saltos de posición (o el avance que algunas apps informan cada pocos segundos).
    let timeline_token = session.TimelinePropertiesChanged(&TypedEventHandler::new(|_, _| {
        publish(false);
        Ok(())
    }));
    match (props_token, playback_token, timeline_token) {
        (Ok(a), Ok(b), Ok(c)) => Some((session, [a, b, c])),
        _ => None,
    }
}

/// Clic en la carátula: pasa a la siguiente app con música. Corre en otro hilo porque leer
/// la sesión nueva espera respuestas de WinRT y no debe trabar la ventana.
pub fn next_session() {
    std::thread::spawn(|| {
        {
            let mut guard = lock();
            let Some(w) = guard.as_mut() else { return };
            let keys: Vec<SessionKey> = list_sessions(&w.manager)
                .into_iter()
                .map(|(_, key)| key)
                .collect();
            if keys.len() < 2 {
                return;
            }
            let next = w
                .session_key
                .as_ref()
                .and_then(|current| keys.iter().position(|key| key == current))
                .map_or(0, |i| (i + 1) % keys.len());
            w.selected = Some(keys[next].clone());
        }
        resubscribe();
    });
}

/// Envía la orden de un botón a la app que suena. No espera la respuesta: el nuevo estado
/// llega solo por los eventos de la sesión.
pub fn press(button: Button) {
    let session = lock()
        .as_ref()
        .and_then(|w| w.session.as_ref().map(|s| s.0.clone()));
    if let Some(s) = session {
        let _ = match button {
            Button::Prev => s.TrySkipPreviousAsync(),
            Button::PlayPause => s.TryTogglePlayPauseAsync(),
            Button::Next => s.TrySkipNextAsync(),
            Button::Lyrics => return,
        };
    }
}

/// Salta a una fracción (0 a 1) de la canción. Como `press`, no espera la respuesta.
pub fn seek(fraction: f32) {
    let session = lock()
        .as_ref()
        .and_then(|w| w.session.as_ref().map(|s| s.0.clone()));
    let Some(s) = session else { return };
    let Ok(tl) = s.GetTimelineProperties() else {
        return;
    };
    if let (Ok(start), Ok(end)) = (tl.StartTime(), tl.EndTime()) {
        let span = (end.Duration - start.Duration) as f64;
        let target = start.Duration + (span * fraction.clamp(0.0, 1.0) as f64) as i64;
        let _ = s.TryChangePlaybackPositionAsync(target);
    }
}

/// Lee el estado de la sesión actual y lo manda a la ventana.
/// `refresh_cover` = volver a decodificar la carátula (cambió la canción o la sesión).
fn publish(refresh_cover: bool) {
    // De a uno: al cambiar de canción llegan varios eventos a la vez. Sin esto, uno que no
    // decodifica la carátula podía leer la guardada (vieja o ninguna) mientras otro decodificaba
    // la nueva, terminar después y dejar la isla con el cuadro gris.
    let _publishing = PUBLISHING.lock().unwrap_or_else(|e| e.into_inner());
    let (session, hwnd, cached, sessions, id) = {
        let guard = lock();
        let Some(w) = guard.as_ref() else { return };
        (
            w.session.as_ref().map(|s| s.0.clone()),
            w.hwnd,
            w.cover.clone(),
            w.sessions,
            w.session_key.as_ref().map(|key| key.0.clone()),
        )
    };
    let has_session = session.is_some();
    let mut info = match session.map(|s| read(&s, refresh_cover, cached)) {
        Some(Ok(info)) => {
            READ_FAILURES.store(0, Ordering::Relaxed);
            Some(info)
        }
        // Windows no respondió esta vez, pero la sesión sigue: no se borra nada (ni la
        // carátula ni la música); se vuelve a leer en un momento.
        Some(Err(_)) => {
            if READ_FAILURES.fetch_add(1, Ordering::Relaxed) < READ_ATTEMPTS {
                std::thread::spawn(|| {
                    std::thread::sleep(COVER_RETRY);
                    publish(true);
                });
                return;
            }
            None
        }
        None => None,
    };
    let mut retry = None;
    if let Some(w) = lock().as_mut() {
        match info.as_ref() {
            Some(i) => {
                let song = song_key(i);
                if let Some(cover) = &i.cover {
                    w.cover = Some((song, cover.clone()));
                    w.cover_retry = None;
                } else {
                    // Canción sin carátula: puede que la app aún no la tenga lista (pasa con
                    // Spotify al cambiar de canción o después de un anuncio). Se reintenta.
                    w.cover = None;
                    let attempts = match w.cover_retry.take() {
                        Some((same, n)) if same == song => n + 1,
                        _ => 1,
                    };
                    if attempts <= COVER_ATTEMPTS {
                        retry = Some(COVER_RETRY * 2u32.pow(attempts - 1));
                    }
                    w.cover_retry = Some((song, attempts));
                }
            }
            // Sin sesión: ya no hay música.
            None if !has_session => {
                w.cover = None;
                w.cover_retry = None;
            }
            None => {}
        }
    }
    if let Some(wait) = retry {
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            publish(true);
        });
    }
    if let Some(info) = info.as_mut() {
        info.sessions = sessions;
        info.app_icon = id.as_deref().and_then(app_icon);
        info.app_id = id.unwrap_or_default();
    }
    post(hwnd, info);
}

/// Ícono de la app `id`: el guardado, o lo busca si aún no se buscó o toca reintentar.
fn app_icon(id: &str) -> Option<AppIcon> {
    if id.is_empty() {
        return None;
    }
    let cached = |now: Instant| -> Option<Option<AppIcon>> {
        let guard = lock();
        match guard.as_ref()?.icons.get(id) {
            Some(IconEntry::Found(icon)) => Some(Some(icon.clone())),
            Some(IconEntry::Missing { attempts, retry_at })
                if *attempts >= ICON_ATTEMPTS || now < *retry_at =>
            {
                Some(None)
            }
            _ => None,
        }
    };
    if let Some(icon) = cached(Instant::now()) {
        return icon;
    }

    let _loading = ICON_LOADING.lock().unwrap_or_else(|e| e.into_inner());
    // Mientras esperaba su turno, otro hilo pudo haberlo buscado ya.
    if let Some(icon) = cached(Instant::now()) {
        return icon;
    }
    let loaded = appicon::load(id).map(|p| AppIcon(Arc::new(p)));

    let mut retry = None;
    if let Some(w) = lock().as_mut() {
        let entry = match &loaded {
            Some(icon) => IconEntry::Found(icon.clone()),
            None => {
                let attempts = match w.icons.get(id) {
                    Some(IconEntry::Missing { attempts, .. }) => attempts + 1,
                    _ => 1,
                };
                let wait = ICON_RETRY * 2u32.pow(attempts - 1);
                if attempts < ICON_ATTEMPTS {
                    retry = Some(wait);
                }
                IconEntry::Missing {
                    attempts,
                    retry_at: Instant::now() + wait,
                }
            }
        };
        w.icons.insert(id.to_string(), entry);
    }
    // Con la música en pausa pueden no llegar eventos por un buen rato: el reintento se agenda.
    if let Some(wait) = retry {
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            publish(false);
        });
    }
    loaded
}

/// Identifica una canción por título y artista, para saber de cuál es la carátula guardada.
fn song_key(info: &NowPlaying) -> String {
    format!("{}\u{1}{}", info.title, info.artist)
}

/// `cached` = la carátula guardada y su canción. Se decodifica de nuevo si se pide
/// (`refresh_cover`) o si la canción ya no es la de la guardada; si esa lectura no trae
/// carátula pero la canción es la misma, se conserva la guardada.
fn read(
    session: &Session,
    refresh_cover: bool,
    cached: Option<(String, Cover)>,
) -> Result<NowPlaying> {
    let props = session.TryGetMediaPropertiesAsync()?.join()?;
    let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;
    let title = props.Title()?.to_string_lossy();
    let artist = props.Artist()?.to_string_lossy();
    let song = format!("{title}\u{1}{artist}");
    let same_song = cached
        .as_ref()
        .filter(|(cached_song, _)| *cached_song == song)
        .map(|(_, cover)| cover.clone());
    // Sin carátula (o si no se pudo decodificar) la isla muestra el marcador gris.
    let cover = match same_song {
        Some(cover) if !refresh_cover => Some(cover),
        kept => read_cover(&props).ok().or(kept),
    };
    Ok(NowPlaying {
        title,
        artist,
        album: props.AlbumTitle()?.to_string_lossy(),
        playing,
        cover,
        timeline: read_timeline(session, playing).ok().flatten(),
        // Los completa `publish`, que sabe qué sesión es y cuántas hay.
        app_icon: None,
        app_id: String::new(),
        sessions: (0, 0),
    })
}

fn read_timeline(session: &Session, playing: bool) -> Result<Option<Timeline>> {
    let tl = session.GetTimelineProperties()?;
    let secs = |t: TimeSpan| t.Duration as f64 / TICKS_PER_SEC;
    let start = secs(tl.StartTime()?);
    let duration = secs(tl.EndTime()?) - start;
    if duration <= 0.0 {
        return Ok(None);
    }
    let mut position = secs(tl.Position()?) - start;

    // La posición es de cuando la app la informó; si está sonando, sumamos lo que pasó desde entonces.
    if playing {
        let updated = tl.LastUpdatedTime()?.UniversalTime;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| (d.as_nanos() / 100) as i64 + EPOCH_DIFF_TICKS)
            .unwrap_or(updated);
        let since = (now - updated) as f64 / TICKS_PER_SEC;
        if since > 0.0 && since < duration {
            position += since;
        }
    }
    Ok(Some(Timeline {
        position: position.clamp(0.0, duration) as f32,
        duration: duration as f32,
        read_at: Instant::now(),
    }))
}

/// Decodifica la carátula con WIC (Windows.Graphics.Imaging), ya escalada y recortada al centro
/// como un cuadrado de `COVER_PX`. Las miniaturas de YouTube son 16:9: se recortan los lados.
fn read_cover(props: &Properties) -> Result<Cover> {
    let stream = props.Thumbnail()?.OpenReadAsync()?.join()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let (w, h) = (decoder.PixelWidth()?, decoder.PixelHeight()?);

    // Escala para que el lado corto mida COVER_PX; el sobrante del lado largo se recorta.
    let k = COVER_PX as f32 / w.min(h).max(1) as f32;
    let sw = ((w as f32 * k).round() as u32).max(COVER_PX);
    let sh = ((h as f32 * k).round() as u32).max(COVER_PX);
    let transform = BitmapTransform::new()?;
    transform.SetScaledWidth(sw)?;
    transform.SetScaledHeight(sh)?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    transform.SetBounds(BitmapBounds {
        X: (sw - COVER_PX) / 2,
        Y: (sh - COVER_PX) / 2,
        Width: COVER_PX,
        Height: COVER_PX,
    })?;

    let pixels = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Rgba8,
            BitmapAlphaMode::Premultiplied,
            &transform,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .join()?
        .DetachPixelData()?;

    let size = IntSize::from_wh(COVER_PX, COVER_PX).ok_or_else(|| Error::from(E_FAIL))?;
    let pixmap = Pixmap::from_vec(pixels.to_vec(), size).ok_or_else(|| Error::from(E_FAIL))?;
    Ok(Cover {
        accent: accent_of(&pixmap),
        image: Arc::new(pixmap),
    })
}

fn post(hwnd: isize, info: Option<NowPlaying>) {
    let ptr = Box::into_raw(Box::new(info));
    unsafe {
        let sent = PostMessageW(
            Some(HWND(hwnd as *mut _)),
            WM_MEDIA,
            WPARAM(0),
            LPARAM(ptr as isize),
        );
        // Si la ventana ya no existe, nadie liberará el dato: lo hacemos aquí.
        if sent.is_err() {
            drop(Box::from_raw(ptr));
        }
    }
}

/// Recupera el dato enviado con `WM_MEDIA`.
///
/// # Safety
/// `lparam` debe venir de un mensaje `WM_MEDIA` y usarse una sola vez.
pub unsafe fn take(lparam: LPARAM) -> Option<NowPlaying> {
    *Box::from_raw(lparam.0 as *mut Option<NowPlaying>)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Diagnóstico (ignorado por defecto): lee 30 veces seguidas la carátula de lo que suena
    /// y cuenta cuántas lecturas fallan sin que cambie la canción.
    /// Con Windows ya listo, el pedido con tiempo límite responde enseguida.
    #[test]
    fn pide_el_administrador_sin_colgarse() {
        let start = Instant::now();
        request_manager().expect("SMTC no respondió");
        assert!(start.elapsed() < INIT_TIMEOUT);
    }

    #[test]
    #[ignore]
    fn lecturas_de_caratula() {
        let manager = Manager::RequestAsync().unwrap().join().unwrap();
        let session = manager.GetCurrentSession().expect("no hay música sonando");
        println!("app: {}", app_id(&session));
        let (mut ok, mut sin_props, mut sin_caratula) = (0, 0, 0);
        for _ in 0..30 {
            match session
                .TryGetMediaPropertiesAsync()
                .and_then(|op| op.join())
            {
                Err(_) => sin_props += 1,
                Ok(props) => match read_cover(&props) {
                    Ok(_) => ok += 1,
                    Err(e) => {
                        sin_caratula += 1;
                        println!("sin carátula: {e}");
                    }
                },
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        println!(
            "bien: {ok}, fallo al leer la canción: {sin_props}, fallo de carátula: {sin_caratula}"
        );
    }
}
