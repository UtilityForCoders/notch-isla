# Genera assets\nocho.ico a partir de assets\nocho.png (el logo con fondo transparente).
#
# El logo se recorta a lo que tiene dibujo, se centra y se escala para ocupar casi todo el
# cuadro: así se ve del mismo tamaño que los demás íconos en la barra de tareas y la bandeja.
# Cada tamaño se reduce por separado desde el original con filtro bicúbico de alta calidad.
#
# Uso (desde la raíz del repositorio):
#   powershell -ExecutionPolicy Bypass -File scripts\generar-icono.ps1

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$source = Join-Path $root 'assets\nocho.png'
$target = Join-Path $root 'assets\nocho.ico'

# Tamaños que Windows elige según el lugar (bandeja, barra de tareas, Explorador, Alt+Tab...)
# y la escala de la pantalla (100 % a 250 %).
$sizes = 16, 20, 24, 32, 40, 48, 64, 96, 128, 256
# Parte del cuadro que ocupa el logo por su lado más largo.
$fill = 0.92

$master = [System.Drawing.Bitmap]::FromFile($source)
try {
    # Caja del dibujo: píxeles con algo de opacidad.
    $minX = $master.Width; $minY = $master.Height; $maxX = -1; $maxY = -1
    for ($y = 0; $y -lt $master.Height; $y++) {
        for ($x = 0; $x -lt $master.Width; $x++) {
            if ($master.GetPixel($x, $y).A -gt 8) {
                if ($x -lt $minX) { $minX = $x }
                if ($x -gt $maxX) { $maxX = $x }
                if ($y -lt $minY) { $minY = $y }
                if ($y -gt $maxY) { $maxY = $y }
            }
        }
    }
    if ($maxX -lt 0) { throw "assets\nocho.png no tiene contenido visible" }
    $crop = New-Object System.Drawing.Rectangle $minX, $minY, ($maxX - $minX + 1), ($maxY - $minY + 1)

    $images = foreach ($size in $sizes) {
        $bitmap = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $graphics.Clear([System.Drawing.Color]::Transparent)
        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
        $scale = $size * $fill / [Math]::Max($crop.Width, $crop.Height)
        $w = $crop.Width * $scale
        $h = $crop.Height * $scale
        $left = ($size - $w) / 2
        $top = ($size - $h) / 2
        [System.Drawing.PointF[]]$dest = @(
            (New-Object System.Drawing.PointF $left, $top),
            (New-Object System.Drawing.PointF ($left + $w), $top),
            (New-Object System.Drawing.PointF $left, ($top + $h))
        )
        $from = New-Object System.Drawing.RectangleF $crop.X, $crop.Y, $crop.Width, $crop.Height
        $attributes = New-Object System.Drawing.Imaging.ImageAttributes
        # Sin esto, el borde del recorte se mezcla con negro transparente y deja un halo oscuro.
        $attributes.SetWrapMode([System.Drawing.Drawing2D.WrapMode]::TileFlipXY)
        $graphics.DrawImage($master, $dest, $from, [System.Drawing.GraphicsUnit]::Pixel, $attributes)
        $graphics.Dispose()
        $stream = New-Object System.IO.MemoryStream
        $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        $bitmap.Dispose()
        [pscustomobject]@{ Size = $size; Data = $stream.ToArray() }
    }
}
finally {
    $master.Dispose()
}

# ICO con cada tamaño guardado como PNG (Windows Vista en adelante).
$out = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter $out
$writer.Write([UInt16]0)                 # reservado
$writer.Write([UInt16]1)                 # tipo: ícono
$writer.Write([UInt16]$images.Count)
$offset = 6 + 16 * $images.Count
foreach ($image in $images) {
    $side = if ($image.Size -ge 256) { 0 } else { $image.Size }
    $writer.Write([byte]$side)            # ancho (0 = 256)
    $writer.Write([byte]$side)            # alto
    $writer.Write([byte]0)                # sin paleta
    $writer.Write([byte]0)                # reservado
    $writer.Write([UInt16]1)              # planos
    $writer.Write([UInt16]32)             # bits por píxel
    $writer.Write([UInt32]$image.Data.Length)
    $writer.Write([UInt32]$offset)
    $offset += $image.Data.Length
}
foreach ($image in $images) { $writer.Write($image.Data) }
$writer.Flush()
[System.IO.File]::WriteAllBytes($target, $out.ToArray())
Write-Host ("assets\nocho.ico: {0} tamaños, {1} bytes" -f $images.Count, $out.Length)
