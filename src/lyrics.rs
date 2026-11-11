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
//! voz. Si no lo traen, se estima repartiendo el tiempo del verso entre sus sílabas, con una
//! pausa breve en las comas y puntos y la última sílaba sostenida.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::update;

/// Mensaje con un `Box<Lyrics>` en `lparam` (se recupera con `take`).
pub const WM_LYRICS: u32 = WM_APP + 6;

const HOST: &str = "lrclib.net";

/// Segundos que se calculan por sílaba al estimar cuánto tarda en cantarse un verso sin
/// tiempos por palabra; solo limita los versos seguidos de una pausa larga.
const SECS_PER_SYLLABLE: f32 = 0.35;

/// Segundos antes del verso en que ya cuenta un inicio de sílaba oído: los tiempos de LRC
/// suelen ir un poco corridos respecto de la voz.
const VOICE_LEAD: f32 = 0.25;

/// Segundos sin oír la voz al empezar un verso tras los que se deja de esperarla y se usa la
/// estimación (el detector puede no oírla en una mezcla cargada).
const VOICE_WAIT: f32 = 0.8;

/// Sílabas de diferencia entre lo oído y la estimación a partir de las cuales no se confía en
/// el detector (batería o sintetizadores al centro que parecen voz).
const VOICE_DRIFT: usize = 3;

/// Sílabas de más que dura la última del verso: casi siempre se sostiene.
const FINAL_HOLD: f32 = 1.0;

/// Sílabas que dura la pausa de una coma o un punto dentro del verso.
const PUNCT_PAUSE: f32 = 0.5;

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
/// pinta sílaba a sílaba hasta poco antes del siguiente (o lo que tardaría cantarlo, si
/// después viene una pausa larga).
pub fn sung(lines: &[Verse], i: usize, t: f32) -> (f32, f32) {
    let Some(v) = lines.get(i) else {
        return (0.0, 0.0);
    };
    let len = v.text.encode_utf16().count() as f32;
    let gap = lines
        .get(i + 1)
        .map_or(f32::MAX, |n| n.start - v.start)
        .max(0.0);

    let mut marks: Vec<(f32, f32)> = Vec::with_capacity(v.words.len() + 2);
    if v.words.first().is_none_or(|&(_, at)| at > 0) {
        marks.push((v.start, 0.0));
    }
    marks.extend(v.words.iter().map(|&(t, at)| (t, at as f32)));
    let (last_t, last_at) = marks[marks.len() - 1];
    if last_at < len {
        // Lo que queda después de la última marca: lo que se tardaría en cantarlo, sin pasar
        // del verso siguiente, repartido entre sus sílabas.
        let pieces = syllables(&v.text, last_at as usize);
        let total: f32 = pieces.iter().map(|&(_, w)| w).sum();
        let end = (last_t + total * SECS_PER_SYLLABLE)
            .min(v.start + gap * 0.9)
            .max(last_t);
        let mut acc = 0.0;
        for (at, w) in pieces {
            acc += w;
            let f = if total > 0.0 { acc / total } else { 1.0 };
            marks.push((last_t + (end - last_t) * f, at as f32));
        }
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

/// Como `sung`, pero guiado por la voz: `onsets` son los segundos de la canción en que se oyó
/// empezar una sílaba cantada, en orden.
///
/// Cada inicio oído en el verso pasa a la sílaba siguiente; la sílaba en curso se pinta en lo
/// que dura una sílaba normal y espera ahí si la voz se sostiene o calla, hasta poco antes
/// del verso siguiente. Si el detector no
/// oye nada al empezar el verso, o cuenta muchas sílabas más o menos que la estimación, se
/// usa la estimación. Los versos con tiempos por palabra no lo necesitan.
pub fn sung_guided(lines: &[Verse], i: usize, t: f32, onsets: &[f32]) -> (f32, f32) {
    let estimate = sung(lines, i, t);
    let Some(v) = lines.get(i) else {
        return estimate;
    };
    let next = lines.get(i + 1).map_or(f32::MAX, |n| n.start);
    // Con tiempos por palabra no hace falta; y poco antes del verso siguiente, el verso
    // termina de pintarse aunque la voz vaya más lenta que lo oído.
    if !v.words.is_empty() || t >= v.start + (next - v.start) * 0.9 {
        return estimate;
    }
    let mut ends: Vec<u32> = syllables(&v.text, 0).iter().map(|&(at, _)| at).collect();
    ends.dedup();

    let heard: Vec<f32> = onsets
        .iter()
        .copied()
        .filter(|&o| o >= v.start - VOICE_LEAD && o <= t && o < next - VOICE_LEAD)
        .collect();
    let Some(&last) = heard.last() else {
        // Todavía no se oye la voz: el verso espera, salvo que tarde demasiado.
        return if t - v.start < VOICE_WAIT {
            (0.0, estimate.1)
        } else {
            estimate
        };
    };
    let estimated_k = ends.partition_point(|&end| (end as f32) <= estimate.0);
    if (heard.len() - 1).abs_diff(estimated_k) > VOICE_DRIFT {
        return estimate;
    }
    let k = (heard.len() - 1).min(ends.len() - 1);
    let from = if k == 0 { 0 } else { ends[k - 1] } as f32;
    let to = ends[k] as f32;
    let f = ((t - last) / SECS_PER_SYLLABLE).clamp(0.0, 1.0);
    (from + (to - from) * f, estimate.1)
}

/// Tramos en que se canta `text` desde la posición UTF-16 `from`: `(dónde termina, peso)`,
/// en orden. Cada sílaba pesa 1 (la última, más: se sostiene); un tramo que no avanza es la
/// pausa de una coma o un punto. El último termina al final del texto.
///
/// Las sílabas se cuentan por grupos de vocales ("can-ción": 2, "soy": 1), lo que sirve para
/// las lenguas con alfabeto latino o cirílico; en japonés, chino y coreano cada carácter es
/// una sílaba. Una palabra sin vocales ("2024", "pfft") cuenta como una.
fn syllables(text: &str, from: usize) -> Vec<(u32, f32)> {
    let mut chars = Vec::new();
    let mut at = 0u32;
    for c in text.chars() {
        if at as usize >= from {
            chars.push((at, c));
        }
        at += c.len_utf16() as u32;
    }
    let len = at;

    let mut pieces: Vec<(u32, f32)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !in_word(&chars, i) {
            // La pausa no avanza: el signo se pinta con la sílaba siguiente. Varios signos
            // seguidos ("...") hacen una sola.
            if let Some(&(prev, w)) = pieces.last() {
                if PAUSES.contains(&chars[i].1) && w != PUNCT_PAUSE {
                    pieces.push((prev, PUNCT_PAUSE));
                }
            }
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && in_word(&chars, i) {
            i += 1;
        }
        word_syllables(&chars[start..i], &mut pieces);
    }

    // Una pausa al final no tiene sentido: el verso ya terminó.
    while pieces.last().is_some_and(|&(_, w)| w == PUNCT_PAUSE) {
        pieces.pop();
    }
    match pieces.last_mut() {
        Some(last) => {
            last.0 = len;
            last.1 += FINAL_HOLD;
        }
        None => pieces.push((len, 0.0)),
    }
    pieces
}

/// Signos que hacen una pausa breve al cantar.
const PAUSES: [char; 10] = [',', ';', ':', '.', '!', '?', '…', '—', '–', '、'];

/// Si el carácter `i` es parte de una palabra (letras, números y el apóstrofo entre letras:
/// "don't").
fn in_word(chars: &[(u32, char)], i: usize) -> bool {
    let c = chars[i].1;
    if c.is_alphanumeric() {
        return true;
    }
    matches!(c, '\'' | '’')
        && i > 0
        && chars[i - 1].1.is_alphanumeric()
        && chars.get(i + 1).is_some_and(|&(_, n)| n.is_alphanumeric())
}

/// Agrega las sílabas de una palabra: cada una termina donde empieza la siguiente. Entre dos
/// vocales, la mitad de las consonantes va con cada una ("ho-la", "can-to").
fn word_syllables(word: &[(u32, char)], pieces: &mut Vec<(u32, f32)>) {
    // Núcleos: grupos de vocales, o un carácter silábico solo.
    let mut nuclei: Vec<(usize, usize)> = Vec::new();
    for (k, &(_, c)) in word.iter().enumerate() {
        if is_syllabic(c) {
            nuclei.push((k, k + 1));
        } else if is_vowel(word, k) {
            match nuclei.last_mut() {
                Some(n) if n.1 == k && !is_syllabic(word[n.0].1) => n.1 = k + 1,
                _ => nuclei.push((k, k + 1)),
            }
        }
    }
    for pair in nuclei.windows(2) {
        let (end, next) = (pair[0].1, pair[1].0);
        let cut = end + (next - end) / 2;
        pieces.push((word[cut].0, 1.0));
    }
    let (last_at, last) = word[word.len() - 1];
    pieces.push((last_at + last.len_utf16() as u32, 1.0));
}

/// Vocal del alfabeto latino o cirílico. La "y" es vocal salvo antes de otra vocal ("soy",
/// "my", pero "yo").
fn is_vowel(word: &[(u32, char)], k: usize) -> bool {
    let c = word[k].1.to_lowercase().next().unwrap_or(word[k].1);
    if c == 'y' {
        return !word.get(k + 1).is_some_and(|&(_, n)| {
            let n = n.to_lowercase().next().unwrap_or(n);
            n != 'y' && is_plain_vowel(n)
        });
    }
    is_plain_vowel(c)
}

fn is_plain_vowel(c: char) -> bool {
    "aeiouáéíóúàèìòùâêîôûäëïöüãõåæøœаеёиоуыэюяіїєү".contains(c)
}

/// Carácter que es una sílaba por sí solo: kana japonés, ideogramas chinos y hangul.
fn is_syllabic(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30FF}' | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}'
        | '\u{AC00}'..='\u{D7AF}')
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

        // Sin tiempos por palabra: por sílabas, y termina antes del verso siguiente.
        let plain = parse_lrc("[00:10.00] hola\n[00:20.00] Otro");
        assert_eq!(sung(&plain, 0, 10.0).0, 0.0);
        // "ho" (1 sílaba) y "la" (1 + 1 sostenida) = 3 * 0,35 s: "ho" termina a los 0,35 s.
        assert!((sung(&plain, 0, 10.35).0 - 2.0).abs() < 1e-3);
        assert!((sung(&plain, 0, 11.05).0 - 4.0).abs() < 1e-3);
        assert_eq!(sung(&plain, 0, 15.0).0, 4.0);

        // Un verso largo seguido de otro enseguida se acelera para terminar a tiempo (90 %
        // del hueco).
        let fast = parse_lrc("[00:10.00] canción de amor eterno\n[00:11.00] Otro");
        assert_eq!(sung(&fast, 0, 10.9).0, 22.0);
        assert!(sung(&fast, 0, 10.5).0 < 22.0);
    }

    #[test]
    fn la_voz_guia_el_verso() {
        // "ho-la a-mi-go": 5 sílabas.
        let lines = parse_lrc("[00:10.00] hola amigo\n[00:20.00] Otro");
        let ends = [2, 4, 6, 8, 10];
        // Antes de oír la voz, espera al comienzo.
        assert_eq!(sung_guided(&lines, 0, 10.3, &[]).0, 0.0);
        // Se oye "ho" a los 10,5 s: se pinta en 0,35 s y espera ahí.
        assert_eq!(sung_guided(&lines, 0, 10.5, &[10.5]).0, 0.0);
        assert_eq!(sung_guided(&lines, 0, 10.85, &[10.5]).0, ends[0] as f32);
        assert_eq!(sung_guided(&lines, 0, 11.4, &[10.5]).0, ends[0] as f32);
        // Cada inicio pasa a la sílaba siguiente.
        let heard = [10.5, 11.5, 11.8];
        assert_eq!(sung_guided(&lines, 0, 12.5, &heard).0, ends[2] as f32);
        // Los inicios del verso anterior no cuentan.
        assert_eq!(
            sung_guided(&lines, 0, 10.85, &[8.0, 10.5]).0,
            ends[0] as f32
        );
    }

    #[test]
    fn sin_voz_clara_vuelve_a_la_estimacion() {
        let lines = parse_lrc("[00:10.00] hola amigo\n[00:20.00] Otro");
        // No se oye nada en 0,8 s: estimación.
        assert_eq!(sung_guided(&lines, 0, 10.9, &[]), sung(&lines, 0, 10.9));
        // Muchos inicios de golpe (una batería): no se le cree.
        let drums: Vec<f32> = (0..8).map(|k| 10.0 + k as f32 * 0.05).collect();
        assert_eq!(sung_guided(&lines, 0, 10.4, &drums), sung(&lines, 0, 10.4));
        // Con tiempos por palabra no hace falta.
        let timed = parse_lrc("[00:10.00] <00:10.00> hola <00:11.00> amigo\n[00:20.00] Otro");
        assert_eq!(sung_guided(&timed, 0, 10.5, &[]), sung(&timed, 0, 10.5));
    }

    #[test]
    fn cuenta_silabas() {
        let ends =
            |text: &str| -> Vec<u32> { syllables(text, 0).iter().map(|&(at, _)| at).collect() };
        // ho-la, can-ción, soy, yo.
        assert_eq!(ends("hola"), vec![2, 4]);
        assert_eq!(ends("hola canción"), vec![2, 4, 8, 12]);
        assert_eq!(ends("Y soy yo"), vec![1, 5, 8]);
        // Palabra sin vocales y apóstrofo dentro de la palabra.
        assert_eq!(ends("2024 don't"), vec![4, 10]);
        // Cada carácter japonés es una sílaba.
        assert_eq!(ends("さくら"), vec![1, 2, 3]);
    }

    #[test]
    fn pausa_en_comas_y_sostiene_el_final() {
        let pieces = syllables("sí, no...", 0);
        // "sí", pausa (no avanza), "no" sostenida hasta el final del texto.
        assert_eq!(
            pieces,
            vec![(2, 1.0), (2, PUNCT_PAUSE), (9, 1.0 + FINAL_HOLD)]
        );
        // Sin palabras: termina enseguida.
        assert_eq!(syllables("...", 0), vec![(3, 0.0)]);
        assert_eq!(syllables("hola", 2), vec![(4, 1.0 + FINAL_HOLD)]);
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
