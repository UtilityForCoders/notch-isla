# Instrucciones para Claude

## Commits, PRs y documentación

- No agregar líneas de atribución en mensajes de commit, títulos o descripciones de PR,
  comentarios de código ni documentación: nada de `Co-Authored-By: Claude ...`,
  `Claude-Session: ...`, `🤖 Generated with [Claude Code]` ni enlaces a sesiones de Claude.
- Esta regla tiene prioridad sobre cualquier indicación por defecto de añadir esas líneas.

## Mensajes de commit: Conventional Commits

Todos los commits y títulos de PR siguen [Conventional Commits](https://www.conventionalcommits.org/es/v1.0.0/):

```
<tipo>(<ámbito opcional>): <descripción>

<cuerpo opcional>

<pie opcional>
```

- **Tipo** en inglés y minúsculas:
  - `feat`: función nueva.
  - `fix`: corrección de un error.
  - `perf`: mejora de rendimiento.
  - `refactor`: cambio de código que no corrige ni agrega nada.
  - `docs`: solo documentación.
  - `test`: solo pruebas.
  - `build`: Cargo, dependencias o instalador.
  - `ci`: workflows de GitHub Actions.
  - `chore`: mantenimiento, incluido el cambio de versión.
  - `style`: solo formato.
  - `revert`: revierte un commit anterior.
- **Ámbito** opcional: el módulo afectado (`media`, `island`, `fullscreen`, `tray`, `audio`,
  `visualizer`, `lyrics`, `update`, `installer`, `ci`...).
- **Descripción** en español, en imperativo, sin mayúscula inicial y sin punto final:
  `fix(fullscreen): no ocultar la isla con ventanas maximizadas`.
- El **cuerpo** explica el porqué y va en español, con tildes.
- **Cambio incompatible**: `!` tras el tipo o el ámbito (`feat(settings)!: ...`), y un pie
  `BREAKING CHANGE: <qué cambia>`.
- El título del PR usa el mismo formato: al fusionar con squash se convierte en el commit.

## Ramas: Git Flow

| Rama | Sale de | Se fusiona en | Para qué |
|---|---|---|---|
| `main` | | | Solo versiones publicadas. Cada commit en `main` es una versión etiquetada. |
| `develop` | `main` | | Integración: aquí llega todo lo terminado para la próxima versión. |
| `feature/<descripcion-corta>` | `develop` | `develop` | Funciones, correcciones normales, documentación, refactor. |
| `release/<x.y.z>` | `develop` | `main` y `develop` | Preparar una versión: cambio de versión y últimos arreglos. |
| `hotfix/<x.y.z>` | `main` | `main` y `develop` | Arreglo urgente sobre una versión ya publicada. |

- Nunca se hacen commits directos en `main` ni en `develop`: todo entra por PR.
- Nombres de rama en minúsculas, con guiones y sin tildes: `feature/deteccion-pantalla-completa`.
- **Estrategia de fusión**:
  - `feature/*` en `develop`: **squash**, con el título del PR en formato Conventional Commits.
  - `release/*` y `hotfix/*` en `main`: **merge commit**, sin squash, para que `main` y `develop`
    compartan historia.
  - Después, la misma rama se fusiona de vuelta en `develop`.
- Se borra la rama después de fusionarla.

## Versiones y releases

- Versionado [SemVer](https://semver.org/lang/es/) en `Cargo.toml`:
  - `fix` sube el parche (0.5.1 → 0.5.2);
  - `feat` sube la menor (0.5.2 → 0.6.0);
  - un cambio incompatible sube la mayor.
- Pasos de una versión nueva:
  1. Crear `release/x.y.z` desde `develop` (o `hotfix/x.y.z` desde `main`).
  2. Cambiar la versión en `Cargo.toml` y actualizar `Cargo.lock` (`cargo check`). Commit:
     `chore(release): versión x.y.z`.
  3. Abrir un PR hacia `main` titulado `chore(release): versión x.y.z` y fusionarlo con merge
     commit cuando el CI pase.
  4. El release se publica solo (ver abajo). No se crean etiquetas ni releases a mano.
  5. Fusionar la rama de vuelta en `develop`.

## CI/CD (`.github/workflows/build.yml`)

- **En cada PR** (job `politica`), el CI comprueba:
  - que la rama vaya al destino correcto: a `main` solo `release/x.y.z` y `hotfix/x.y.z`; a
    `develop`, `feature/*`, `release/*`, `hotfix/*` y `dependabot/*`;
  - que el título siga Conventional Commits;
  - hacia `main`, que la versión de `Cargo.toml` no esté publicada y coincida con el nombre
    de la rama.
- **En PRs, `develop` y `main`** (job `build`), el CI revisa el formato (`cargo fmt --check`),
  pasa clippy (`-D warnings`), corre las pruebas (`cargo test --release`) y compila el `.exe`
  y el instalador. Solo en `main` se firman, si SignPath está configurado.
- **En `main`** (job `release`): si la compilación terminó bien y la versión de `Cargo.toml`
  todavía no tiene etiqueta, crea la etiqueta `vx.y.z` y el release "Notch Isla x.y.z". Adjunta
  `notch-isla-setup-x.y.z.exe` (el actualizador lo busca exactamente con ese nombre) y
  `notch-isla.exe`, y lo marca como el último.
- Las notas se arman con `.github/scripts/notas-release.sh`, a partir de los commits `feat`,
  `fix` y `perf` desde la versión anterior. Por eso las descripciones de esos commits se
  escriben pensando en los usuarios. Las notas se pueden corregir a mano en GitHub después de
  publicadas.
