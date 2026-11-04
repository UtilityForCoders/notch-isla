# Mide cuanto gasta la Notch Isla que ya esta abierta: CPU (en % de un nucleo) y memoria.
# No abre ni cambia nada; solo observa el proceso durante unos segundos.
#
# Uso: deja la isla en el estado que quieras medir (en reposo, musica sonando con la isla
# contraida, expandida con letra...) y ejecuta:
#   powershell -ExecutionPolicy Bypass -File scripts\medir-rendimiento.ps1 -Segundos 20
# Agrega -IncluirGpu para observar los motores GPU de la isla y DWM (todo el escritorio).
# -IdProceso permite elegir la compilacion de esta rama si hay varias copias abiertas.
#
# Sale con error si pasa los limites (-MaxCpu en %, -MaxMemoria en MB de memoria privada).

param(
    [ValidateRange(1, 3600)]
    [int]$Segundos = 15,
    [double]$MaxCpu = 3.0,
    [double]$MaxMemoria = 40.0,
    [int]$IdProceso = 0,
    [switch]$IncluirGpu
)

$proc = if ($IdProceso -gt 0) {
    Get-Process -Id $IdProceso -ErrorAction SilentlyContinue
} else {
    Get-Process notch-isla -ErrorAction SilentlyContinue | Select-Object -First 1
}
if (-not $proc) {
    Write-Host "La isla no esta abierta."
    exit 2
}

$cpu0 = $proc.TotalProcessorTime
$t0 = Get-Date
$picoMemoria = 0.0
$gpuIsla = @()
$gpuDwm = @()
$gpuDisponible = $true
$dwmIds = @(Get-Process dwm -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq $proc.SessionId } | ForEach-Object { $_.Id })
for ($i = 0; $i -lt $Segundos; $i++) {
    Start-Sleep -Seconds 1
    $proc.Refresh()
    $picoMemoria = [math]::Max($picoMemoria, $proc.PrivateMemorySize64 / 1MB)
    if ($IncluirGpu -and $gpuDisponible) {
        try {
            $motores = @(Get-CimInstance Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine -ErrorAction Stop)
            if ($motores.Count -eq 0) { throw 'Sin contadores GPU' }
            $isla = 0.0
            $dwm = 0.0
            foreach ($motor in $motores) {
                if ($motor.Name -match '^pid_(\d+)_') {
                    $motorPid = [int]$Matches[1]
                    if ($motorPid -eq $proc.Id) { $isla += [double]$motor.UtilizationPercentage }
                    if ($dwmIds -contains $motorPid) { $dwm += [double]$motor.UtilizationPercentage }
                }
            }
            $gpuIsla += $isla
            $gpuDwm += $dwm
        } catch {
            $gpuDisponible = $false
        }
    }
}
$proc.Refresh()
$cpuMs = ($proc.TotalProcessorTime - $cpu0).TotalMilliseconds
$wallMs = ((Get-Date) - $t0).TotalMilliseconds
$cpu = 100.0 * $cpuMs / $wallMs

"CPU:               {0:N2} % de un nucleo ({1:N0} ms de CPU en {2:N0} s)" -f $cpu, $cpuMs, ($wallMs / 1000)
"Memoria privada:   {0:N1} MB (pico {1:N1} MB)" -f ($proc.PrivateMemorySize64 / 1MB), $picoMemoria
"Memoria en uso:    {0:N1} MB (incluye DLL de Windows compartidas)" -f ($proc.WorkingSet64 / 1MB)
"Hilos:             $($proc.Threads.Count)"
"Handles:           $($proc.HandleCount)"
"Proceso:           $($proc.Id) - $($proc.Path)"
if ($IncluirGpu) {
    if ($gpuDisponible -and $gpuIsla.Count -gt 0) {
        $islaStats = $gpuIsla | Measure-Object -Average -Maximum
        "GPU isla:          media {0:N2} %, pico {1:N2} %" -f $islaStats.Average, $islaStats.Maximum
        if ($dwmIds.Count -gt 0) {
            $dwmStats = $gpuDwm | Measure-Object -Average -Maximum
            "GPU DWM:           media {0:N2} %, pico {1:N2} %" -f $dwmStats.Average, $dwmStats.Maximum
        } else {
            "GPU DWM:           proceso no disponible en esta sesion"
        }
        "GPU: suma de motores y adaptadores; puede superar 100 %. DWM incluye otras apps."
        "Estas muestras no miden FPS ni tiempos de presentacion."
    } else {
        "GPU: contadores no disponibles; CPU y memoria siguen siendo validas."
    }
}

$fallas = @()
if ($cpu -gt $MaxCpu) { $fallas += "CPU por encima de $MaxCpu %" }
if ($picoMemoria -gt $MaxMemoria) { $fallas += "memoria por encima de $MaxMemoria MB" }
if ($fallas.Count -gt 0) {
    Write-Host ("FALLA: " + ($fallas -join ", "))
    exit 1
}
Write-Host "OK"
