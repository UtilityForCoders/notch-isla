# Política de privacidad / Privacy policy

*Última actualización / Last updated: 8 de noviembre de 2026 / November 8, 2026*

## Español

Notch Isla no tiene telemetría, estadísticas de uso, anuncios ni cuentas.

Se conecta a internet solo para dos cosas. **Actualizaciones**: un minuto después de abrirse y luego cada 6 horas, la app pide a GitHub (`api.github.com`) el número de la última versión publicada. Si hay una versión nueva y la app está instalada, descarga su instalador desde `github.com` a la carpeta temporal; solo se instala si haces clic en el aviso. Las peticiones no llevan datos tuyos, solo el nombre y la versión de la app. Como cualquier conexión, GitHub ve tu dirección IP; se aplica la [declaración de privacidad de GitHub](https://docs.github.com/es/site-policy/privacy-policies/github-general-privacy-statement).

**Letra de la canción (opcional, apagada por defecto)**: si activas alguna de las opciones de **Mostrar letra de la canción** en Configuración (con la isla expandida o bajo la línea de reposo), por cada canción nueva la app pide su letra sincronizada a [LRCLIB](https://lrclib.net) (`lrclib.net`), una base de letras pública y gratuita. Para eso envía el **título, el artista, el álbum y la duración** de la canción que suena; LRCLIB también ve tu dirección IP. No se envía nada más ni se identifica tu cuenta de ninguna app. La letra se guarda solo en memoria mientras la app está abierta. Si no activas ninguna de las dos, no se hace ninguna consulta.

Para funcionar, la app usa información que ya está en tu PC, solo en memoria y mientras está abierta:

- **Música que suena**: título, artista, carátula, estado y avance de la canción, que Windows comparte con cualquier app a través de los controles multimedia del sistema (SMTC).
- **Ícono de la app que reproduce** (Spotify, Chrome...): se obtiene de Windows o del archivo `.exe` del programa en ejecución.
- **Volumen**: el de la app que suena o el general, solo se lee y se cambia cuando giras la rueda del mouse sobre la isla.
- **Salidas de audio**: sus nombres, para el aviso de cambio de salida y para el menú de salida por app.
- **Batería de periféricos**: nombres, identificadores locales, nivel y estado de carga que Windows o los receptores compatibles comunican. Las lecturas y el historial de avisos se mantienen solo en memoria. Se consulta el servicio de batería Bluetooth LE cuando es necesario; no se emparejan dispositivos ni se envía esta información a internet.
- **Sonido de la app que suena** (si está activado **Barras con el sonido real**, que viene activado): mientras se ven las barras del reproductor extendido, se lee lo que suena por los parlantes (no el micrófono) solo para calcular la altura de las barras. Se procesa en memoria y se descarta al instante: no se graba, no se guarda y no se envía.

Lo que la app escribe en tu PC:

- Sus **ajustes** del menú de la bandeja, en `HKEY_CURRENT_USER\Software\NotchIsla`.
- La opción **Iniciar con Windows**, si la activas: un valor llamado `NotchIsla` en `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`. Se quita al desactivar la opción o al desinstalar la app.
- Si eliges una **salida de audio para una app**, Windows la guarda en su propia configuración de sonido (la misma del Mezclador de volumen); se quita eligiendo **Predeterminada del sistema**.

**Compartir cerca** funciona en la red local, sin cuentas ni servidor. Con **Recibir de equipos vinculados** activado, la app anuncia el nombre del PC y su certificado público mediante mDNS mientras está abierta; la recepción permanece disponible hasta que la pauses y esa preferencia se conserva al reiniciar. Solo los equipos vinculados pueden enviar contenido. Para agregar uno nuevo, abres una ventana de vinculación de dos minutos y confirmas un código en ambos PCs. Las conexiones usan TLS con autenticación de ambos equipos. El certificado, la clave privada, los nombres y certificados de los equipos vinculados y la preferencia de recepción se guardan en `%LOCALAPPDATA%\NotchIsla\share.dat`, cifrados con DPAPI para el usuario de Windows. **Olvidar equipo** revoca la confianza en este PC. Al enviar, el destinatario recibe el nombre de tu PC, los nombres y tamaños de los elementos y el contenido seleccionado. El portapapeles se lee solo al elegir **Enviar texto o imagen copiada**; recibir no lo modifica hasta pulsar **Copiar**. El texto recibido se mantiene en memoria y las imágenes se guardan temporalmente hasta cerrar la vista previa. Los demás archivos solo se guardan en Descargas después de aceptarlos.

Preguntas: abre un *issue* en https://github.com/UtilityForCoders/notch-isla/issues

## English

Notch Isla has no telemetry, usage statistics, ads, or accounts.

It connects to the internet for only two things. **Updates**: one minute after it starts and then every 6 hours, the app asks GitHub (`api.github.com`) for the number of the latest published version. If there is a new version and the app is installed, it downloads the installer from `github.com` to the temporary folder; it is installed only if you click the notification. The requests carry none of your data, only the app's name and version. As with any connection, GitHub sees your IP address; the [GitHub General Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement) applies.

**Song lyrics (optional, off by default)**: if you turn on either option under **Mostrar letra de la canción** (show song lyrics) in the tray menu (with the island expanded or collapsed), for each new song the app requests its synced lyrics from [LRCLIB](https://lrclib.net) (`lrclib.net`), a free public lyrics database. To do so it sends the **title, artist, album, and duration** of the song that is playing; LRCLIB also sees your IP address. Nothing else is sent and no account of any app is identified. Lyrics are kept only in memory while the app is running. If you turn neither on, no request is made.

To work, the app uses information already on your PC, only in memory and only while it is running:

- **The music that is playing**: title, artist, cover art, playback state, and position, which Windows shares with any app through the system media transport controls (SMTC).
- **The icon of the app that is playing** (Spotify, Chrome...): obtained from Windows or from the running program's `.exe` file.
- **Volume**: that of the playing app or the overall volume, read and changed only when you turn the mouse wheel over the island.
- **Audio outputs**: their names, for the output-change notice and the per-app output menu.
- **Peripheral batteries**: names, local identifiers, battery levels and charging state reported by Windows or supported receivers. Readings and notification history stay in memory. The Bluetooth LE battery service is queried when needed; the app does not pair devices or send this information to the internet.
- **Sound of the playing app** (if **Barras con el sonido real** (bars with real sound) is on, which is the default): while the island's bars are visible, what plays through the speakers (not the microphone) is read only to compute the bar heights. It is processed in memory and discarded immediately: it is not recorded, stored, or sent.

What the app writes to your PC:

- Its tray-menu **settings**, under `HKEY_CURRENT_USER\Software\NotchIsla`.
- The **Start with Windows** option, if you enable it: a value named `NotchIsla` under `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`. It is removed when you turn the option off or uninstall the app.
- If you choose an **audio output for an app**, Windows stores it in its own sound settings (the same as the Volume mixer); choosing **Predeterminada del sistema** (system default) removes it.

**Nearby sharing** works on the local network without accounts or a server. With **Recibir de equipos vinculados** (receive from paired devices) enabled, the app advertises your computer name and public certificate using mDNS while running. Receiving stays available until paused, and that preference survives restarts. Only paired devices can send content. Adding a device opens a two-minute pairing window and requires confirming the same code on both computers. Connections use TLS with mutual device authentication. The certificate, private key, paired device names and certificates, and receiving preference are stored in `%LOCALAPPDATA%\NotchIsla\share.dat`, encrypted with Windows DPAPI for the current user. **Olvidar equipo** (forget device) revokes trust on this PC. A recipient receives your computer name, item names and sizes, and the selected content. The clipboard is read only when choosing **Enviar texto o imagen copiada** (send copied text or image); receiving changes it only after clicking **Copiar** (copy). Received text stays in memory and images are stored temporarily until their preview closes. Other files are saved to Downloads only after acceptance.

Questions: open an issue at https://github.com/UtilityForCoders/notch-isla/issues
