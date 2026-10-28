# Mide cuanto gasta la Notch Isla que ya esta abierta: CPU (en % de un nucleo) y memoria.
# No abre ni cambia nada; solo observa el proceso durante unos segundos.
#
# Uso: deja la isla en el estado que quieras medir (en reposo, musica sonando con la isla
# contraida, expandida con letra...) y ejecuta:
#   powershell -ExecutionPolicy Bypass -File scripts\medir-rendimiento.ps1 -Segundos 20
#
# Sale con error si pasa los limites (-MaxCpu en %, -MaxMemoria en MB de memoria privada).

param(
    [int]$Segundos = 15,
    [double]$MaxCpu = 3.0,
    [double]$MaxMemoria = 40.0
)

$proc = Get-Process notch-isla -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) {
    Write-Host "La isla no esta abierta."
    exit 2
}

$cpu0 = $proc.TotalProcessorTime
$t0 = Get-Date
$picoMemoria = 0.0
for ($i = 0; $i -lt $Segundos; $i++) {
    Start-Sleep -Seconds 1
    $proc.Refresh()
    $picoMemoria = [math]::Max($picoMemoria, $proc.PrivateMemorySize64 / 1MB)
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

$fallas = @()
if ($cpu -gt $MaxCpu) { $fallas += "CPU por encima de $MaxCpu %" }
if ($picoMemoria -gt $MaxMemoria) { $fallas += "memoria por encima de $MaxMemoria MB" }
if ($fallas.Count -gt 0) {
    Write-Host ("FALLA: " + ($fallas -join ", "))
    exit 1
}
Write-Host "OK"
