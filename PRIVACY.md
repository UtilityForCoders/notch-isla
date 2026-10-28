# Política de privacidad / Privacy policy

*Última actualización / Last updated: 27 de octubre de 2026 / October 27, 2026*

## Español

Notch Isla **no recopila, no guarda y no envía ningún dato personal**. No tiene telemetría, estadísticas de uso, anuncios ni cuentas.

Se conecta a internet solo para dos cosas. **Actualizaciones**: un minuto después de abrirse y luego cada 6 horas, la app pide a GitHub (`api.github.com`) el número de la última versión publicada. Si hay una versión nueva y la app está instalada, descarga su instalador desde `github.com` a la carpeta temporal; solo se instala si haces clic en el aviso. Las peticiones no llevan datos tuyos, solo el nombre y la versión de la app. Como cualquier conexión, GitHub ve tu dirección IP; se aplica la [declaración de privacidad de GitHub](https://docs.github.com/es/site-policy/privacy-policies/github-general-privacy-statement).

**Letra de la canción (opcional, apagada por defecto)**: si activas alguna de las opciones de **Mostrar letra de la canción** en el menú de la bandeja (con la isla expandida o encogida), por cada canción nueva la app pide su letra sincronizada a [LRCLIB](https://lrclib.net) (`lrclib.net`), una base de letras pública y gratuita. Para eso envía el **título, el artista, el álbum y la duración** de la canción que suena; LRCLIB también ve tu dirección IP. No se envía nada más ni se identifica tu cuenta de ninguna app. La letra se guarda solo en memoria mientras la app está abierta. Si no activas ninguna de las dos, no se hace ninguna consulta.

Para funcionar, la app usa información que ya está en tu PC, solo en memoria y mientras está abierta:

- **Música que suena**: título, artista, carátula, estado y avance de la canción, que Windows comparte con cualquier app a través de los controles multimedia del sistema (SMTC).
- **Ícono de la app que reproduce** (Spotify, Chrome...): se obtiene de Windows o del archivo `.exe` del programa en ejecución.
- **Volumen**: el de la app que suena o el general, solo se lee y se cambia cuando giras la rueda del mouse sobre la isla.
- **Salidas de audio**: sus nombres, para el aviso de cambio de salida y para el menú de salida por app.
- **Sonido de la app que suena** (si está activado **Barras con el sonido real**, que viene activado): mientras se ven las barras de la isla, se lee lo que suena por los parlantes (no el micrófono) solo para calcular la altura de las barras. Se procesa en memoria y se descarta al instante: no se graba, no se guarda y no se envía.

Lo que la app escribe en tu PC:

- Sus **ajustes** del menú de la bandeja, en `HKEY_CURRENT_USER\Software\NotchIsla`.
- La opción **Iniciar con Windows**, si la activas: un valor llamado `NotchIsla` en `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`. Se quita al desactivar la opción o al desinstalar la app.
- Si eliges una **salida de audio para una app**, Windows la guarda en su propia configuración de sonido (la misma del Mezclador de volumen); se quita eligiendo **Predeterminada del sistema**.

Fuera de las actualizaciones y de la letra (si la activas), ningún dato sale de tu equipo ni se comparte con terceros.

Preguntas: abre un *issue* en https://github.com/gportal21/notch-isla/issues

## English

Notch Isla **does not collect, store, or send any personal data**. It has no telemetry, usage statistics, ads, or accounts.

It connects to the internet for only two things. **Updates**: one minute after it starts and then every 6 hours, the app asks GitHub (`api.github.com`) for the number of the latest published version. If there is a new version and the app is installed, it downloads the installer from `github.com` to the temporary folder; it is installed only if you click the notification. The requests carry none of your data, only the app's name and version. As with any connection, GitHub sees your IP address; the [GitHub General Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement) applies.

**Song lyrics (optional, off by default)**: if you turn on either option under **Mostrar letra de la canción** (show song lyrics) in the tray menu (with the island expanded or collapsed), for each new song the app requests its synced lyrics from [LRCLIB](https://lrclib.net) (`lrclib.net`), a free public lyrics database. To do so it sends the **title, artist, album, and duration** of the song that is playing; LRCLIB also sees your IP address. Nothing else is sent and no account of any app is identified. Lyrics are kept only in memory while the app is running. If you turn neither on, no request is made.

To work, the app uses information already on your PC, only in memory and only while it is running:

- **The music that is playing**: title, artist, cover art, playback state, and position, which Windows shares with any app through the system media transport controls (SMTC).
- **The icon of the app that is playing** (Spotify, Chrome...): obtained from Windows or from the running program's `.exe` file.
- **Volume**: that of the playing app or the overall volume, read and changed only when you turn the mouse wheel over the island.
- **Audio outputs**: their names, for the output-change notice and the per-app output menu.
- **Sound of the playing app** (if **Barras con el sonido real** (bars with real sound) is on, which is the default): while the island's bars are visible, what plays through the speakers (not the microphone) is read only to compute the bar heights. It is processed in memory and discarded immediately: it is not recorded, stored, or sent.

What the app writes to your PC:

- Its tray-menu **settings**, under `HKEY_CURRENT_USER\Software\NotchIsla`.
- The **Start with Windows** option, if you enable it: a value named `NotchIsla` under `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`. It is removed when you turn the option off or uninstall the app.
- If you choose an **audio output for an app**, Windows stores it in its own sound settings (the same as the Volume mixer); choosing **Predeterminada del sistema** (system default) removes it.

Apart from updates and lyrics (if you turn them on), no data leaves your computer or is shared with third parties.

Questions: open an issue at https://github.com/gportal21/notch-isla/issues
