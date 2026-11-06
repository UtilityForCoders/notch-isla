use super::*;
use windows::Win32::System::Threading::{GetCurrentProcess, GetGuiResources, GR_GDIOBJECTS};
use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, PM_REMOVE};

#[test]
#[ignore = "ejercita la isla real fuera de pantalla; requiere DWM y escritorio interactivo"]
fn isla_con_desenfoque_sigue_ocultandose_y_recibiendo_clics() {
    unsafe {
        windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
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
        settings.pinned_expanded = false;
        settings.hotkey = None;
        let mut app = App::new(hwnd, monitors::find(""), settings).unwrap();
        app.tray.remove();
        app.pos = POINT {
            x: -32000,
            y: -32000,
        };
        app.x_spring.reset(-32000.0);
        app.hz = 240;
        let animation_bench = std::env::var_os("NOTCH_ANIMATION_BENCH").is_some();
        let drag_bench = std::env::var_os("NOTCH_DRAG_BENCH").is_some();
        if !animation_bench && !drag_bench {
            app.monitor_bounds = RECT {
                left: -32000,
                top: -32000,
                right: -30000,
                bottom: -30000,
            };
        }
        app.high_refresh_driver = if animation_bench {
            HighRefreshDriver::new(hwnd)
        } else {
            HighRefreshDriver::with_sync(hwnd, || Ok(()))
        };
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
        if animation_bench {
            let monitor = monitors::find("");
            app.hz = monitors::refresh_rate(monitor);
            app.settings.hide_delay = 0;
            app.settings.lyrics_island = true;
            app.lyrics_spring.reset(1.0);
            app.lyrics.insert(
                lyrics::key("Prueba local", "Sin sesión multimedia real"),
                (0..12)
                    .map(|i| lyrics::Verse {
                        start: i as f32 * 2.0,
                        text: format!("Letra local para medir la animación del verso {i}"),
                        words: Vec::new(),
                    })
                    .collect(),
            );
            let rc = app.monitor_rect();
            app.pos = POINT {
                x: rc.left + ((rc.right - rc.left) - app.surface.width as i32) / 2,
                y: rc.top + 200,
            };
            app.x_spring.reset(app.pos.x as f32);
        }
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        with_app(|app| {
            app.update_visibility();
            app.redraw();
        });
        if animation_bench {
            let seconds = std::env::var("NOTCH_STRESS_SECONDS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(5)
                .max(2);
            let started = Instant::now();
            let mut next_action = started;
            let mut expanded = false;
            let mut frames = Vec::new();
            let mut costs = Vec::new();
            while started.elapsed() < Duration::from_secs(seconds) {
                if Instant::now() >= next_action {
                    expanded = !expanded;
                    with_app(|app| app.aim_expansion(expanded));
                    next_action += Duration::from_millis(400);
                }
                let mut msg = MSG::default();
                if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let frame = Instant::now();
                    DispatchMessageW(&msg);
                    if msg.message == WM_ANIM_TICK {
                        frames.push(frame);
                        costs.push(frame.elapsed().as_secs_f64() * 1000.0);
                    }
                    ui::promote_timers();
                } else {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
            let mut hz = 0;
            with_app(|app| {
                hz = app.hz;
                assert!(app
                    .backdrop
                    .as_ref()
                    .is_some_and(|backdrop| backdrop.is_shown()));
                app.set_timer(None);
                app.tray.remove();
            });
            let _ = DestroyWindow(hwnd);
            APP.with(|cell| cell.borrow_mut().take());
            let mut gaps = frames
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).as_secs_f64() * 1000.0)
                .collect::<Vec<_>>();
            costs.sort_by(f64::total_cmp);
            gaps.sort_by(f64::total_cmp);
            assert!(!costs.is_empty() && !gaps.is_empty());
            eprintln!("ISLA con letra y blur: {} cuadros / {seconds} s, objetivo {hz} Hz; intervalo medio={:.3} ms p95={:.3} ms; dibujado medio={:.3} ms p95={:.3} ms max={:.3} ms",
                frames.len(), gaps.iter().sum::<f64>() / gaps.len() as f64, gaps[gaps.len()*95/100],
                costs.iter().sum::<f64>() / costs.len() as f64, costs[costs.len()*95/100], costs[costs.len()-1]);
            assert!(
                frames.len() as u64 >= seconds * hz as u64 * 80 / 100,
                "la isla real debe seguir el ritmo del monitor"
            );
            return;
        }
        if drag_bench {
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
        with_app(|app| {
            let media = app.media.as_ref().unwrap();
            let current_song = lyrics::key(&media.title, &media.artist);
            app.set_lyrics(lyrics::Lyrics {
                key: current_song.clone(),
                lines: Vec::new(),
            });
            for i in 0..MAX_CACHED_LYRICS {
                app.set_lyrics(lyrics::Lyrics {
                    key: format!("Canción {i}"),
                    lines: Vec::new(),
                });
            }
            assert_eq!(app.lyrics.len(), MAX_CACHED_LYRICS);
            assert!(app.lyrics.contains_key(&current_song));
            assert!(!app.lyrics.contains_key("Canción 0"));

            let rc = app.monitor_rect();
            let anchor = app.x_spring.value;
            let saved_offset = app.settings.offset_x;
            let mut window = RECT::default();
            for (edge, left) in [
                (rc.left - app.surface.width as i32, true),
                (rc.right, false),
            ] {
                app.spring.reset(0.0);
                app.set_island_x(edge);
                let collapsed = app.pos.x;
                let offset = app.settings.offset_x;
                app.spring.reset(1.0);
                app.render();
                let compact = app.pos.x;
                assert!(if left {
                    compact > collapsed
                } else {
                    compact < collapsed
                });
                app.lyrics_spring.reset(1.0);
                app.render();
                assert!(if left {
                    app.pos.x > compact
                } else {
                    app.pos.x < compact
                });
                windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut window).unwrap();
                assert_eq!(window.left, app.pos.x);
                app.spring.reset(0.0);
                app.lyrics_spring.reset(0.0);
                app.render();
                assert_eq!(app.pos.x, collapsed);
                assert_eq!(app.settings.offset_x, offset);
            }
            app.settings.offset_x = saved_offset;
            app.x_spring.reset(anchor);
            app.render();

            app.settings.pinned_expanded = true;
            app.hovered = false;
            app.spring.reset(1.0);
            app.show_notice(
                Notice {
                    icon: NoticeIcon::Mouse,
                    color: [120, 220, 160],
                },
                "Dispositivo conectado",
                "Mouse de prueba",
                50,
            );
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut msg = MSG::default();
            if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
                ui::promote_timers();
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut restored = false;
            with_app(|app| {
                assert_eq!(app.spring.value, 1.0, "la isla debe seguir extendida");
                restored = app.notice.is_none()
                    && app.last_notice.is_none()
                    && app.notice_spring.value == 0.0;
            });
            if restored {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "el aviso debe ceder al reproductor"
            );
        }
        with_app(|app| {
            let notices = app.settings.notices;
            let connections = app.settings.connection_notices;
            app.settings.notices = true;
            app.settings.connection_notices = true;
            let headset = battery::Device {
                id: "bt:prueba".into(),
                name: "Audífonos de prueba".into(),
                kind: battery::DeviceKind::Headphones,
                connection: "Bluetooth",
                level: Some(battery::Level::Percent(80)),
                charging: false,
                stale: false,
                windows_cached: false,
                detail: String::new(),
            };
            let audio = |app: &mut App| {
                app.queue_audio_notice("Salida de audio".into(), headset.name.clone(), true);
            };
            let connect = |app: &mut App| {
                app.connection_pending.push_back(headset.clone());
                app.show_next_notice();
            };
            for audio_first in [true, false] {
                let (first, second) = if audio_first {
                    (&audio as &dyn Fn(&mut App), &connect as &dyn Fn(&mut App))
                } else {
                    (&connect as &dyn Fn(&mut App), &audio as &dyn Fn(&mut App))
                };
                first(app);
                let shown = app.notice_card.as_ref().unwrap().window();
                second(app);
                assert_eq!(app.notice_card.as_ref().unwrap().window(), shown);
                assert_eq!(app.audio_notice_pending.is_some(), !audio_first);
                assert_eq!(app.connection_pending.len(), usize::from(audio_first));
                app.clear_notice_card();
                app.show_next_notice();
                assert!(app.notice_card.is_some());
                assert!(app.audio_notice_pending.is_none() && app.connection_pending.is_empty());
                app.clear_notice_card();
            }
            let battery_notices = app.settings.battery_notices;
            app.settings.battery_notices = true;
            let low_phone = battery::Device {
                level: Some(battery::Level::Percent(20)),
                name: "Teléfono de prueba".into(),
                windows_cached: true,
                ..headset.clone()
            };
            audio(app);
            app.battery_pending
                .extend(app.battery_alerts.update(&[low_phone], true));
            app.show_next_notice();
            assert_eq!(app.battery_pending.len(), 1);
            app.clear_notice_card();
            app.show_next_notice();
            let notice = app.notice.as_ref().unwrap();
            assert_eq!(notice.title, "Batería baja");
            assert_eq!(notice.subtitle, "Teléfono de prueba · 20 % · Baja");
            app.end_notice();
            app.settings.battery_notices = battery_notices;
            app.settings.notices = notices;
            app.settings.connection_notices = connections;
        });
        for expanded in [true, false] {
            with_app(|app| {
                app.settings.pinned_expanded = expanded;
                app.spring.reset(if expanded { 1.0 } else { 0.0 });
                app.presence.reset(1.0);
                app.show_notice_card(
                    Notice {
                        icon: if expanded {
                            NoticeIcon::Mouse
                        } else {
                            NoticeIcon::Audio
                        },
                        color: if expanded { GREEN } else { BLUE },
                    },
                    if expanded {
                        "Dispositivo conectado"
                    } else {
                        "Salida de audio"
                    },
                    if expanded {
                        "MCHOSE K7 Ultra · USB / 2.4 GHz · 78 %"
                    } else {
                        "Audífonos inalámbricos"
                    },
                    300,
                );
                let card = app
                    .notice_card
                    .as_mut()
                    .expect("crear la ventana del aviso");
                card.set_display(
                    app.scale,
                    RECT {
                        left: -32000,
                        top: -32000,
                        right: -30000,
                        bottom: -30000,
                    },
                )
                .unwrap();
                app.place_notice_card();
                let mut rect = RECT::default();
                windows::Win32::UI::WindowsAndMessaging::GetWindowRect(
                    app.notice_card.as_ref().unwrap().window(),
                    &mut rect,
                )
                .unwrap();
                let bottom = app.pos.y
                    + island::bottom(
                        app.surface.width as f32,
                        app.surface.height as f32,
                        app.scale,
                        app.spring.value,
                        app.presence.value,
                        app.shape(),
                    )
                    .ceil() as i32;
                assert!(rect.top > bottom, "la tarjeta queda debajo de la isla");
                assert!(app.notice.is_none(), "la tarjeta no ocupa el reproductor");
            });
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let mut msg = MSG::default();
                if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                    ui::promote_timers();
                } else {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let mut closed = false;
                with_app(|app| {
                    assert_eq!(app.spring.value, if expanded { 1.0 } else { 0.0 });
                    closed = app.notice_card.is_none();
                });
                if closed {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "la tarjeta debe cerrarse a tiempo"
                );
            }
        }
        with_app(|app| {
            app.settings.pinned_expanded = false;
            app.start_idle_countdown();
        });

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
