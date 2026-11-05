#!/usr/bin/env bash
# Arma las notas de un release a partir de los commits (Conventional Commits) desde la versión
# anterior. Solo entran los cambios que notan los usuarios: `feat`, `fix` y `perf`.
#
# Uso: .github/scripts/notas-release.sh <versión> [etiqueta-anterior] > notas.md
# Sin etiqueta anterior, se usa la última `v*` alcanzable desde HEAD. Con FIRMADO=true no se
# agrega el aviso de archivos sin firmar.

set -euo pipefail

version="${1:?falta la versión, por ejemplo 0.5.3}"
previous="${2:-$(git describe --tags --abbrev=0 --match 'v*' HEAD 2>/dev/null || true)}"
range="${previous:+$previous..}HEAD"

features=()
fixes=()
perf=()
breaking=()

# tipo, ámbito opcional, "!" opcional y la descripción.
pattern='^([a-z]+)(\([^)]*\))?(!)?: (.+)$'

while IFS= read -r subject; do
    [[ $subject =~ $pattern ]] || continue
    type="${BASH_REMATCH[1]}"
    bang="${BASH_REMATCH[3]}"
    text="${BASH_REMATCH[4]}"
    # Sin la referencia al PR que agrega el squash ("... (#12)") y con mayúscula inicial.
    text="$(sed -E 's/ \(#[0-9]+\)$//' <<<"$text")"
    text="${text^}"
    case "$type" in
        feat) features+=("$text") ;;
        fix) fixes+=("$text") ;;
        perf) perf+=("$text") ;;
        *) continue ;;
    esac
    if [[ -n $bang ]]; then
        breaking+=("$text")
    fi
done < <(git log --no-merges --reverse --format='%s' "$range")

section() {
    local title="$1"
    shift
    (($# > 0)) || return 0
    printf '## %s\n\n' "$title"
    printf -- '- %s\n' "$@"
    printf '\n'
}

section "Cambios importantes" "${breaking[@]}"
section "Novedades" "${features[@]}"
section "Correcciones" "${fixes[@]}"
section "Rendimiento" "${perf[@]}"
if ((${#features[@]} + ${#fixes[@]} + ${#perf[@]} == 0)); then
    printf '## Cambios\n\n- Mejoras internas.\n\n'
fi

cat <<EOF
## Si vienes de una versión anterior

- Desde la **0.3.1 o posteriores**: te llega sola; haz clic en el aviso (o en el punto azul de la isla) para instalarla.
- Desde la **0.3.0 o anteriores**: instala esta versión a mano una vez. Después, las siguientes llegan solas.

## Descargas

- \`notch-isla-setup-${version}.exe\`: instalador (recomendado, necesario para las actualizaciones automáticas). No pide permisos de administrador y actualiza encima de la versión anterior.
- \`notch-isla.exe\`: versión portable, sin instalar.
EOF

# FIRMADO=true lo pone el workflow cuando SignPath firmó los archivos.
if [[ ${FIRMADO:-false} != true ]]; then
    printf '\nLos archivos todavía no están firmados: Windows SmartScreen puede mostrar un aviso ("Más información" > "Ejecutar de todas formas").\n'
fi
