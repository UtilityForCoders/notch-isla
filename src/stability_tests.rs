use super::*;
use windows::Win32::System::Threading::{GetCurrentProcess, GetGuiResources, GR_GDIOBJECTS};
use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, PM_REMOVE};

#[test]
#[ignore = "ejercita la isla real fuera de pantalla; requiere DWM y escritorio interactivo"]
fn isla_con_desenfoque_sigue_ocultandose_y_recibiendo_clics() {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
        let instance = GetModuleHandleW(None).unwrap();
        let class = w!("NotchIslaStabilityProbe");
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        });
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            class,
            WS_POPUP,
            -32000,
            -32000,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .unwrap();
        let mut settings = Settings::load();
        settings.high_refresh = true;
        settings.translucent = true;
        settings.hide_delay = 1;
        settings.hide_fullscreen = false;
        settings.real_bars = false;
        settings.lyrics_island = false;
        settings.lyrics_projector = false;
        settings.hotkey = None;
        let mut app = App::new(hwnd, monitors::find(""), settings).unwrap();
        app.tray.remove();
        app.pos = POINT {
            x: -32000,
            y: -32000,
        };
        app.x_spring.reset(-32000.0);
        app.hz = 240;
        app.high_refresh_driver = HighRefreshDriver::with_sync(hwnd, || Ok(()));
        app.media = Some(NowPlaying {
            title: "Prueba local".into(),
            artist: "Sin sesión multimedia real".into(),
            album: String::new(),
            playing: true,
            cover: None,
            timeline: Some(media::Timeline {
                position: 0.0,
                duration: 3600.0,
                read_at: Instant::now(),
            }),
            app_icon: None,
            app_id: String::new(),
            sessions: (0, 1),
        });
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        with_app(|app| {
            app.update_visibility();
            app.redraw();
        });
        if std::env::var_os("NOTCH_DRAG_BENCH").is_some() {
            with_app(|app| {
                app.settings.hide_delay = 0;
                app.stop_idle_countdown();
                app.pos.y = 0;
                app.presence.reset(1.0);
                app.spring.reset(1.0);
            });
            for translucent in [false, true] {
                with_app(|app| {
                    app.settings.translucent = translucent;
                    app.redraw();
                    assert_eq!(
                        app.backdrop
                            .as_ref()
                            .is_some_and(|backdrop| backdrop.is_shown()),
                        translucent,
                        "el caso medido debe tener el desenfoque realmente activo"
                    );
                });
                let mut times = Vec::new();
                for frame in 0..240 {
                    let start = Instant::now();
                    with_app(|app| {
                        let rc = app.monitor_rect();
                        let center =
                            rc.left + ((rc.right - rc.left) - app.surface.width as i32) / 2;
                        let offset = if frame < 120 {
                            frame * 2
                        } else {
                            (240 - frame) * 2
                        };
                        app.set_island_x(center + offset - 120);
                    });
                    let mut msg = MSG::default();
                    if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        DispatchMessageW(&msg);
                    }
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(f64::total_cmp);
                eprintln!(
                    "ARRASTRE blur={translucent}: media={:.3} ms p95={:.3} ms max={:.3} ms",
                    times.iter().sum::<f64>() / times.len() as f64,
                    times[228],
                    times[239]
                );
            }
            with_app(|app| {
                app.set_timer(None);
                app.tray.remove();
            });
            let _ = DestroyWindow(hwnd);
            APP.with(|cell| cell.borrow_mut().take());
            return;
        }
        let seconds = std::env::var("NOTCH_STRESS_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30)
            .max(5);
        let started = Instant::now();
        let mut next_action = started;
        let mut expanded = false;
        let mut hides = 0;
        let mut clicks = 0;
        let mut actions = 0;
        let baseline_gdi = GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS);
        while started.elapsed() < Duration::from_secs(seconds) {
            let mut msg = MSG::default();
            if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
                ui::promote_timers();
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
            if Instant::now() >= next_action {
                next_action = Instant::now() + Duration::from_millis(500);
                let mut sent_click = false;
                with_app(|app| {
                    if app.idle_hidden {
                        hides += 1;
                        app.reveal();
                    }
                    // Clic real de entrada, enviado mientras el estado está prestado para
                    // provocar la reentrada que antes descartaba los botones.
                    if app.visible {
                        // La prueba de clics corresponde a controles ya abiertos: durante
                        // la transición hit_test los deshabilita deliberadamente.
                        app.spring.reset(1.0);
                        app.presence.reset(1.0);
                        let button = (0..app.surface.height).step_by(4).find_map(|y| {
                            (0..app.surface.width).step_by(4).find_map(|x| {
                                (app.hit_at(x as f32, y as f32)
                                    == Some(Hit::Button(Button::PlayPause)))
                                .then_some((x, y))
                            })
                        });
                        if let Some((x, y)) = button {
                            app.expanded_by_click = true;
                            let point = LPARAM(((y << 16) | x) as isize);
                            wndproc(hwnd, WM_LBUTTONDOWN, WPARAM(1), point);
                            wndproc(hwnd, WM_LBUTTONUP, WPARAM(0), point);
                            sent_click = true;
                        }
                    }
                    expanded = !expanded;
                    app.aim_expansion(expanded);
                    app.settings.blur_radius = (actions % 41) as u32;
                    actions += 1;
                });
                with_app(|app| {
                    if sent_click {
                        assert!(
                            app.pressed
                                .is_some_and(|(button, _)| button == Button::PlayPause),
                            "el clic debe llegar al botón de reproducción"
                        );
                        clicks += 1;
                    }
                    assert!(
                        app.drag_island.is_none(),
                        "el clic debe liberar el arrastre"
                    );
                    assert!(app.dragging.is_none(), "el mouse no debe quedar capturado");
                });
            }
        }
        let gdi = GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS);
        with_app(|app| {
            app.set_timer(None);
            app.tray.remove();
        });
        let _ = DestroyWindow(hwnd);
        APP.with(|cell| cell.borrow_mut().take());
        assert!(hides >= seconds / 3, "debe seguir ocultándose: {hides}");
        assert!(
            clicks >= seconds / 5,
            "los botones deben seguir recibiendo clics: {clicks}"
        );
        assert!(
            gdi <= baseline_gdi + 32,
            "recursos GDI estables: {baseline_gdi} -> {gdi}"
        );
        eprintln!("{seconds} s de isla real: {hides} ocultaciones, {clicks} clics, {actions} cambios; GDI {baseline_gdi} -> {gdi}");
    }
}
