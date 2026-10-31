//! Barras de audio reales: siguen el sonido de la app que suena en lugar de una animación.
//!
//! Un hilo "escucha" la salida de audio con WASAPI en modo loopback (lo que suena, no el
//! micrófono): en Windows 10 2004+ y 11, solo el audio del proceso de la app (y sus procesos
//! hijos, como el de audio de Chrome); si eso no se puede, el audio general. Unas 30 veces por
//! segundo separa graves, medios y agudos con una transformada de Fourier y deja los 4 niveles
//! listos para la isla. El audio se procesa en memoria y se descarta: no se guarda ni se envía.
//!
//! Solo corre mientras se ven las barras (música sonando, isla contraída y a la vista); en
//! cualquier otro caso el hilo termina y la CPU vuelve a 0 %.

use std::f32::consts::PI;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
}

/// Controla la captura: se le dice qué app escuchar (o ninguna) en cada cuadro.
#[derive(Default)]
pub struct Visualizer {
    /// App que se está escuchando y sus niveles.
    current: Option<(String, Arc<Shared>)>,
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
        });
        let thread_shared = shared.clone();
        let id = app_id.to_string();
        std::thread::spawn(move || {
            let _ = capture(&id, &thread_shared);
            thread_shared.running.store(false, Ordering::SeqCst);
        });
        self.current = Some((app_id.to_string(), shared));
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
    while !shared.stop.load(Ordering::SeqCst) {
        std::thread::sleep(TICK);
        let mut got = false;
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
                } else {
                    // Estéreo de 16 bits: se promedian los dos canales.
                    let pcm = std::slice::from_raw_parts(data as *const i16, frames as usize * 2);
                    samples.extend(
                        pcm.as_chunks::<2>()
                            .0
                            .iter()
                            .map(|[l, r]| (*l as f32 + *r as f32) / 65536.0),
                    );
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
