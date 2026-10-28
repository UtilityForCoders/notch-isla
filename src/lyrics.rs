//! Letra sincronizada de la canción, de LRCLIB (lrclib.net: base pública y gratuita, sin
//! cuenta). Solo se consulta si el usuario activa "Mostrar letra" en la bandeja: se envían
//! título, artista, álbum y duración de la canción.
//!
//! La letra llega en formato LRC, una línea por verso con el momento en que se canta
//! ("[00:12.40] texto"). Se pide una vez por canción, en otro hilo, y el resultado (con o sin
//! letra) se pasa a la ventana con `WM_LYRICS`; la isla lo guarda para no volver a pedirlo.
//!
//! Algunas letras traen además el momento de cada palabra (LRC extendido:
//! "[00:12.40] <00:12.40> Y <00:12.90> soy"): con eso el verso se pinta al ritmo exacto de la
//! voz. Si no lo traen, se estima repartiendo el tiempo del verso entre sus letras.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::update;

/// Mensaje con un `Box<Lyrics>` en `lparam` (se recupera con `take`).
pub const WM_LYRICS: u32 = WM_APP + 6;

const HOST: &str = "lrclib.net";

/// Segundos que se calculan por letra al estimar cuánto tarda en cantarse un verso sin
/// tiempos por palabra; solo limita los versos seguidos de una pausa larga.
const SECS_PER_LETTER: f32 = 0.3;

/// Letra de una canción: versos en orden. Vacía si LRCLIB no la tiene sincronizada (o no se
/// pudo consultar).
pub struct Lyrics {
    /// La canción a la que pertenece (ver `key`).
    pub key: String,
    pub lines: Vec<Verse>,
}

/// Un verso: segundo en que empieza y su texto.
#[derive(Debug, PartialEq)]
pub struct Verse {
    pub start: f32,
    pub text: String,
    /// Palabras con su propio tiempo, si la letra lo trae: (segundo, posición en `text` en
    /// unidades UTF-16, como las cuenta DirectWrite). Vacío = se estima.
    pub words: Vec<(f32, u32)>,
}

/// Identifica una canción por título y artista.
pub fn key(title: &str, artist: &str) -> String {
    format!("{title}\u{1}{artist}")
}

/// Pide la letra en otro hilo; el resultado llega con `WM_LYRICS`.
pub fn fetch(hwnd: HWND, title: String, artist: String, album: String, duration: Option<f32>) {
    let hwnd = hwnd.0 as isize;
    std::thread::spawn(move || {
        let lines = download(&title, &artist, &album, duration)
            .map(|lrc| parse_lrc(&lrc))
            .unwrap_or_default();
        let result = Box::into_raw(Box::new(Lyrics {
            key: key(&title, &artist),
            lines,
        }));
        unsafe {
            if PostMessageW(
                Some(HWND(hwnd as _)),
                WM_LYRICS,
                WPARAM(0),
                LPARAM(result as isize),
            )
            .is_err()
            {
                drop(Box::from_raw(result));
            }
        }
    });
}

/// Recupera la letra enviada con `WM_LYRICS`.
///
/// # Safety
/// `lparam` debe venir de un mensaje `WM_LYRICS` y usarse una sola vez.
pub unsafe fn take(lparam: LPARAM) -> Lyrics {
    *unsafe { Box::from_raw(lparam.0 as *mut Lyrics) }
}

/// Verso que se está cantando en el segundo `t`, o `None` antes del primero o en una pausa
/// instrumental (verso vacío).
pub fn line_at(lines: &[Verse], t: f32) -> Option<&str> {
    let text = lines.get(index_at(lines, t)?)?.text.as_str();
    (!text.is_empty()).then_some(text)
}

/// Posición del verso que se canta en el segundo `t`; `None` antes del primero.
pub fn index_at(lines: &[Verse], t: f32) -> Option<usize> {
    lines.partition_point(|v| v.start <= t).checked_sub(1)
}

/// Segundo en que empieza el verso siguiente al momento `t`, si queda alguno.
pub fn next_start(lines: &[Verse], t: f32) -> Option<f32> {
    let i = lines.partition_point(|v| v.start <= t);
    lines.get(i).map(|v| v.start)
}

/// Hasta dónde va cantado el verso `i` en el segundo `t`, en unidades UTF-16 de su texto y
/// con decimales (el pintado avanza suave): `(posición, largo total)`.
///
/// Con tiempos por palabra, cada palabra se pinta mientras dura, hasta que empieza la
/// siguiente: una nota larga se pinta despacio y el resto espera. Sin ellos, el verso se
/// pinta parejo hasta poco antes del siguiente (o lo que tardaría cantarlo, si después
/// viene una pausa larga).
pub fn sung(lines: &[Verse], i: usize, t: f32) -> (f32, f32) {
    let Some(v) = lines.get(i) else {
        return (0.0, 0.0);
    };
    let len = v.text.encode_utf16().count() as f32;
    let gap = lines.get(i + 1).map_or(f32::MAX, |n| n.start - v.start).max(0.0);

    let mut marks: Vec<(f32, f32)> = Vec::with_capacity(v.words.len() + 2);
    if v.words.first().is_none_or(|&(_, at)| at > 0) {
        marks.push((v.start, 0.0));
    }
    marks.extend(v.words.iter().map(|&(t, at)| (t, at as f32)));
    let (last_t, last_at) = marks[marks.len() - 1];
    if last_at < len {
        // Lo que queda después de la última marca: lo que se tardaría en cantarlo, sin pasar
        // del verso siguiente.
        let tail = letters(&v.text, last_at as usize) as f32;
        let end = (last_t + tail * SECS_PER_LETTER)
            .min(v.start + gap * 0.9)
            .max(last_t);
        marks.push((end, len));
    }

    let k = marks.partition_point(|&(at, _)| at <= t);
    let pos = match (k.checked_sub(1).map(|j| marks[j]), marks.get(k)) {
        (None, _) => 0.0,
        (Some((_, at)), None) => at,
        (Some((t0, p0)), Some(&(t1, p1))) => {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
            p0 + (p1 - p0) * f.clamp(0.0, 1.0)
        }
    };
    (pos.clamp(0.0, len), len)
}

/// Letras y números de `text` desde la posición UTF-16 `from` (los espacios y signos casi no
/// se demoran al cantar).
fn letters(text: &str, from: usize) -> usize {
    let mut at = 0;
    text.chars()
        .filter(|c| {
            let skip = at < from;
            at += c.len_utf16();
            !skip && c.is_alphanumeric()
        })
        .count()
}

/// Texto LRC de la canción. Primero la búsqueda exacta (título, artista, álbum y duración);
/// si no hay, la búsqueda general por título y artista.
fn download(title: &str, artist: &str, album: &str, duration: Option<f32>) -> Option<String> {
    if let Some(d) = duration {
        let path = format!(
            "/api/get?track_name={}&artist_name={}&album_name={}&duration={}",
            encode(title),
            encode(artist),
            encode(album),
            d.round() as u32
        );
        if let Some(lrc) = get_json(&path).and_then(|json| synced(&json)) {
            return Some(lrc);
        }
    }
    let path = format!(
        "/api/search?track_name={}&artist_name={}",
        encode(title),
        encode(artist)
    );
    get_json(&path).and_then(|json| synced(&json))
}

fn get_json(path: &str) -> Option<String> {
    String::from_utf8(update::get(HOST, path, false)?).ok()
}

/// El primer `"syncedLyrics"` con texto del JSON (en la búsqueda llegan varios resultados y
/// algunos no tienen letra sincronizada: vienen con `null`).
fn synced(json: &str) -> Option<String> {
    const KEY: &str = "\"syncedLyrics\"";
    let mut rest = json;
    while let Some(at) = rest.find(KEY) {
        rest = &rest[at + KEY.len()..];
        let value = rest.trim_start().strip_prefix(':')?.trim_start();
        if let Some(text) = value.strip_prefix('"') {
            let lrc = json_unescape(text)?;
            if !lrc.trim().is_empty() {
                return Some(lrc);
            }
        }
    }
    None
}

/// Lee una cadena JSON (a partir de su comilla de apertura, ya quitada) hasta la comilla de
/// cierre, resolviendo los escapes (\n, \", \uXXXX...).
fn json_unescape(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' | 'f' => {}
                'u' => {
                    let hi = hex4(&mut chars)?;
                    let code = if (0xD800..0xDC00).contains(&hi) {
                        // Par sustituto (caracteres fuera del plano básico de Unicode).
                        if chars.next()? != '\\' || chars.next()? != 'u' {
                            return None;
                        }
                        let lo = hex4(&mut chars)?;
                        0x10000 + ((hi - 0xD800) << 10) + (lo.checked_sub(0xDC00)?)
                    } else {
                        hi
                    };
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// Los 4 dígitos hexadecimales de un escape \uXXXX.
fn hex4(chars: &mut std::str::Chars) -> Option<u32> {
    let hex: String = chars.take(4).collect();
    u32::from_str_radix(&hex, 16).ok()
}

/// Versos de un texto LRC, en orden. Una línea puede llevar varias marcas de tiempo
/// ("[00:12.40][01:30.10] estribillo"); las de datos ("[ar:Artista]") se ignoran.
fn parse_lrc(lrc: &str) -> Vec<Verse> {
    let mut lines = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(tag) = rest.strip_prefix('[') {
            let Some(end) = tag.find(']') else { break };
            match parse_time(&tag[..end]) {
                Some(t) => times.push(t),
                None => break,
            }
            rest = tag[end + 1..].trim_start();
        }
        let (text, words) = parse_words(rest);
        let first = times.first().copied().unwrap_or(0.0);
        for &t in &times {
            // Un estribillo repetido trae los tiempos de palabra de su primera vez: se corren
            // al inicio de cada repetición.
            let words = words.iter().map(|&(w, at)| (w + (t - first), at)).collect();
            lines.push(Verse {
                start: t,
                text: text.clone(),
                words,
            });
        }
    }
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    lines
}

/// Quita las marcas de palabra ("<00:12.90>") del texto de un verso y devuelve el texto limpio
/// con el tiempo de cada marca y dónde cae en él (unidades UTF-16).
fn parse_words(raw: &str) -> (String, Vec<(f32, u32)>) {
    let mut text = String::new();
    let mut words = Vec::new();
    let mut rest = raw;
    while let Some(open) = rest.find('<') {
        let tag = &rest[open + 1..];
        let Some(close) = tag.find('>') else { break };
        let Some(t) = parse_time(&tag[..close]) else {
            // No era una marca: el "<" es parte del texto.
            text.push_str(&rest[..=open]);
            rest = tag;
            continue;
        };
        push_piece(&mut text, &rest[..open]);
        words.push((t, text.encode_utf16().count() as u32));
        rest = &tag[close + 1..];
    }
    push_piece(&mut text, rest);

    // Sin los espacios de los bordes, las posiciones se corren lo mismo que el texto.
    let lead = text.len() - text.trim_start().len();
    let lead16 = text[..lead].encode_utf16().count() as u32;
    let clean = text.trim().to_string();
    let len16 = clean.encode_utf16().count() as u32;
    for (_, at) in &mut words {
        *at = at.saturating_sub(lead16).min(len16);
    }
    words.dedup_by(|b, a| b.1 == a.1);
    (clean, words)
}

/// Agrega un trozo de texto entre marcas: las marcas suelen ir rodeadas de espacios
/// ("Y <00:49.00> soy"), y al quitarlas no deben quedar dos seguidos.
fn push_piece(text: &mut String, piece: &str) {
    if text.ends_with(char::is_whitespace) {
        text.push_str(piece.trim_start());
    } else {
        text.push_str(piece);
    }
}

/// "mm:ss.xx" -> segundos.
fn parse_time(tag: &str) -> Option<f32> {
    let (min, sec) = tag.split_once(':')?;
    let min: f32 = min.trim().parse().ok()?;
    let sec: f32 = sec.trim().parse().ok()?;
    Some(min * 60.0 + sec)
}

/// Codifica un texto para la dirección (UTF-8 con %XX, salvo letras, números y -_.~).
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_lrc() {
        let lines =
            parse_lrc("[ar:Alguien]\n[00:12.40] Uno\n[00:05.00][01:00.50]Coro\n[00:20.00]\n");
        let starts: Vec<(f32, &str)> = lines.iter().map(|v| (v.start, v.text.as_str())).collect();
        assert_eq!(
            starts,
            vec![(5.0, "Coro"), (12.4, "Uno"), (20.0, ""), (60.5, "Coro")]
        );
        assert!(lines.iter().all(|v| v.words.is_empty()));
        assert_eq!(line_at(&lines, 1.0), None);
        assert_eq!(line_at(&lines, 5.0), Some("Coro"));
        assert_eq!(line_at(&lines, 15.0), Some("Uno"));
        // Pausa instrumental.
        assert_eq!(line_at(&lines, 30.0), None);
        assert_eq!(line_at(&lines, 99.0), Some("Coro"));
    }

    #[test]
    fn lee_tiempos_por_palabra() {
        let lines = parse_lrc("[00:48.54] <00:48.54> Y <00:49.00> soy <00:52.00> solamente <00:53.00>\n[00:55.00] Otro");
        let v = &lines[0];
        assert_eq!(v.text, "Y soy solamente");
        // "Y" en 0, "soy" en 2, "solamente" en 6 y el final (marca de cierre) en 15.
        assert_eq!(v.words, vec![(48.54, 0), (49.0, 2), (52.0, 6), (53.0, 15)]);
        // "<" que no es marca de tiempo se queda en el texto.
        assert_eq!(parse_words("a <3 b").0, "a <3 b");
    }

    #[test]
    fn pinta_al_ritmo_de_la_voz() {
        let lines = parse_lrc("[00:48.54] <00:48.54> Y <00:49.00> soy <00:52.00> solamente <00:53.00>\n[00:55.00] Otro");
        // Durante la nota larga ("soy", de 49 a 52 s) el pintado no pasa de esa palabra.
        let (pos, len) = sung(&lines, 0, 50.5);
        assert_eq!(len, 15.0);
        assert!((2.0..6.0).contains(&pos), "{pos}");
        assert_eq!(sung(&lines, 0, 53.5).0, 15.0);

        // Sin tiempos por palabra: parejo, y termina antes del verso siguiente.
        let plain = parse_lrc("[00:10.00] abcd\n[00:20.00] Otro");
        assert_eq!(sung(&plain, 0, 10.0).0, 0.0);
        // 4 letras * 0,3 s = 1,2 s: a la mitad, la mitad del verso.
        assert!((sung(&plain, 0, 10.6).0 - 2.0).abs() < 1e-3);
        assert_eq!(sung(&plain, 0, 15.0).0, 4.0);
    }

    #[test]
    fn lee_json_de_lrclib() {
        let json = r#"[{"id":1,"syncedLyrics":null},{"id":2,"syncedLyrics":"[00:01.00] Corazón \"bello\"\n[00:02.00] 𝄞"}]"#;
        assert_eq!(
            synced(json).as_deref(),
            Some("[00:01.00] Corazón \"bello\"\n[00:02.00] \u{1D11E}")
        );
        assert_eq!(synced(r#"{"syncedLyrics":null}"#), None);
    }

    #[test]
    fn codifica_la_direccion() {
        assert_eq!(encode("Canción & Co"), "Canci%C3%B3n%20%26%20Co");
    }

    /// Consulta real a lrclib.net (se ignora por defecto: necesita internet).
    #[test]
    #[ignore]
    fn consulta_lrclib() {
        let lrc = download(
            "Bohemian Rhapsody",
            "Queen",
            "A Night at the Opera",
            Some(355.0),
        )
        .expect("sin letra");
        let lines = parse_lrc(&lrc);
        println!("{} versos, primero: {:?}", lines.len(), lines.first());
        assert!(lines.len() > 10);
    }
}
