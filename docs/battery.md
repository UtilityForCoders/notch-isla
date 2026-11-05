# Batería de periféricos

Abre **Dispositivos y batería** como submenú lateral del menú de la bandeja.
El menú principal conserva su tamaño y sigue disponible para cambiar otros
ajustes. Pasa el puntero, haz clic o usa la flecha derecha para abrir un grupo;
izquierda o Esc vuelve al nivel anterior. Solo una lista que no quepa en
pantalla necesita desplazamiento con la rueda.
Cada dispositivo activo tiene una fila con el ícono del tipo, el nombre, el
nivel de batería y el ícono de conexión. El símbolo de Bluetooth identifica
ese enlace, las ondas indican radio (como 2.4 GHz) y el símbolo USB identifica
los dispositivos detectados por USB. El estado de carga aparece junto al nivel
cuando se informa. Los avisos de conexión y batería baja también usan el ícono
del periférico cuando se conoce su tipo.

La detección usa la clase de Bluetooth clásico, la apariencia de Bluetooth LE
y la información de mandos y receptores que expone Windows. Los perfiles de
mouse conocidos tienen prioridad sobre las interfaces de teclado para macros.
Si la información es ambigua o falta, la lista y el aviso de conexión muestran
un ícono genérico, y el aviso de batería baja muestra el de batería.

La consulta se hace al abrir la app, cada 5 segundos con los avisos de conexión activados
(cada 30 segundos si se desactivan) y al pulsar **Actualizar ahora**. Abrir el
menú general también pide una actualización. El grupo recibe las nuevas lecturas
mientras permanece abierto.

El aviso de batería baja está activado por defecto y se puede desactivar en ese
apartado. Un porcentaje válido menor de 20 dispara el aviso; exactamente 20 no.
Los mandos que solo comunican niveles aproximados avisan con el nivel **Baja**,
sin atribuirle un porcentaje. No se avisa cargando ni usando lecturas anteriores.
Cada dispositivo avisa una vez por descarga durante la sesión de la app; vuelve
a habilitarse el aviso tras detectar carga o al alcanzar el 25 %. Los avisos
esperan a que termine otro aviso y a que se salga de pantalla completa.

**Avisar al conectar un dispositivo**, activado por defecto, muestra una tarjeta
**Dispositivo conectado** debajo de la isla, con nombre, conexión y batería si hay una lectura
actual. Funciona con los periféricos detectados por las mismas fuentes del panel:
Bluetooth, mandos de Windows y receptores compatibles. El primer sondeo de cada
fuente establece la lista inicial sin anunciar los equipos ya conectados; volver
a activar la opción tampoco los anuncia de golpe. Los cambios de dispositivos
de Windows piden una lectura inmediata y el sondeo permite detectar un mouse
encendido sin retirar su dongle. La detección puede tardar unos 5 segundos más
el tiempo de respuesta del controlador.

La lista conserva el dispositivo durante un fallo breve de detección. La
ausencia se confirma con sondeos válidos separados por al menos cinco segundos;
un fallo de la fuente interrumpe esa comprobación. Después de una ausencia
confirmada, volver a detectar el enlace sí genera un aviso.

Un dongle que sigue presente solo genera un aviso la primera vez que el
periférico responde. El mismo receptor puede perder la lectura durante una
suspensión o un apagado. Recuperarla en ese caso actualiza la fila sin un
segundo aviso. Si se retira el dongle durante el tiempo de confirmación,
volver a conectarlo y obtener una respuesta habilita otro aviso.
Los avisos esperan a que se cierre el menú o se salga de pantalla completa, sin
reemplazar otro aviso. La batería baja tiene prioridad sobre la conexión.

Un receptor USB puede seguir presente con el mouse apagado o dormido. Una
respuesta vacía o inválida significa **sin lectura**, nunca 0 %. Si ya hubo una
lectura válida, se conserva en memoria y se marca **anterior / Última lectura**.
Un receptor que ya respondió conserva su fila durante los primeros 15 segundos
sin lectura. Si la falta de respuesta continúa, pasa a **Receptores sin enlace
confirmado**, con su porcentaje como detalle histórico. En MCHOSE también
se comprueba el bit de enlace del informe, para rechazar un porcentaje
almacenado sin mouse enlazado.
Al despertar el mouse, usa **Actualizar ahora** para obtener el nivel de nuevo.
La lista incluye los dispositivos Bluetooth conectados y los periféricos USB
reconocidos; no pretende listar todos los dispositivos del administrador de Windows.

Cambiar un mouse de 2.4 GHz a Bluetooth no proporciona necesariamente un
identificador común. En la unidad K7 Ultra probada Windows asigna diferentes
ContainerId al receptor USB y al enlace Bluetooth; el USB expone además el
serial genérico `0123456789`. No se fusionan dispositivos por nombre, modelo ni
porcentaje. Una identidad común fiable requeriría un serial único compartido,
un identificador del periférico expuesto por el protocolo o una vinculación
manual explícita. Esta versión separa el receptor inactivo, sin afirmar esa
identidad común; tampoco comparte entre transportes el historial de avisos.

## Compatibilidad

| Ruta | Implementación y estado |
|---|---|
| Bluetooth clásico | Batería del nodo PnP que expone Windows; conexión y nombre mediante WinRT. Solo se incluyen enlaces conectados. |
| Bluetooth LE | Misma ruta de Windows, con lectura del servicio estándar BAS `180F`, característica `2A19`, como alternativa. |
| MCHOSE K7 Ultra | **Probado físicamente por 2.4 GHz el 4 de noviembre de 2026**. Receptor `5253:1021`, colección `FF01:0001`, consulta `06` en feature report `11`. Lectura observada: **78 %**, sin cargar. También se comprobaron respuestas sin datos al dormir. |
| MCHOSE de la misma familia | Receptor `5253:1020` y variantes K7 identificadas. Consulta de estado invertida en report `11`, con alternativa `12`. Pendientes de prueba en otros modelos. |
| AJAZZ AJ159 / Pro, familia CompX | Receptores `248A/249A:5C2F` y `248A:5B2F`, interfaz 2. Consulta de frecuencia para comprobar que el mouse responde y luego identidad `10`, batería en byte 13. Se exige respuesta válida, checksum y modelo `M620/P630/M630`. Implementado; **pendiente de prueba física** en el AJ159 Pro. |
| AJAZZ receptor APEX 8K | `3151:5007`, colección `FFFF:0002`. Consulta `F7` y lectura del informe `05`. No se confunde con el protocolo CompX. Pendiente de prueba física. |
| GameSir G7 Pro y MANBA One V1 | Informe de `Windows.Gaming.Input.RawGameController`. Si Windows no entrega capacidades de batería, aparece **no disponible**. Los informes con cuatro niveles se presentan como aproximados. **Pendientes de prueba física**; no se garantiza que el MANBA comunique batería por su receptor. |

Los receptores AJAZZ/MCHOSE reconocidos sin protocolo implementado se muestran
sin enviarles consultas específicas. No se cambian DPI, botones, iluminación,
perfiles ni firmware. Las consultas ocurren fuera del hilo de la interfaz y las
operaciones asíncronas de Windows tienen un límite de espera.

## Validación

```powershell
cargo test -- --test-threads=1
cargo run -- --battery-diagnostics
```

El segundo comando funciona aunque haya otra instancia abierta. Imprime las
lecturas, los errores de cada fuente y las interfaces HID; en MCHOSE también
imprime la respuesta de estado. No escribe un archivo de diagnóstico ni envía
datos a internet. Para comprobar la UI nueva, cierra la instancia anterior de la
app y ejecuta `cargo run`.

Las pruebas cubren el límite de 20 %, repetición de avisos, recarga, ausencia de
datos, valores fuera de rango, checksum AJAZZ, lecturas anteriores y la respuesta
real del K7 Ultra. El aviso de batería baja se valida con datos simulados; la
unidad física disponible estaba al 78 %.

## Referencias de protocolo

- [Microsoft: BluetoothDevice](https://learn.microsoft.com/en-us/uwp/api/windows.devices.bluetooth.bluetoothdevice).
- [Microsoft: BluetoothLEDevice](https://learn.microsoft.com/en-us/uwp/api/windows.devices.bluetooth.bluetoothledevice).
- [Bluetooth SIG: tipos de apariencia y clase de dispositivo](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/Assigned_Numbers/out/en/index-en.html).
- [Microsoft: usos HID](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/hid-usages).
- [Microsoft: RawGameController.TryGetBatteryReport](https://learn.microsoft.com/en-us/uwp/api/windows.gaming.input.rawgamecontroller.trygetbatteryreport).
- [MCHOSE: descripción independiente del protocolo](https://github.com/alexfrih/mchose-linux/blob/main/PROTOCOL.md).
- [AJAZZ CompX: formato de los informes y batería](https://github.com/Geovane2dd/Ajazzy/blob/main/protocol/hid.h).
- [AJAZZ APEX: consulta de batería F7](https://github.com/Aiacos/ajazz-control-center/blob/develop/docs/protocols/mouse/aj_series_battery.md).

La implementación local se basa en los formatos de comunicación documentados;
no necesita instalar ni ejecutar esas aplicaciones.
