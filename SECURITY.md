# Seguridad / Security

## Español

### Reportar una vulnerabilidad

Si encuentras un problema de seguridad en Notch Isla, **no abras un issue público**. Repórtalo en privado desde la pestaña **Security > Report a vulnerability** de este repositorio (https://github.com/UtilityForCoders/notch-isla/security/advisories/new). Incluye qué versión usas, qué pasa y, si puedes, cómo reproducirlo.

Respondo lo antes posible. Cuando la corrección esté publicada, el reporte se hace público con el crédito para quien lo encontró (si lo desea).

### Versiones con soporte

Solo la **última versión publicada** recibe correcciones. La app se actualiza sola (ver [PRIVACY.md](PRIVACY.md)), así que la corrección llega a todos los usuarios con la versión siguiente.

### Cómo se protege el proyecto

- `main` solo cambia por pull request revisado por el dueño del repo (`CODEOWNERS`), con la compilación en verde.
- Las etiquetas de versión (`v*`), que son las que generan los releases, solo las puede crear el dueño del repo.
- Las GitHub Actions de los pull requests externos esperan aprobación manual, no reciben secretos y solo pueden usar acciones de GitHub o de autores verificados, fijadas a un commit exacto.
- El escaneo de secretos y Dependabot están activos.
- Las actualizaciones automáticas solo se descargan de `github.com` por HTTPS, se verifican contra la huella SHA-256 que publica GitHub y se vuelven a verificar justo antes de ejecutarse.

## English

### Reporting a vulnerability

If you find a security issue in Notch Isla, **please do not open a public issue**. Report it privately from this repository's **Security > Report a vulnerability** tab (https://github.com/UtilityForCoders/notch-isla/security/advisories/new), including the version you use, what happens and, if possible, how to reproduce it.

I will reply as soon as possible. Once the fix is released, the advisory is made public, crediting the reporter (if they wish).

### Supported versions

Only the **latest released version** gets fixes. The app updates itself, so fixes reach every user with the next release.
