//! Acciones de compartir en la isla y diálogos de confirmación de Windows.

use crate::share_identity::Store;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, Result, PWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::Shell::{
    DragFinish, DragQueryFileW, FOLDERID_Downloads, FileOpenDialog, IFileOpenDialog,
    SHGetKnownFolderPath, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_PATHMUSTEXIST, HDROP, KF_FLAG_DEFAULT, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::island::{Notice, NoticeIcon};
use crate::menu::{self, Item};
use crate::share::{self, Payload, Peer, ReceiveEvent};

pub const WM_SHARE_EVENT: u32 = WM_APP + 12;
pub const WM_SHARE_MENU: u32 = WM_APP + 13;
const TITLE: &str = "Compartir cerca";
// mDNS usa UDP y un sondeo aislado puede no recibir respuesta. No conviertas una sola
// pérdida de paquetes en "Sin conexión" ni impidas un envío que todavía puede funcionar.
const PEER_GRACE: Duration = Duration::from_secs(30);

enum Search {
    Pair,
    Manage,
}

enum Event {
    Devices(Search, std::result::Result<Vec<Peer>, String>),
    Discovery(std::result::Result<Vec<Peer>, String>),
    Sent(std::result::Result<(), String>),
    Paired(std::result::Result<(), String>),
    ConfirmPair {
        name: String,
        code: String,
        decision: Sender<bool>,
    },
    Receive(u64, ReceiveEvent),
    Stopped(u64, std::result::Result<(), String>),
}

struct Ui {
    tx: Sender<Event>,
    rx: mpsc::Receiver<Event>,
    store: Option<Arc<Store>>,
    pairing: Arc<share::Pairing>,
    cancel: Option<Sender<()>>,
    service_id: u64,
    receive_code: Option<String>,
    peers: Vec<Peer>,
    peer_seen: BTreeMap<Vec<u8>, Instant>,
    selected: Option<Vec<u8>>,
    sending: bool,
    accepting: bool,
    status: String,
    modal: bool,
    inbox: VecDeque<PendingMessage>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PendingMessage {
    sender: String,
    content: PendingContent,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PendingContent {
    Text { text: String },
    Image { path: PathBuf },
    Files { path: PathBuf },
}

thread_local! {
    static UI: RefCell<Ui> = {
        let (tx, rx) = mpsc::channel();
        RefCell::new(Ui { tx, rx, store: None, pairing: Arc::new(share::Pairing::default()), cancel: None, service_id: 0, receive_code: None, peers: Vec::new(), peer_seen: BTreeMap::new(), selected: None, sending: false, accepting: false, status: String::new(), modal: false, inbox: load_inbox() })
    };
}

fn store() -> Option<Arc<Store>> {
    if let Some(store) = UI.with_borrow(|ui| ui.store.clone()) {
        return Some(store);
    }
    match Store::load() {
        Ok(store) => {
            let store = Arc::new(store);
            UI.with_borrow_mut(|ui| ui.store = Some(store.clone()));
            Some(store)
        }
        Err(error) => {
            notify(&format!(
                "No se pudieron abrir los equipos vinculados. {error}"
            ));
            None
        }
    }
}

pub fn init(hwnd: HWND) {
    start_discovery(hwnd);
    if store().is_some_and(|store| store.enabled()) {
        start_receive(hwnd);
    }
    if let Some(message) = UI.with_borrow(|ui| ui.inbox.front().cloned()) {
        notify_received(&message, UI.with_borrow(|ui| ui.inbox.len()));
    }
}

fn reporter(hwnd: HWND) -> impl Fn(Event) + Send + 'static {
    let hwnd = hwnd.0 as isize;
    let tx = UI.with_borrow(|ui| ui.tx.clone());
    move |event| {
        if tx.send(event).is_ok() {
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as _)), WM_SHARE_EVENT, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn notify(detail: &str) {
    UI.with_borrow_mut(|ui| ui.status = detail.to_string());
    crate::with_app(|app| {
        if app.page == crate::workspace::Page::Share {
            app.scroll_start = std::time::Instant::now();
            app.redraw();
            return;
        }
        app.show_notice_card(
            Notice {
                icon: NoticeIcon::Wireless,
                color: [83, 166, 255],
            },
            TITLE,
            detail,
            4500,
        );
    });
}

fn action(id: usize, label: impl Into<String>) -> Item {
    Item::Action {
        id,
        label: label.into(),
        checked: false,
    }
}

struct PanelModal(bool);

fn panel_modal() -> PanelModal {
    PanelModal(UI.with_borrow_mut(|ui| {
        let previous = ui.modal;
        ui.modal = true;
        previous
    }))
}

impl Drop for PanelModal {
    fn drop(&mut self) {
        UI.with_borrow_mut(|ui| ui.modal = self.0);
        crate::with_app(|app| app.redraw());
    }
}

pub fn panel_state() -> crate::workspace::ShareState {
    let devices = store().map_or_else(Vec::new, |store| store.devices());
    UI.with_borrow_mut(|ui| {
        let selected_exists = ui
            .selected
            .as_ref()
            .is_some_and(|selected| devices.iter().any(|device| device.certificate == *selected));
        if !selected_exists {
            ui.selected = devices.first().map(|device| device.certificate.clone());
        }
        let selected = ui.selected.as_ref().and_then(|certificate| {
            devices
                .iter()
                .find(|device| device.certificate == *certificate)
        });
        crate::workspace::ShareState {
            receiving: ui.cancel.is_some(),
            device_name: selected
                .map(|device| device.display_name().to_owned())
                .unwrap_or_default(),
            busy: ui.sending || ui.accepting || ui.modal,
            status: ui.status.clone(),
            unread: ui.inbox.len(),
        }
    })
}

fn select_device(hwnd: HWND) {
    let Some(store) = store() else {
        return;
    };
    let devices = store.devices();
    if devices.is_empty() {
        start_pairing(hwnd);
        return;
    }
    let _modal = panel_modal();
    let selected = UI.with_borrow(|ui| ui.selected.clone());
    let mut items = vec![Item::Label("Enviar a".into())];
    for (index, device) in devices.iter().enumerate() {
        items.push(Item::Action {
            id: index + 1,
            label: device.display_name().to_owned(),
            checked: selected.as_ref() == Some(&device.certificate),
        });
    }
    items.push(Item::Separator);
    items.push(action(1000, "Vincular otro equipo…"));
    match menu::show(items) {
        Some(1000) => start_pairing(hwnd),
        Some(id) if (1..=devices.len()).contains(&id) => {
            UI.with_borrow_mut(|ui| {
                ui.selected = Some(devices[id - 1].certificate.clone());
                ui.status.clear();
            });
        }
        _ => {}
    }
}

/// Las acciones del panel se ejecutan fuera del préstamo de App: los diálogos procesan mensajes.
pub fn perform(hwnd: HWND, action: crate::workspace::ShareAction) {
    use crate::workspace::ShareAction;
    if UI.with_borrow(|ui| ui.sending || ui.accepting || ui.modal) {
        notify("Termina primero la solicitud actual.");
        return;
    }
    match action {
        ShareAction::Files => {
            let _modal = panel_modal();
            notify("Elige archivos para compartir");
            let selected = choose_files(hwnd);
            match selected {
                Ok(Some(files)) => start_send(hwnd, Payload::Files(files)),
                Ok(None) => {}
                Err(_) => notify("No se pudo abrir el selector de archivos."),
            }
        }
        ShareAction::Clipboard => match crate::share_preview::clipboard_image(hwnd)
            .map(Payload::Image)
            .or_else(|| clipboard_text(hwnd).map(Payload::Text))
        {
            Some(payload) => start_send(hwnd, payload),
            None => notify("Copia texto o una imagen primero."),
        },
        ShareAction::Device => select_device(hwnd),
        ShareAction::Inbox => open_received(hwnd),
        ShareAction::Receiving => set_receiving(hwnd, !receiving_enabled()),
    }
    crate::with_app(|app| app.redraw());
}

pub fn open(hwnd: HWND) {
    let Some(store) = store() else {
        return;
    };
    let (receiving, accepting, pairing) =
        UI.with_borrow(|ui| (ui.cancel.is_some(), ui.accepting, ui.pairing.is_open()));
    let unread = UI.with_borrow(|ui| ui.inbox.len());
    let mut items = vec![Item::Label(TITLE.into())];
    if unread > 0 {
        items.push(action(
            7,
            if unread == 1 {
                "1 mensaje sin leer".to_string()
            } else {
                format!("{unread} mensajes sin leer")
            },
        ));
        items.push(Item::Separator);
    }
    items.extend([
        action(1, "Elegir fotos o archivos…"),
        action(2, "Enviar texto o imagen copiada"),
        Item::Separator,
        action(4, "Vincular equipo…"),
        action(5, format!("Tus dispositivos · {}", store.devices().len())),
        Item::Separator,
    ]);
    if accepting {
        items.push(Item::Label("Solicitud entrante en curso".into()));
    }
    if pairing {
        items.push(action(6, "Cerrar vinculación para equipos nuevos"));
    }
    items.push(Item::Action {
        id: 3,
        label: "Recibir de equipos vinculados".into(),
        checked: receiving,
    });
    match menu::show(items) {
        Some(1) => match choose_files(hwnd) {
            Ok(Some(files)) => start_send(hwnd, Payload::Files(files)),
            Ok(None) => {}
            Err(_) => notify("No se pudo abrir el selector de archivos."),
        },
        Some(2) => match crate::share_preview::clipboard_image(hwnd)
            .map(Payload::Image)
            .or_else(|| clipboard_text(hwnd).map(Payload::Text))
        {
            Some(payload) => start_send(hwnd, payload),
            None => notify("Copia texto (hasta 256 KB) o una imagen (hasta 32 MB)."),
        },
        Some(3) => set_receiving(hwnd, !receiving),
        Some(4) => start_pairing(hwnd),
        Some(5) => start_search(hwnd, Search::Manage),
        Some(6) => {
            UI.with_borrow(|ui| ui.pairing.close());
            notify("Vinculación cerrada. Solo pueden enviar tus equipos guardados.");
        }
        Some(7) => open_received(hwnd),
        _ => {}
    }
}

pub fn drop_files(hwnd: HWND, drop: HDROP) {
    let files = unsafe {
        let count = DragQueryFileW(drop, u32::MAX, None);
        let files = (1..=share::MAX_ITEMS as u32).contains(&count).then(|| {
            (0..count)
                .filter_map(|index| {
                    let len = DragQueryFileW(drop, index, None);
                    let mut path = vec![0; len as usize + 1];
                    let written = DragQueryFileW(drop, index, Some(&mut path));
                    (written != 0)
                        .then(|| PathBuf::from(OsString::from_wide(&path[..written as usize])))
                })
                .collect::<Vec<_>>()
        });
        DragFinish(drop);
        files.filter(|files| files.len() == count as usize)
    };
    if let Some(files) = files {
        start_send(hwnd, Payload::Files(files));
    } else {
        notify("Arrastra entre 1 y 20 archivos.");
    }
}

fn choose_files(hwnd: HWND) -> Result<Option<Vec<PathBuf>>> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetTitle(w!("Elige fotos o archivos para compartir"))?;
        dialog.SetOptions(
            dialog.GetOptions()?
                | FOS_ALLOWMULTISELECT
                | FOS_FORCEFILESYSTEM
                | FOS_FILEMUSTEXIST
                | FOS_PATHMUSTEXIST,
        )?;
        if let Err(error) = dialog.Show(Some(hwnd)) {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(None);
            }
            return Err(error);
        }
        let items = dialog.GetResults()?;
        let count = items.GetCount()?;
        if count as usize > share::MAX_ITEMS {
            notify("Selecciona un máximo de 20 archivos.");
            return Ok(None);
        }
        let paths = (0..count)
            .map(|index| {
                let path = items.GetItemAt(index)?.GetDisplayName(SIGDN_FILESYSPATH)?;
                Ok(owned_path(path))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(paths))
    }
}

unsafe fn owned_path(path: PWSTR) -> PathBuf {
    let value = PathBuf::from(OsString::from_wide(path.as_wide()));
    CoTaskMemFree(Some(path.0.cast()));
    value
}

pub(crate) fn clipboard_text(hwnd: HWND) -> Option<String> {
    // CF_UNICODETEXT, leído únicamente al elegir la acción.
    const UNICODE_TEXT: u32 = 13;
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let result = (|| {
            let memory = HGLOBAL(GetClipboardData(UNICODE_TEXT).ok()?.0);
            let size = GlobalSize(memory) / 2;
            let ptr = GlobalLock(memory).cast::<u16>();
            if ptr.is_null() {
                return None;
            }
            let text = std::slice::from_raw_parts(ptr, size.min(share::MAX_TEXT + 1));
            let result = text
                .iter()
                .position(|&c| c == 0)
                .map(|end| String::from_utf16_lossy(&text[..end]));
            let _ = GlobalUnlock(memory);
            result.filter(|text| !text.is_empty() && text.len() <= share::MAX_TEXT)
        })();
        let _ = CloseClipboard();
        result
    }
}

fn start_send(hwnd: HWND, payload: Payload) {
    let Some(store) = store() else {
        return;
    };
    let Some((selected, peer)) = UI.with_borrow(|ui| {
        let selected = ui.selected.clone()?;
        let peer = ui
            .peers
            .iter()
            .find(|peer| peer.certificate.as_ref() == selected.as_slice())
            .cloned();
        Some((selected, peer))
    }) else {
        notify("Elige primero un dispositivo vinculado.");
        return;
    };
    let started = UI.with_borrow_mut(|ui| {
        if ui.sending || ui.accepting {
            return false;
        }
        ui.sending = true;
        true
    });
    if !started {
        notify("Termina primero la solicitud actual.");
        return;
    }
    notify(if peer.is_some() {
        "Enviando al equipo vinculado…"
    } else {
        "Buscando el equipo vinculado…"
    });
    let report = reporter(hwnd);
    thread::spawn(move || {
        let peer = match peer {
            Some(peer) => Ok(peer),
            None => share::discover(&store)
                .map_err(|error| error.to_string())
                .and_then(|peers| {
                    let peer = peers
                        .iter()
                        .find(|peer| peer.certificate.as_ref() == selected.as_slice())
                        .cloned();
                    // Conserva también el resultado del sondeo solicitado por el usuario.
                    report(Event::Discovery(Ok(peers)));
                    peer.ok_or_else(|| {
                        "El equipo no respondió a la búsqueda. Comprueba que tenga activada la recepción y que ambos estén en la misma red.".to_string()
                    })
                }),
        };
        report(Event::Sent(peer.and_then(|peer| {
            share::send(&store, peer, payload).map_err(|error| error.to_string())
        })))
    });
}

fn update_discovered_peers(ui: &mut Ui, discovered: Vec<Peer>) {
    let now = Instant::now();
    for peer in discovered {
        let certificate = peer.certificate.to_vec();
        ui.peer_seen.insert(certificate.clone(), now);
        if let Some(current) = ui
            .peers
            .iter_mut()
            .find(|current| current.certificate.as_ref() == certificate.as_slice())
        {
            *current = peer;
        } else {
            ui.peers.push(peer);
        }
    }
    ui.peer_seen
        .retain(|_, seen| now.saturating_duration_since(*seen) <= PEER_GRACE);
    ui.peers
        .retain(|peer| ui.peer_seen.contains_key(peer.certificate.as_ref()));
    ui.peers
        .sort_by(|left, right| left.name.cmp(&right.name).then(left.code.cmp(&right.code)));
}

fn start_search(hwnd: HWND, search: Search) {
    let Some(store) = store() else {
        return;
    };
    let started = UI.with_borrow_mut(|ui| {
        if ui.sending || ui.accepting {
            return false;
        }
        ui.sending = true;
        true
    });
    if !started {
        notify("Termina primero la solicitud actual.");
        return;
    }
    notify("Buscando equipos disponibles…");
    let report = reporter(hwnd);
    thread::spawn(move || {
        report(Event::Devices(
            search,
            share::discover(&store).map_err(|error| error.to_string()),
        ))
    });
}

fn start_discovery(hwnd: HWND) {
    let Some(store) = store() else {
        return;
    };
    let report = reporter(hwnd);
    thread::spawn(move || loop {
        report(Event::Discovery(
            share::discover(&store).map_err(|error| error.to_string()),
        ));
        thread::sleep(std::time::Duration::from_secs(5));
    });
}

pub fn receiving_enabled() -> bool {
    store().is_some_and(|store| store.enabled())
}

/// Activa o pausa el listener sin borrar identidades ni equipos vinculados.
pub fn set_receiving(hwnd: HWND, enabled: bool) {
    let Some(store) = store() else {
        return;
    };
    if let Err(error) = store.set_enabled(enabled) {
        notify(&format!(
            "No se pudo {} la recepción. {error}",
            if enabled { "activar" } else { "pausar" }
        ));
        return;
    }
    if enabled {
        start_receive(hwnd);
        notify("Recepción activada para tus equipos vinculados.");
    } else {
        stop();
        notify("No molestar activado. Tus equipos siguen vinculados.");
    }
}

fn start_receive(hwnd: HWND) {
    if UI.with_borrow(|ui| ui.cancel.is_some()) {
        return;
    }
    let Some(store) = store() else {
        return;
    };
    let downloads = unsafe {
        SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None)
            .map(|path| owned_path(path))
    };
    let Ok(downloads) = downloads else {
        notify("No se encontró la carpeta Descargas.");
        return;
    };
    let (stop, cancel) = mpsc::channel();
    let (id, pairing) = UI.with_borrow_mut(|ui| {
        ui.service_id += 1;
        ui.cancel = Some(stop);
        (ui.service_id, ui.pairing.clone())
    });
    let report = reporter(hwnd);
    thread::spawn(move || {
        let result = share::serve(downloads, store, pairing, cancel, |event| {
            report(Event::Receive(id, event))
        })
        .map_err(|error| error.to_string());
        report(Event::Stopped(id, result));
    });
}

fn start_pairing(hwnd: HWND) {
    if UI.with_borrow(|ui| ui.sending || ui.accepting) {
        notify("Termina primero la solicitud actual.");
        return;
    }
    let Some(store) = store() else {
        return;
    };
    if let Err(error) = store.set_enabled(true) {
        notify(&format!("No se pudo activar la vinculación. {error}"));
        return;
    }
    UI.with_borrow(|ui| ui.pairing.open());
    start_receive(hwnd);
    start_search(hwnd, Search::Pair);
}

/// Cierra el listener sin cambiar la preferencia guardada (también se usa al salir).
pub fn stop() {
    UI.with_borrow_mut(|ui| {
        ui.service_id += 1;
        ui.pairing.close();
        ui.receive_code = None;
        if let Some(cancel) = ui.cancel.take() {
            let _ = cancel.send(());
        }
    });
}

fn choose_pair(hwnd: HWND, peers: Vec<Peer>) {
    let Some(store) = store() else {
        return;
    };
    let _modal = panel_modal();
    let peers: Vec<_> = peers.into_iter().filter(|peer| peer.pairable).collect();
    let mut items = vec![
        Item::Label("Vincular equipo".into()),
        Item::Label("Abre Vincular equipo en el otro PC".into()),
        Item::Label("Después, compara el código en ambos".into()),
        Item::Separator,
    ];
    for (index, peer) in peers.iter().enumerate() {
        items.push(action(index + 1, peer.name.clone()));
    }
    if peers.is_empty() {
        items.push(Item::Label("No hay equipos listos para vincular".into()));
    }
    items.push(action(1000, "Buscar de nuevo"));
    let selected = menu::show(items);
    if selected == Some(1000) {
        start_pairing(hwnd);
        return;
    }
    let Some(peer) = selected
        .and_then(|id| id.checked_sub(1))
        .and_then(|index| peers.into_iter().nth(index))
    else {
        return;
    };
    UI.with_borrow_mut(|ui| ui.sending = true);
    let report = reporter(hwnd);
    thread::spawn(move || {
        let result = share::pair(&store, peer, |name, code| {
            let (decision, answer) = mpsc::channel();
            report(Event::ConfirmPair {
                name,
                code,
                decision,
            });
            answer.recv_timeout(std::time::Duration::from_secs(60)) == Ok(true)
        })
        .map_err(|error| error.to_string());
        report(Event::Paired(result));
    });
}

pub fn manage_linked(hwnd: HWND) {
    manage_devices(hwnd, None);
}

fn manage_devices(hwnd: HWND, peers: Option<&[Peer]>) {
    let Some(store) = store() else {
        return;
    };
    let _modal = panel_modal();
    let devices = store.devices();
    let mut items = vec![Item::Label("Tus dispositivos".into())];
    for (index, device) in devices.iter().enumerate() {
        let name = device.display_name();
        let label = match peers {
            Some(peers)
                if peers
                    .iter()
                    .any(|peer| peer.certificate.as_ref() == device.certificate) =>
            {
                format!("{name} · Disponible")
            }
            Some(_) => format!("{name} · Sin conexión"),
            None => name.to_owned(),
        };
        items.push(Item::Group {
            label,
            items: vec![
                action(index * 2 + 1, "Cambiar nombre…"),
                action(index * 2 + 2, "Olvidar equipo"),
            ],
        });
    }
    if devices.is_empty() {
        items.push(Item::Label("Todavía no hay equipos vinculados".into()));
    }
    let Some(index) = menu::show(items).and_then(|id| id.checked_sub(1)) else {
        return;
    };
    let Some(device) = devices.get(index / 2) else {
        return;
    };
    if index % 2 == 0 {
        match crate::share_preview::edit_device_name(hwnd, device.display_name()) {
            Ok(Some(name)) => match store.rename(&device.certificate, &name) {
                Ok(()) => notify("Nombre del equipo guardado"),
                Err(error) => notify(&format!("No se pudo guardar el nombre. {error}")),
            },
            Ok(None) => {}
            Err(error) => notify(&format!("No se pudo abrir el editor de nombre. {error}")),
        }
    } else {
        match store.forget(&device.certificate) {
            Ok(()) => notify(&format!(
                "{} ya no es un equipo de confianza.",
                device.display_name()
            )),
            Err(error) => notify(&format!("No se pudo olvidar el equipo. {error}")),
        }
    }
}

fn confirm_pair(hwnd: HWND, name: String, code: String, decision: Sender<bool>) {
    let busy = UI.with_borrow_mut(|ui| {
        let busy = ui.accepting;
        if !busy {
            ui.accepting = true;
        }
        busy
    });
    if busy {
        let _ = decision.send(false);
        return;
    }
    let accepted = crate::share_preview::pair(hwnd, &name, &code);
    let _ = decision.send(accepted);
    UI.with_borrow_mut(|ui| ui.accepting = false);
}

fn inbox_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("NotchIsla").join("share-inbox.json"))
}

fn load_inbox() -> VecDeque<PendingMessage> {
    let Some(path) = inbox_path() else {
        return VecDeque::new();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return VecDeque::new();
    };
    let Ok(mut messages) = serde_json::from_slice::<VecDeque<PendingMessage>>(&bytes) else {
        return VecDeque::new();
    };
    messages.truncate(100);
    messages.retain(|message| match &message.content {
        PendingContent::Text { .. } => true,
        PendingContent::Image { path } | PendingContent::Files { path } => path.exists(),
    });
    messages
}

fn save_inbox(messages: &VecDeque<PendingMessage>) -> std::io::Result<()> {
    let path = inbox_path().ok_or_else(|| std::io::Error::other("LOCALAPPDATA no disponible"))?;
    let directory = path.parent().expect("share-inbox tiene carpeta");
    std::fs::create_dir_all(directory)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec(messages)?)?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    std::fs::rename(temporary, path)
}

fn stable_image(path: &std::path::Path, name: &str) -> std::io::Result<PathBuf> {
    let directory = inbox_path()
        .and_then(|path| path.parent().map(|parent| parent.join("share-inbox")))
        .ok_or_else(|| std::io::Error::other("LOCALAPPDATA no disponible"))?;
    std::fs::create_dir_all(&directory)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 10
                && extension
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
        .unwrap_or("img");
    let destination = directory.join(format!("imagen-{stamp}.{extension}"));
    if std::fs::rename(path, &destination).is_err() {
        std::fs::copy(path, &destination)?;
    }
    Ok(destination)
}

fn message_summary(message: &PendingMessage) -> &'static str {
    match message.content {
        PendingContent::Text { .. } => "Te envió un texto",
        PendingContent::Image { .. } => "Te envió una imagen",
        PendingContent::Files { .. } => "Sus archivos ya están en Descargas",
    }
}

fn notify_received(message: &PendingMessage, unread: usize) {
    let suffix = if unread > 1 {
        format!(" · {unread} sin leer")
    } else {
        String::new()
    };
    crate::with_app(|app| {
        app.queue_share_notice(
            format!("Mensaje de {}", message.sender),
            format!(
                "{}{} · Haz clic para abrir",
                message_summary(message),
                suffix
            ),
        );
        app.redraw();
    });
}

fn queue_received(result: std::result::Result<Option<share::Received>, String>) {
    let message = match result {
        Ok(Some(share::Received::Text { sender, text })) => Some(PendingMessage {
            sender,
            content: PendingContent::Text { text },
        }),
        Ok(Some(share::Received::Image {
            sender,
            name,
            directory,
            path,
        })) => match stable_image(&path, &name) {
            Ok(path) => {
                drop(directory);
                Some(PendingMessage {
                    sender,
                    content: PendingContent::Image { path },
                })
            }
            Err(error) => {
                drop(directory);
                notify(&format!("No se pudo guardar la imagen recibida. {error}"));
                None
            }
        },
        Ok(Some(share::Received::Files { sender, path })) => Some(PendingMessage {
            sender,
            content: PendingContent::Files { path },
        }),
        Ok(None) => None,
        Err(error) => {
            notify(&format!("No se pudo recibir. {error}"));
            None
        }
    };
    let Some(message) = message else {
        return;
    };
    let (unread, save_result) = UI.with_borrow_mut(|ui| {
        if ui.inbox.len() == 100 {
            if let Some(old) = ui.inbox.pop_front() {
                remove_message_file(&old);
            }
        }
        ui.inbox.push_back(message.clone());
        (ui.inbox.len(), save_inbox(&ui.inbox))
    });
    if let Err(error) = save_result {
        notify(&format!(
            "El mensaje se recibió, pero no pudo guardarse. {error}"
        ));
    }
    notify_received(&message, unread);
}

fn remove_message_file(message: &PendingMessage) {
    if let PendingContent::Image { path } = &message.content {
        let _ = std::fs::remove_file(path);
    }
}

pub fn open_received(hwnd: HWND) {
    if UI.with_borrow(|ui| ui.inbox.is_empty()) {
        notify("No hay mensajes pendientes.");
        return;
    }
    let _modal = panel_modal();
    while let Some((message, pending)) = UI.with_borrow(|ui| {
        ui.inbox
            .front()
            .cloned()
            .map(|message| (message, ui.inbox.len().saturating_sub(1)))
    }) {
        let shown = match &message.content {
            PendingContent::Text { text } => {
                crate::share_preview::text_pending(hwnd, &message.sender, text, pending)
            }
            PendingContent::Image { path } => {
                crate::share_preview::image_pending(hwnd, &message.sender, path, pending)
            }
            PendingContent::Files { path } => {
                crate::share_preview::files_pending(hwnd, &message.sender, path, pending)
            }
        };
        match shown {
            Ok(open_next) => {
                remove_message_file(&message);
                UI.with_borrow_mut(|ui| {
                    ui.inbox.pop_front();
                    if let Err(error) = save_inbox(&ui.inbox) {
                        ui.status = format!("No se pudo actualizar la bandeja. {error}");
                    }
                });
                if !open_next {
                    break;
                }
            }
            Err(error) => {
                notify(&format!("No se pudo abrir el mensaje. {error}"));
                break;
            }
        }
    }
}

pub fn on_event(hwnd: HWND) {
    while let Ok(event) = UI.with_borrow(|ui| ui.rx.try_recv()) {
        match event {
            Event::Devices(search, result) => {
                UI.with_borrow_mut(|ui| ui.sending = false);
                match result {
                    Ok(peers) => match search {
                        Search::Pair => choose_pair(hwnd, peers),
                        Search::Manage => manage_devices(hwnd, Some(&peers)),
                    },
                    Err(error) => notify(&format!("No se pudo buscar equipos. {error}")),
                }
            }
            Event::Discovery(result) => {
                if let Ok(peers) = result {
                    UI.with_borrow_mut(|ui| update_discovered_peers(ui, peers));
                    crate::with_app(|app| app.redraw());
                }
            }
            Event::Sent(result) => {
                UI.with_borrow_mut(|ui| ui.sending = false);
                match result {
                    Ok(()) => notify("Envío completado"),
                    Err(error) => notify(&format!("No se pudo enviar. {error}")),
                }
            }
            Event::Paired(result) => {
                UI.with_borrow_mut(|ui| {
                    ui.sending = false;
                    if result.is_ok() {
                        ui.pairing.close();
                    }
                });
                match result {
                    Ok(()) => notify(
                        "Equipo vinculado. Ya puedes compartir sin activar la recepción cada vez.",
                    ),
                    Err(error) => notify(&format!("No se pudo vincular. {error}")),
                }
            }
            Event::ConfirmPair {
                name,
                code,
                decision,
            } => confirm_pair(hwnd, name, code, decision),
            Event::Receive(id, event) => {
                if UI.with_borrow(|ui| ui.service_id != id) {
                    continue;
                }
                match event {
                    ReceiveEvent::Ready(code) => {
                        UI.with_borrow_mut(|ui| ui.receive_code = Some(code));
                        crate::with_app(|app| app.redraw());
                    }
                    ReceiveEvent::Pair {
                        name,
                        code,
                        decision,
                    } => confirm_pair(hwnd, name, code, decision),
                    ReceiveEvent::Paired(name) => notify(&format!(
                        "{name} quedó vinculado. Ya puedes compartir con él."
                    )),
                    ReceiveEvent::Offer {
                        description,
                        decision,
                    } => {
                        let code = UI.with_borrow_mut(|ui| {
                            ui.accepting = true;
                            ui.receive_code.clone().unwrap_or_default()
                        });
                        let accepted = crate::share_preview::confirm(hwnd, &description, &code);
                        let _ = decision.send(accepted);
                        UI.with_borrow_mut(|ui| ui.accepting = false);
                    }
                    ReceiveEvent::Completed { result, resume } => {
                        queue_received(result.map_err(|error| error.to_string()));
                        let _ = resume.send(());
                    }
                }
            }
            Event::Stopped(id, result) => {
                if UI.with_borrow(|ui| ui.service_id != id) {
                    continue;
                }
                UI.with_borrow_mut(|ui| {
                    ui.cancel = None;
                    ui.receive_code = None;
                });
                if let Err(error) = result {
                    notify(&format!("Se detuvo la recepción. {error}"));
                }
            }
        }
    }
}
