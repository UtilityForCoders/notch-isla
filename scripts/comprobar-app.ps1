# Comprobación de arranque y de consumo en reposo, para el CI y para usar a mano.
#
# Arranca el ejecutable, lo deja unos segundos sin tocar y mide lo que gasta sin hacer nada: el
# tiempo de CPU, los hilos y la memoria. Falla si el proceso se cae o si se pasa de un
# presupuesto. Así un temporizador demasiado rápido o un bucle de fondo nuevo se ven en el pull
# request y no semanas después en la batería de un portátil.
#
# Los presupuestos son holgados a propósito: los equipos del CI son más lentos y ruidosos que uno
# de escritorio. No miden si la app va "un poco peor", sino si algo se rompió de verdad.
#
#   ./scripts/comprobar-app.ps1 -Exe target/release/app.exe -MaxCpu 8 -MaxHilos 48 -MaxMemoriaMB 150 -MaxExeKB 3000

param(
    [Parameter(Mandatory)][string]$Exe,
    [string[]]$Argumentos = @(),
    # Segundos que se deja asentar la app antes de empezar a medir (carga inicial, íconos...).
    [int]$Arranque = 8,
    # Segundos de medición en reposo.
    [int]$Segundos = 30,
    # Máximo de CPU en reposo, en % de un núcleo (como la columna de detalles del Administrador
    # de tareas multiplicada por el número de núcleos).
    [double]$MaxCpu = 8,
    [int]$MaxHilos = 48,
    # Memoria privada (commit), en MB.
    [int]$MaxMemoriaMB = 150,
    # Tamaño del ejecutable, en KB. 0 = no se comprueba.
    [int]$MaxExeKB = 0
)

$ErrorActionPreference = 'Stop'
$ruta = (Resolve-Path $Exe).Path
$fallos = @()
$filas = @()

function Anotar([string]$nombre, [string]$valor, [string]$limite, [bool]$bien) {
    $script:filas += [pscustomobject]@{ Medida = $nombre; Valor = $valor; Limite = $limite; Estado = $(if ($bien) { 'bien' } else { 'SE PASA' }) }
    if (-not $bien) { $script:fallos += "${nombre}: $valor (límite $limite)" }
}

$exeKB = [math]::Round((Get-Item $ruta).Length / 1KB)
if ($MaxExeKB -gt 0) {
    Anotar 'Tamaño del .exe' "$exeKB KB" "$MaxExeKB KB" ($exeKB -le $MaxExeKB)
}

$parametros = @{ FilePath = $ruta; PassThru = $true }
if ($Argumentos.Count -gt 0) { $parametros.ArgumentList = $Argumentos }
$proceso = Start-Process @parametros
try {
    Start-Sleep -Seconds $Arranque
    $proceso.Refresh()
    if ($proceso.HasExited) {
        throw "La app se cerró sola durante el arranque (código $($proceso.ExitCode))."
    }
    $cpuInicial = $proceso.TotalProcessorTime.TotalSeconds
    $hilosMax = 0
    $memoriaMax = 0
    for ($i = 0; $i -lt $Segundos; $i++) {
        Start-Sleep -Seconds 1
        $proceso.Refresh()
        if ($proceso.HasExited) {
            throw "La app se cerró sola a los $($Arranque + $i) s (código $($proceso.ExitCode))."
        }
        $hilosMax = [math]::Max($hilosMax, $proceso.Threads.Count)
        $memoriaMax = [math]::Max($memoriaMax, $proceso.PrivateMemorySize64)
    }
    $cpu = ($proceso.TotalProcessorTime.TotalSeconds - $cpuInicial) / $Segundos * 100
    $memoriaMB = [math]::Round($memoriaMax / 1MB)

    Anotar 'Arranque' "viva tras $($Arranque + $Segundos) s" 'no cerrarse' $true
    Anotar 'CPU en reposo' ('{0:N2} % de un núcleo' -f $cpu) "$MaxCpu %" ($cpu -le $MaxCpu)
    Anotar 'Hilos' "$hilosMax" "$MaxHilos" ($hilosMax -le $MaxHilos)
    Anotar 'Memoria privada' "$memoriaMB MB" "$MaxMemoriaMB MB" ($memoriaMB -le $MaxMemoriaMB)
}
catch {
    $fallos += $_.Exception.Message
}
finally {
    if (-not $proceso.HasExited) {
        Stop-Process -Id $proceso.Id -Force -ErrorAction SilentlyContinue
    }
}

$filas | Format-Table -AutoSize | Out-String | Write-Host

# En GitHub Actions, la tabla sale también en el resumen del trabajo.
if ($env:GITHUB_STEP_SUMMARY) {
    $md = @("### Arranque y consumo en reposo: $(Split-Path $ruta -Leaf)", '', '| Medida | Valor | Límite | Estado |', '|---|---|---|---|')
    $md += $filas | ForEach-Object { "| $($_.Medida) | $($_.Valor) | $($_.Limite) | $($_.Estado) |" }
    $md += $fallos | Where-Object { $_ -notmatch 'límite' } | ForEach-Object { '', "**$_**" }
    $md -join "`n" | Add-Content -Path $env:GITHUB_STEP_SUMMARY -Encoding utf8
}

if ($fallos.Count -gt 0) {
    $fallos | ForEach-Object { Write-Host "::error::$_" }
    exit 1
}
Write-Host 'Todo dentro del presupuesto.'
