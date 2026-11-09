//! Acciones de compartir en la isla y diálogos de confirmación de Windows.

use crate::share_identity::Store;
use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;

use windows::core::{w, Result, HSTRING, PWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::Shell::{
    DragFinish, DragQueryFileW, FOLDERID_Downloads, FileOpenDialog, IFileOpenDialog,
    SHGetKnownFolderPath, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_PATHMUSTEXIST, HDROP, KF_FLAG_DEFAULT, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, PostMessageW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, WM_APP,
};

use crate::island::{Notice, NoticeIcon};
use crate::menu::{self, Item};
use crate::share::{self, Payload, Peer, ReceiveEvent};

pub const WM_SHARE_EVENT: u32 = WM_APP + 12;
pub const WM_SHARE_MENU: u32 = WM_APP + 13;
const TITLE: &str = "Compartir cerca";

enum Search {
    Send(Payload),
    Pair,
    Manage,
}

enum Event {
    Devices(Search, std::result::Result<Vec<Peer>, String>),
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
    sending: bool,
    accepting: bool,
    status: String,
    modal: bool,
}

thread_local! {
    static UI: RefCell<Ui> = {
        let (tx, rx) = mpsc::channel();
        RefCell::new(Ui { tx, rx, store: None, pairing: Arc::new(share::Pairing::default()), cancel: None, service_id: 0, receive_code: None, sending: false, accepting: false, status: String::new(), modal: false })
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
    if store().is_some_and(|store| store.enabled()) {
        start_receive(hwnd);
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
    UI.with_borrow(|ui| crate::workspace::ShareState {
        receiving: ui.cancel.is_some(),
        devices: ui.store.as_ref().map_or(0, |store| store.device_count()),
        busy: ui.sending || ui.accepting || ui.modal,
        status: ui.status.clone(),
    })
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
        ShareAction::Pair => start_pairing(hwnd),
        ShareAction::Devices => start_search(hwnd, Search::Manage),
        ShareAction::Receive => {
            let Some(store) = store() else {
                return;
            };
            if UI.with_borrow(|ui| ui.cancel.is_some()) {
                match store.set_enabled(false) {
                    Ok(()) => {
                        stop();
                        notify("Recepción pausada");
                    }
                    Err(error) => notify(&format!("No se pudo pausar. {error}")),
                }
            } else {
                enable_receive(hwnd);
                if UI.with_borrow(|ui| ui.cancel.is_some()) {
                    notify("Recepción activada");
                }
            }
        }
    }
    crate::with_app(|app| app.redraw());
}

pub fn open(hwnd: HWND) {
    let Some(store) = store() else {
        return;
    };
    let (receiving, accepting, pairing) =
        UI.with_borrow(|ui| (ui.cancel.is_some(), ui.accepting, ui.pairing.is_open()));
    let mut items = vec![
        Item::Label(TITLE.into()),
        action(1, "Elegir fotos o archivos…"),
        action(2, "Enviar texto o imagen copiada"),
        Item::Separator,
        action(4, "Vincular equipo…"),
        action(5, format!("Tus dispositivos · {}", store.devices().len())),
        Item::Separator,
    ];
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
        Some(3) if receiving => match store.set_enabled(false) {
            Ok(()) => {
                stop();
                notify("Recepción pausada. Tus equipos siguen vinculados.");
            }
            Err(error) => notify(&format!("No se pudo pausar la recepción. {error}")),
        },
        Some(3) => enable_receive(hwnd),
        Some(4) => start_pairing(hwnd),
        Some(5) => start_search(hwnd, Search::Manage),
        Some(6) => {
            UI.with_borrow(|ui| ui.pairing.close());
            notify("Vinculación cerrada. Solo pueden enviar tus equipos guardados.");
        }
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

fn clipboard_text(hwnd: HWND) -> Option<String> {
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
    start_search(hwnd, Search::Send(payload));
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

fn enable_receive(hwnd: HWND) {
    let Some(store) = store() else {
        return;
    };
    if let Err(error) = store.set_enabled(true) {
        notify(&format!("No se pudo activar la recepción. {error}"));
        return;
    }
    start_receive(hwnd);
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

fn choose_device(hwnd: HWND, payload: Payload, peers: Vec<Peer>) {
    let Some(store) = store() else {
        return;
    };
    let _modal = panel_modal();
    let description = match &payload {
        Payload::Image(_) => "Imagen copiada".into(),
        Payload::Text(text) => format!("Texto · {} caracteres", text.chars().count()),
        Payload::Files(files) if files.len() == 1 => files[0].file_name().map_or_else(
            || "1 archivo".into(),
            |name| name.to_string_lossy().into_owned(),
        ),
        Payload::Files(files) => format!("{} archivos", files.len()),
    };
    let mut items = vec![
        Item::Label(description),
        Item::Label("Tus dispositivos".into()),
    ];
    let devices = store.devices();
    let mut available = Vec::new();
    for device in devices {
        if let Some(peer) = peers
            .iter()
            .find(|peer| peer.certificate.as_ref() == device.certificate)
        {
            let mut peer = peer.clone();
            peer.name = device.name.clone();
            available.push(peer);
            items.push(action(
                available.len(),
                format!("{} · Disponible", device.name),
            ));
        } else {
            items.push(Item::Label(format!("{} · Sin conexión", device.name)));
        }
    }
    if available.is_empty() {
        items.push(Item::Label(
            "Abre Notch Isla y permite recibir en el otro PC".into(),
        ));
    }
    items.push(Item::Separator);
    items.push(action(1000, "Vincular otro equipo…"));
    let selected = menu::show(items);
    if selected == Some(1000) {
        start_pairing(hwnd);
        return;
    }
    let Some(peer) = selected
        .and_then(|id| id.checked_sub(1))
        .and_then(|index| available.into_iter().nth(index))
    else {
        return;
    };
    UI.with_borrow_mut(|ui| ui.sending = true);
    notify("Enviando al equipo vinculado…");
    let report = reporter(hwnd);
    thread::spawn(move || {
        report(Event::Sent(
            share::send(&store, peer, payload).map_err(|error| error.to_string()),
        ))
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

fn manage_devices(peers: Vec<Peer>) {
    let Some(store) = store() else {
        return;
    };
    let _modal = panel_modal();
    let devices = store.devices();
    let mut items = vec![Item::Label("Tus dispositivos".into())];
    for (index, device) in devices.iter().enumerate() {
        let online = peers
            .iter()
            .any(|peer| peer.certificate.as_ref() == device.certificate);
        items.push(Item::Group {
            label: format!(
                "{} · {}",
                device.name,
                if online {
                    "Disponible"
                } else {
                    "Sin conexión"
                }
            ),
            items: vec![action(index + 1, "Olvidar equipo")],
        });
    }
    if devices.is_empty() {
        items.push(Item::Label("Todavía no hay equipos vinculados".into()));
    }
    if let Some(device) = menu::show(items)
        .and_then(|id| id.checked_sub(1))
        .and_then(|index| devices.get(index))
    {
        match store.forget(&device.certificate) {
            Ok(()) => notify(&format!("{} ya no es un equipo de confianza.", device.name)),
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

fn show_received(hwnd: HWND, result: std::result::Result<Option<share::Received>, String>) {
    match result {
        Ok(Some(share::Received::Text { sender, text })) => {
            if let Err(error) = crate::share_preview::text(hwnd, &sender, &text) {
                notify(&format!("No se pudo mostrar el texto. {error}"));
            }
        }
        Ok(Some(share::Received::Image {
            sender,
            directory,
            path,
        })) => {
            if let Err(error) = crate::share_preview::image(hwnd, &sender, &path) {
                notify(&format!("No se pudo mostrar la imagen. {error}"));
            }
            drop(directory);
        }
        Ok(Some(share::Received::Files(path))) => {
            let message = HSTRING::from(format!("Guardado en:\n{}", path.display()));
            unsafe {
                MessageBoxW(
                    None,
                    &message,
                    w!("Envío recibido · Notch Isla"),
                    MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
                );
            }
        }
        Ok(None) => {}
        Err(error) => notify(&format!("No se pudo recibir. {error}")),
    }
}

pub fn on_event(hwnd: HWND) {
    while let Ok(event) = UI.with_borrow(|ui| ui.rx.try_recv()) {
        match event {
            Event::Devices(search, result) => {
                UI.with_borrow_mut(|ui| ui.sending = false);
                match result {
                    Ok(peers) => match search {
                        Search::Send(payload) => choose_device(hwnd, payload, peers),
                        Search::Pair => choose_pair(hwnd, peers),
                        Search::Manage => manage_devices(peers),
                    },
                    Err(error) => notify(&format!("No se pudo buscar equipos. {error}")),
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
                        UI.with_borrow_mut(|ui| ui.accepting = true);
                        show_received(hwnd, result.map_err(|error| error.to_string()));
                        UI.with_borrow_mut(|ui| ui.accepting = false);
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
