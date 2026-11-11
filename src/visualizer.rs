//! Barras de audio reales: siguen el sonido de la app que suena en lugar de una animación.
//!
//! Un hilo "escucha" la salida de audio con WASAPI en modo loopback (lo que suena, no el
//! micrófono): en Windows 10 2004+ y 11, solo el audio del proceso de la app (y sus procesos
//! hijos, como el de audio de Chrome); si eso no se puede, el audio general. Unas 30 veces por
//! segundo separa graves, medios y agudos con una transformada de Fourier y deja los 4 niveles
//! listos para la isla. El audio se procesa en memoria y se descarta: no se guarda ni se envía.
//!
//! Con "Seguir la voz" activado, el mismo hilo busca además cuándo empieza cada sílaba cantada
//! (`VoiceDetector`), para que la letra avance con la voz.
//!
//! Solo corre mientras se ven las barras (música sonando, reproductor extendido y a la vista) o
//! la letra guiada por la voz; en cualquier otro caso el hilo termina y la CPU vuelve a 0 %.

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Result, HRESULT};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Media::Audio::{
    eConsole, eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IAgileObject, IAgileObject_Impl, BLOB, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::VT_BLOB;

use crate::audio;

/// Cantidad de barras de la isla (de graves a agudos).
pub const BARS: usize = 4;

/// Formato pedido a Windows: PCM de 16 bits, estéreo, 44,1 kHz (lo convierte si hace falta).
const RATE: u32 = 44_100;
/// Muestras por análisis: 1024 a 44,1 kHz son 23 ms, con bandas de unos 43 Hz.
const FFT: usize = 1024;
/// Cada cuánto se lee el audio y se recalculan los niveles (~30 veces por segundo).
const TICK: Duration = Duration::from_millis(33);
/// Bandas en Hz: graves, medios-graves, medios-agudos y agudos.
const BANDS: [(f32, f32); BARS] = [
    (40.0, 250.0),
    (250.0, 1000.0),
    (1000.0, 4000.0),
    (4000.0, 12000.0),
];
/// Rango visible bajo el pico reciente de cada banda, en dB.
const RANGE_DB: f32 = 30.0;

/// Niveles compartidos entre el hilo de captura y la isla.
struct Shared {
    /// 0 a 1 por barra, guardados como bits de f32.
    levels: [AtomicU32; BARS],
    /// La captura arrancó (si no, la isla usa la animación de siempre).
    running: AtomicBool,
    stop: AtomicBool,
    /// Buscar los inicios de sílaba (cuesta un poco más de CPU, solo con la opción activa).
    voice: AtomicBool,
    /// Momentos recientes en que empezó una sílaba cantada (los últimos `MAX_ONSETS`).
    onsets: Mutex<VecDeque<Instant>>,
}

/// Inicios de sílaba que se guardan: sobra para el verso que se está cantando.
const MAX_ONSETS: usize = 64;

/// Controla la captura: se le dice qué app escuchar (o ninguna) en cada cuadro.
#[derive(Default)]
pub struct Visualizer {
    /// App que se está escuchando y sus niveles.
    current: Option<(String, Arc<Shared>)>,
    /// Buscar los inicios de sílaba (se pasa a cada captura nueva).
    voice: bool,
}

impl Visualizer {
    /// Escucha la app `app_id` (el id de SMTC), o deja de escuchar con `None`. Cambiar de app
    /// reinicia la captura; repetir la misma no hace nada.
    pub fn set_target(&mut self, app_id: Option<&str>) {
        if self.current.as_ref().map(|(id, _)| id.as_str()) == app_id {
            return;
        }
        if let Some((_, shared)) = self.current.take() {
            shared.stop.store(true, Ordering::SeqCst);
        }
        let Some(app_id) = app_id else { return };
        let shared = Arc::new(Shared {
            levels: Default::default(),
            running: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            voice: AtomicBool::new(self.voice),
            onsets: Mutex::new(VecDeque::with_capacity(MAX_ONSETS)),
        });
        let thread_shared = shared.clone();
        let id = app_id.to_string();
        std::thread::spawn(move || {
            let _ = capture(&id, &thread_shared);
            thread_shared.running.store(false, Ordering::SeqCst);
        });
        self.current = Some((app_id.to_string(), shared));
    }

    /// Activa o apaga la búsqueda de inicios de sílaba (también en la captura en curso).
    pub fn set_voice(&mut self, on: bool) {
        if self.voice == on {
            return;
        }
        self.voice = on;
        if let Some((_, shared)) = &self.current {
            if let Ok(mut onsets) = shared.onsets.lock() {
                onsets.clear();
            }
            shared.voice.store(on, Ordering::SeqCst);
        }
    }

    /// Momentos recientes en que empezó una sílaba cantada, del más viejo al más nuevo, o
    /// `None` si no se están buscando (sin captura o con la opción apagada).
    pub fn onsets(&self) -> Option<Vec<Instant>> {
        let (_, shared) = self.current.as_ref()?;
        if !self.voice || !shared.running.load(Ordering::SeqCst) {
            return None;
        }
        Some(shared.onsets.lock().ok()?.iter().copied().collect())
    }

    /// Niveles actuales, o `None` si no hay captura funcionando.
    pub fn levels(&self) -> Option<[f32; BARS]> {
        let (_, shared) = self.current.as_ref()?;
        if !shared.running.load(Ordering::SeqCst) {
            return None;
        }
        Some(std::array::from_fn(|i| {
            f32::from_bits(shared.levels[i].load(Ordering::Relaxed))
        }))
    }
}

impl Drop for Visualizer {
    fn drop(&mut self) {
        self.set_target(None);
    }
}

/// El hilo de captura: abre el audio, y hasta que le pidan parar lo lee y lo analiza.
fn capture(app_id: &str, shared: &Shared) -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let format = WAVEFORMATEX {
        wFormatTag: 1, // WAVE_FORMAT_PCM
        nChannels: 2,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * 4,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    // 200 ms de búfer, en unidades de 100 ns.
    let buffer = 2_000_000;
    let client = match audio::app_root_pid(app_id).and_then(|pid| process_client(pid).ok()) {
        Some(client) => {
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    buffer,
                    0,
                    &format,
                    None,
                )?;
            }
            client
        }
        // Sin captura por app (Windows anterior, app no encontrada): el audio general.
        None => unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let client: IAudioClient = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)?
                .Activate(CLSCTX_ALL, None)?;
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                buffer,
                0,
                &format,
                None,
            )?;
            client
        },
    };
    let reader: IAudioCaptureClient = unsafe { client.GetService()? };
    unsafe { client.Start()? };
    shared.running.store(true, Ordering::SeqCst);

    let mut analyzer = Analyzer::new();
    let mut samples: Vec<f32> = Vec::with_capacity(FFT * 4);
    let mut voice = VoiceDetector::new();
    let mut stereo: Vec<[f32; 2]> = Vec::with_capacity(FFT * 4);
    while !shared.stop.load(Ordering::SeqCst) {
        std::thread::sleep(TICK);
        let mut got = false;
        let listen_voice = shared.voice.load(Ordering::Relaxed);
        unsafe {
            while reader.GetNextPacketSize().unwrap_or(0) > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                if reader
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .is_err()
                {
                    break;
                }
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                if silent || data.is_null() {
                    samples.extend(std::iter::repeat_n(0.0, frames as usize));
                    if listen_voice {
                        stereo.extend(std::iter::repeat_n([0.0; 2], frames as usize));
                    }
                } else {
                    // Estéreo de 16 bits: se promedian los dos canales.
                    let pcm = std::slice::from_raw_parts(data as *const i16, frames as usize * 2);
                    samples.extend(
                        pcm.as_chunks::<2>()
                            .0
                            .iter()
                            .map(|[l, r]| (*l as f32 + *r as f32) / 65536.0),
                    );
                    if listen_voice {
                        stereo.extend(
                            pcm.as_chunks::<2>()
                                .0
                                .iter()
                                .map(|[l, r]| [*l as f32 / 32768.0, *r as f32 / 32768.0]),
                        );
                    }
                }
                let _ = reader.ReleaseBuffer(frames);
                got = true;
            }
        }
        // Solo hacen falta las últimas `FFT` muestras.
        if samples.len() > FFT {
            samples.drain(..samples.len() - FFT);
        }
        let levels = if got && samples.len() == FFT {
            analyzer.analyze(&samples)
        } else {
            // Sin audio nuevo (silencio: el loopback general no manda nada): las barras bajan.
            analyzer.decay()
        };
        for (slot, level) in shared.levels.iter().zip(levels) {
            slot.store(level.to_bits(), Ordering::Relaxed);
        }

        if !listen_voice {
            stereo.clear();
            continue;
        }
        // Cada `HOP` muestras, un análisis de las últimas `FFT`. La muestra más nueva sonó
        // recién; las demás, tantas muestras antes.
        let now = Instant::now();
        while stereo.len() >= FFT {
            if let Some(hops_ago) = voice.step(&stereo[..FFT]) {
                let pending = stereo.len() - FFT + hops_ago * HOP + HOP / 2;
                let at = now - Duration::from_secs_f32(pending as f32 / RATE as f32);
                if let Ok(mut onsets) = shared.onsets.lock() {
                    if onsets.len() == MAX_ONSETS {
                        onsets.pop_front();
                    }
                    onsets.push_back(at);
                }
            }
            stereo.drain(..HOP);
        }
    }
    unsafe {
        let _ = client.Stop();
    }
    Ok(())
}

/// Avisa cuando Windows terminó de preparar la captura del proceso (es asíncrono).
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Activated {
    done: Mutex<Option<Sender<()>>>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(
        &self,
        _operation: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> Result<()> {
        if let Some(tx) = self.done.lock().ok().and_then(|mut d| d.take()) {
            let _ = tx.send(());
        }
        Ok(())
    }
}

impl IAgileObject_Impl for Activated_Impl {}

/// Cliente de audio que captura solo lo que suena en el proceso `pid` y sus hijos.
fn process_client(pid: u32) -> Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // Los parámetros van en un PROPVARIANT de tipo BLOB que apunta a `params`. No se suelta
    // con PropVariantClear (ManuallyDrop): la memoria es de `params`, no de COM.
    let prop = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: &params as *const _ as *mut u8,
                    },
                },
            }),
        },
    });
    let (tx, rx) = channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Activated {
        done: Mutex::new(Some(tx)),
    }
    .into();
    unsafe {
        let operation = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*prop as *const PROPVARIANT),
            &handler,
        )?;
        rx.recv_timeout(Duration::from_secs(3))
            .map_err(|_| windows::core::Error::from(E_FAIL))?;
        let mut result = HRESULT(0);
        let mut activated = None;
        operation.GetActivateResult(&mut result, &mut activated)?;
        result.ok()?;
        activated
            .ok_or_else(|| windows::core::Error::from(E_FAIL))?
            .cast()
    }
}

/// Convierte muestras en el nivel de cada barra.
struct Analyzer {
    window: Vec<f32>,
    /// Búferes de la transformada, reutilizados en cada análisis (30 por segundo).
    re: Vec<f32>,
    im: Vec<f32>,
    /// Pico reciente de cada banda (baja de a poco): las barras se adaptan al volumen.
    peaks: [f32; BARS],
    /// Pico reciente de la banda más fuerte.
    global: f32,
    levels: [f32; BARS],
}

impl Analyzer {
    fn new() -> Self {
        Self {
            // Ventana de Hann: suaviza los bordes del bloque para que las bandas no se mezclen.
            window: (0..FFT)
                .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / FFT as f32).cos())
                .collect(),
            re: vec![0.0; FFT],
            im: vec![0.0; FFT],
            peaks: [1e-3; BARS],
            global: 1e-3,
            levels: [0.0; BARS],
        }
    }

    fn analyze(&mut self, samples: &[f32]) -> [f32; BARS] {
        for ((r, s), w) in self.re.iter_mut().zip(samples).zip(&self.window) {
            *r = s * w;
        }
        self.im.fill(0.0);
        fft(&mut self.re, &mut self.im);
        let (re, im) = (&self.re, &self.im);
        let hz_per_bin = RATE as f32 / FFT as f32;
        let energies: [f32; BARS] = std::array::from_fn(|i| {
            let (lo, hi) = BANDS[i];
            let (a, b) = (
                (lo / hz_per_bin) as usize,
                ((hi / hz_per_bin) as usize).min(FFT / 2),
            );
            let b = b.max(a + 1);
            (a..b)
                .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt())
                .sum::<f32>()
                / (b - a) as f32
        });
        // Pico de la banda más fuerte: una banda muy por debajo de él (como los agudos de un
        // tono grave) se queda baja en lugar de llenarse sola por adaptarse al volumen.
        self.global = (self.global * 0.995).max(energies.iter().copied().fold(0.0, f32::max));
        for (i, &energy) in energies.iter().enumerate() {
            self.peaks[i] = (self.peaks[i] * 0.995)
                .max(energy)
                .max(self.global * 0.1)
                .max(1e-3);
            // Decibeles bajo el pico: el pico es la barra llena, `RANGE_DB` menos es vacía.
            let db = 20.0 * (energy.max(1e-9) / self.peaks[i]).log10();
            let target = if energy < 1e-4 {
                0.0
            } else {
                (1.0 + db / RANGE_DB).clamp(0.0, 1.0)
            };
            // Suben rápido y bajan más lento, como un vúmetro.
            let speed = if target > self.levels[i] { 0.6 } else { 0.2 };
            self.levels[i] += (target - self.levels[i]) * speed;
        }
        self.levels
    }

    fn decay(&mut self) -> [f32; BARS] {
        for level in &mut self.levels {
            *level *= 0.8;
        }
        self.levels
    }
}

/// Paso entre análisis de la voz: 512 muestras son 11,6 ms.
const HOP: usize = 512;
/// Banda de la voz cantada en Hz (fundamental y primeros formantes).
const VOICE_BAND: (f32, f32) = (250.0, 3000.0);
/// Análisis mínimos entre dos inicios de sílaba (10 son 116 ms: hasta ~8 sílabas por segundo).
const MIN_ONSET_GAP: u32 = 10;

/// Umbrales del detector de voz.
#[derive(Clone, Copy, Debug)]
struct Tuning {
    /// Cuántas veces el promedio reciente del flujo tiene que superar un inicio.
    ratio: f32,
    /// Flujo mínimo de un inicio, solo contra el ruido numérico: un mínimo mayor depende del
    /// volumen (con música real, 0,02 dejaba pasar una sílaba de cada ocho).
    floor: f32,
    /// Energía mínima de la voz respecto de su pico reciente para considerar que canta.
    gate: f32,
    /// Cuánto tiene que subir la energía de la voz en un inicio.
    rise: f32,
}

const TUNING: Tuning = Tuning {
    ratio: 1.8,
    floor: 0.002,
    gate: 0.12,
    rise: 1.1,
};

/// Busca cuándo empieza cada sílaba cantada.
///
/// La voz suele ir al centro de la mezcla (igual en los dos canales) y muchos instrumentos a
/// los lados: por cada frecuencia, lo que sobra del centro (M = (L + R) / 2) por encima de los
/// lados (S = (L - R) / 2) se toma como voz. Un inicio de sílaba es un salto de esa energía en
/// la banda de la voz (flujo espectral) bien por encima de su promedio reciente, y que sea un
/// máximo local. Con audio en mono no hay lados: se usa la mezcla entera de esa banda.
struct VoiceDetector {
    window: Vec<f32>,
    mid: (Vec<f32>, Vec<f32>),
    side: (Vec<f32>, Vec<f32>),
    /// Energía de la voz por frecuencia en el análisis anterior (comprimida con logaritmo).
    prev: Vec<f32>,
    /// Flujo de los dos análisis anteriores: un inicio se confirma un análisis después.
    flux: [f32; 2],
    /// Promedio reciente del flujo, para el umbral adaptable.
    average: f32,
    /// Pico reciente de la energía de la voz: muy por debajo de él se considera que no canta.
    peak: f32,
    /// Energía de la voz en los dos análisis anteriores.
    energy: [f32; 2],
    /// Análisis desde el último inicio.
    since: u32,
    tuning: Tuning,
}

impl VoiceDetector {
    fn new() -> Self {
        let hz_per_bin = RATE as f32 / FFT as f32;
        let bins = (VOICE_BAND.1 / hz_per_bin) as usize - (VOICE_BAND.0 / hz_per_bin) as usize;
        Self {
            window: (0..FFT)
                .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / FFT as f32).cos())
                .collect(),
            mid: (vec![0.0; FFT], vec![0.0; FFT]),
            side: (vec![0.0; FFT], vec![0.0; FFT]),
            prev: vec![0.0; bins],
            flux: [0.0; 2],
            average: 0.0,
            peak: 1e-3,
            energy: [0.0; 2],
            since: MIN_ONSET_GAP,
            tuning: TUNING,
        }
    }

    /// Analiza las últimas `FFT` muestras estéreo. Devuelve `Some(análisis atrás)` si ahí
    /// empezó una sílaba (se confirma con un análisis de retraso: `Some(1)`).
    fn step(&mut self, frames: &[[f32; 2]]) -> Option<usize> {
        for (k, ([l, r], w)) in frames.iter().zip(&self.window).enumerate() {
            self.mid.0[k] = (l + r) * 0.5 * w;
            self.side.0[k] = (l - r) * 0.5 * w;
        }
        self.mid.1.fill(0.0);
        self.side.1.fill(0.0);
        fft(&mut self.mid.0, &mut self.mid.1);
        fft(&mut self.side.0, &mut self.side.1);

        let hz_per_bin = RATE as f32 / FFT as f32;
        let first = (VOICE_BAND.0 / hz_per_bin) as usize;
        let (mut flux, mut energy) = (0.0f32, 0.0f32);
        for (i, prev) in self.prev.iter_mut().enumerate() {
            let k = first + i;
            let m = (self.mid.0[k].powi(2) + self.mid.1[k].powi(2)).sqrt();
            let s = (self.side.0[k].powi(2) + self.side.1[k].powi(2)).sqrt();
            let center = (m - s).max(0.0);
            energy += center;
            let c = (1.0 + center).ln();
            flux += (c - *prev).max(0.0);
            *prev = c;
        }
        let bins = self.prev.len() as f32;
        flux /= bins;
        energy /= bins;
        self.peak = (self.peak * 0.998).max(energy).max(1e-3);

        // El análisis anterior es un inicio si su flujo fue un máximo local, bastante por
        // encima del promedio, con voz audible y no muy pegado al inicio anterior.
        let [before, candidate] = self.flux;
        let Tuning {
            ratio,
            floor,
            gate,
            rise,
        } = self.tuning;
        let voiced = self.energy[1].max(energy) > self.peak * gate;
        let onset = candidate > before
            && candidate >= flux
            && candidate > self.average * ratio + floor
            && voiced
            // Al terminar una nota también cambia el espectro: solo cuenta si la voz sube.
            && self.energy[1] > self.energy[0] * rise
            && self.since >= MIN_ONSET_GAP;

        self.average += (candidate - self.average) * 0.05;
        self.flux = [candidate, flux];
        self.energy = [self.energy[1], energy];
        self.since = if onset {
            1
        } else {
            self.since.saturating_add(1)
        };
        onset.then_some(1)
    }
}

/// Transformada rápida de Fourier (radix 2, en el lugar). `re.len()` debe ser potencia de 2.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    // Reordena por inversión de bits.
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * PI / len as f32;
        let (wr, wi) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                (cr, ci) = (cr * wr - ci * wi, cr * wi + ci * wr);
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_encuentra_la_frecuencia() {
        // Un tono de 1 kHz debe dar su pico en el bin 1000 / 43,07 = 23.
        let mut re: Vec<f32> = (0..FFT)
            .map(|i| (2.0 * PI * 1000.0 * i as f32 / RATE as f32).sin())
            .collect();
        let mut im = vec![0.0; FFT];
        fft(&mut re, &mut im);
        let peak = (1..FFT / 2)
            .max_by(|&a, &b| {
                let m = |k: usize| re[k] * re[k] + im[k] * im[k];
                m(a).total_cmp(&m(b))
            })
            .unwrap();
        assert_eq!(peak, 23);
    }

    /// Escucha de verdad 1,5 s (se ignora por defecto: depende del audio del equipo). Con
    /// Spotify abierto prueba la captura por proceso; si no, la general.
    #[test]
    #[ignore]
    fn captura_real() {
        let pid = crate::audio::app_root_pid("Spotify.exe");
        println!("proceso de Spotify: {pid:?}");
        if let Some(pid) = pid {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            println!(
                "captura por proceso: {:?}",
                process_client(pid).map(|_| "ok")
            );
        }
        let mut v = Visualizer::default();
        v.set_target(Some("Spotify.exe"));
        let mut seen = None;
        for _ in 0..15 {
            std::thread::sleep(Duration::from_millis(100));
            if let Some(l) = v.levels() {
                seen = Some(l);
            }
        }
        println!("niveles: {seen:?}");
        assert!(seen.is_some(), "la captura no arrancó");
    }

    /// Escucha 10 s de Spotify buscando la voz (se ignora por defecto: depende de lo que
    /// suene). Muestra cuántos inicios de sílaba oyó y cada cuánto.
    #[test]
    #[ignore]
    fn voz_real() {
        let mut v = Visualizer::default();
        v.set_voice(true);
        v.set_target(Some("Spotify.exe"));
        std::thread::sleep(Duration::from_secs(10));
        let onsets = v.onsets().expect("la captura no arrancó");
        let gaps: Vec<u128> = onsets
            .windows(2)
            .map(|w| (w[1] - w[0]).as_millis())
            .collect();
        println!(
            "{} inicios en 10 s; separación en ms: {gaps:?}",
            onsets.len()
        );
    }

    /// Graba 12 s de Spotify en memoria (no se guardan) y prueba varios umbrales sobre el
    /// mismo audio (se ignora por defecto: depende de lo que suene).
    #[test]
    #[ignore]
    fn ajusta_umbrales_con_audio_real() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let pid = crate::audio::app_root_pid("Spotify.exe").expect("Spotify no está abierto");
        let client = process_client(pid).expect("sin captura por proceso");
        let format = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 2,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut audio: Vec<[f32; 2]> = Vec::new();
        unsafe {
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    2_000_000,
                    0,
                    &format,
                    None,
                )
                .unwrap();
            let reader: IAudioCaptureClient = client.GetService().unwrap();
            client.Start().unwrap();
            let end = Instant::now() + Duration::from_secs(12);
            while Instant::now() < end {
                std::thread::sleep(TICK);
                while reader.GetNextPacketSize().unwrap_or(0) > 0 {
                    let mut data = std::ptr::null_mut();
                    let (mut frames, mut flags) = (0u32, 0u32);
                    reader
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                        .unwrap();
                    if data.is_null() || flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                        audio.extend(std::iter::repeat_n([0.0; 2], frames as usize));
                    } else {
                        let pcm =
                            std::slice::from_raw_parts(data as *const i16, frames as usize * 2);
                        audio.extend(
                            pcm.as_chunks::<2>()
                                .0
                                .iter()
                                .map(|[l, r]| [*l as f32 / 32768.0, *r as f32 / 32768.0]),
                        );
                    }
                    let _ = reader.ReleaseBuffer(frames);
                }
            }
            let _ = client.Stop();
        }
        let secs = audio.len() as f32 / RATE as f32;
        let (mut mid, mut side) = (0.0f32, 0.0f32);
        for [l, r] in &audio {
            mid += ((l + r) * 0.5).powi(2);
            side += ((l - r) * 0.5).powi(2);
        }
        println!(
            "{secs:.1} s grabados; energía centro/lados: {:.1}",
            mid / side.max(1e-9)
        );
        for ratio in [1.2, 1.5, 1.8] {
            for floor in [0.0, 0.01, 0.02] {
                for gate in [0.05, 0.12] {
                    for rise in [1.0, 1.1] {
                        let tuning = Tuning {
                            ratio,
                            floor,
                            gate,
                            rise,
                        };
                        let mut detector = VoiceDetector::new();
                        detector.tuning = tuning;
                        let mut count = 0;
                        let mut at = 0;
                        while at + FFT <= audio.len() {
                            count += detector.step(&audio[at..at + FFT]).is_some() as usize;
                            at += HOP;
                        }
                        println!(
                            "ratio {ratio} floor {floor} gate {gate} rise {rise}: {:.1} por s",
                            count as f32 / secs
                        );
                    }
                }
            }
        }
    }

    /// El análisis corre 30 veces por segundo mientras suenan las barras: tiene que costar
    /// muy poco (con margen para equipos lentos; sin optimizar, 20 veces más).
    #[test]
    fn analizar_es_barato() {
        let slack = if cfg!(debug_assertions) { 20.0 } else { 1.0 };
        let mut analyzer = Analyzer::new();
        let noise: Vec<f32> = (0..FFT)
            .map(|i| ((i * 7919) % 200) as f32 / 400.0 - 0.25)
            .collect();
        let runs = 300;
        let start = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(analyzer.analyze(&noise));
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        println!("analisis: {ms:.4} ms");
        assert!(ms < 0.2 * slack, "analisis: {ms:.4} ms");
    }

    /// Sílabas de voz al centro: un tono con armónicos (como una vocal) que suena 180 ms de
    /// cada 300 ms. `panned` la manda solo por la izquierda; `side` agrega un instrumento
    /// constante a un lado.
    fn sung_syllables(count: usize, panned: bool, side: bool) -> Vec<[f32; 2]> {
        let period = (0.3 * RATE as f32) as usize;
        let on = (0.18 * RATE as f32) as usize;
        (0..period * count + FFT)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                // Entra en 20 ms y se apaga en 40 ms, como una sílaba cantada.
                let k = i % period;
                let envelope = if k < on {
                    (k as f32 / (0.02 * RATE as f32))
                        .min((on - k) as f32 / (0.04 * RATE as f32))
                        .min(1.0)
                } else {
                    0.0
                };
                let vowel = envelope
                    * [220.0, 440.0, 660.0, 880.0]
                        .iter()
                        .map(|f| 0.1 * (2.0 * PI * f * t).sin())
                        .sum::<f32>();
                let guitar = if side {
                    0.3 * (2.0 * PI * 330.0 * t).sin()
                } else {
                    0.0
                };
                if panned {
                    [vowel + guitar, 0.0]
                } else {
                    [vowel + guitar, vowel]
                }
            })
            .collect()
    }

    fn count_onsets(audio: &[[f32; 2]]) -> usize {
        let mut detector = VoiceDetector::new();
        let mut count = 0;
        let mut at = 0;
        while at + FFT <= audio.len() {
            count += detector.step(&audio[at..at + FFT]).is_some() as usize;
            at += HOP;
        }
        count
    }

    #[test]
    fn encuentra_las_silabas_al_centro() {
        assert_eq!(count_onsets(&sung_syllables(10, false, false)), 10);
        // Con una guitarra constante a un lado, igual.
        assert_eq!(count_onsets(&sung_syllables(10, false, true)), 10);
    }

    /// El momento que se anota (como en `capture`: el final de la ventana, menos los análisis
    /// de atraso y medio paso) cae cerca del comienzo real de cada sílaba.
    #[test]
    fn anota_el_momento_de_cada_silaba() {
        let audio = sung_syllables(10, false, true);
        let period = 0.3 * RATE as f32;
        let mut detector = VoiceDetector::new();
        let mut at = 0;
        while at + FFT <= audio.len() {
            if let Some(hops_ago) = detector.step(&audio[at..at + FFT]) {
                let sample = (at + FFT - hops_ago * HOP - HOP / 2) as f32;
                let offset = sample - (sample / period).round() * period;
                let ms = offset / RATE as f32 * 1000.0;
                assert!(ms.abs() < 60.0, "{ms:.0} ms del comienzo");
            }
            at += HOP;
        }
    }

    #[test]
    fn ignora_lo_que_suena_a_un_lado() {
        // La "voz" va solo por el canal izquierdo: es un instrumento, no la voz principal.
        assert!(count_onsets(&sung_syllables(10, true, false)) <= 1);
        // Silencio: nada.
        assert_eq!(count_onsets(&vec![[0.0; 2]; RATE as usize]), 0);
    }

    /// Con la opción activa corre ~86 veces por segundo: tiene que costar poco.
    #[test]
    fn buscar_la_voz_es_barato() {
        let slack = if cfg!(debug_assertions) { 20.0 } else { 1.0 };
        let audio = sung_syllables(2, false, true);
        let mut detector = VoiceDetector::new();
        let runs = 300;
        let start = std::time::Instant::now();
        for i in 0..runs {
            let at = (i * HOP) % (audio.len() - FFT);
            std::hint::black_box(detector.step(&audio[at..at + FFT]));
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        assert!(ms < 0.3 * slack, "voz: {ms:.4} ms");
    }

    #[test]
    fn graves_mueven_la_primera_barra() {
        let mut analyzer = Analyzer::new();
        let tone: Vec<f32> = (0..FFT)
            .map(|i| 0.5 * (2.0 * PI * 100.0 * i as f32 / RATE as f32).sin())
            .collect();
        let mut levels = [0.0; BARS];
        for _ in 0..10 {
            levels = analyzer.analyze(&tone);
        }
        assert!(levels[0] > 0.9, "graves: {levels:?}");
        assert!(levels[3] < 0.3, "agudos: {levels:?}");
    }
}
