$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

Add-Type -AssemblyName System.Drawing

$output = Join-Path (Get-Location) "assets\sight-relay.ico"
$directory = Split-Path $output -Parent
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$sizes = @(16, 24, 32, 48, 64, 128, 256)
$images = @()

try {
    foreach ($size in $sizes) {
        $bitmap = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.Clear([System.Drawing.Color]::Transparent)

        $blue = [System.Drawing.Brushes]::RoyalBlue
        $radius = [Math]::Max(2, [int]($size * 0.19))
        $path = New-Object System.Drawing.Drawing2D.GraphicsPath
        $path.AddArc(0, 0, $radius * 2, $radius * 2, 180, 90)
        $path.AddArc($size - $radius * 2, 0, $radius * 2, $radius * 2, 270, 90)
        $path.AddArc($size - $radius * 2, $size - $radius * 2, $radius * 2, $radius * 2, 0, 90)
        $path.AddArc(0, $size - $radius * 2, $radius * 2, $radius * 2, 90, 90)
        $path.CloseFigure()
        $graphics.FillPath($blue, $path)

        $white = New-Object System.Drawing.Pen([System.Drawing.Color]::White, [Math]::Max(1, $size * 0.075))
        $white.StartCap = [System.Drawing.Drawing2D.LineCap]::Square
        $white.EndCap = [System.Drawing.Drawing2D.LineCap]::Square
        $inset = $size * 0.27
        $arm = $size * 0.24
        $graphics.DrawLine($white, $inset, $inset, $inset + $arm, $inset)
        $graphics.DrawLine($white, $inset, $inset, $inset, $inset + $arm)
        $graphics.DrawLine($white, $size - $inset, $inset, $size - $inset - $arm, $inset)
        $graphics.DrawLine($white, $size - $inset, $inset, $size - $inset, $inset + $arm)
        $graphics.DrawLine($white, $inset, $size - $inset, $inset + $arm, $size - $inset)
        $graphics.DrawLine($white, $inset, $size - $inset, $inset, $size - $inset - $arm)
        $graphics.DrawLine($white, $size - $inset, $size - $inset, $size - $inset - $arm, $size - $inset)
        $graphics.DrawLine($white, $size - $inset, $size - $inset, $size - $inset, $size - $inset - $arm)

        $stream = New-Object System.IO.MemoryStream
        $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        $images += ,@($size, $stream.ToArray())
        $white.Dispose(); $path.Dispose(); $graphics.Dispose(); $bitmap.Dispose(); $stream.Dispose()
    }

    $stream = New-Object System.IO.FileStream($output, [System.IO.FileMode]::Create)
    $writer = New-Object System.IO.BinaryWriter($stream)
    $writer.Write([UInt16]0); $writer.Write([UInt16]1); $writer.Write([UInt16]$sizes.Count)
    $offset = 6 + (16 * $sizes.Count)
    foreach ($entry in $images) {
        $size = $entry[0]; $bytes = $entry[1]
        $writer.Write([Byte]($(if ($size -ge 256) { 0 } else { $size })))
        $writer.Write([Byte]($(if ($size -ge 256) { 0 } else { $size })))
        $writer.Write([Byte]0); $writer.Write([Byte]0); $writer.Write([UInt16]1); $writer.Write([UInt16]32)
        $writer.Write([UInt32]$bytes.Length); $writer.Write([UInt32]$offset)
        $offset += $bytes.Length
    }
    foreach ($entry in $images) { $writer.Write([Byte[]]$entry[1]) }
    $writer.Dispose(); $stream.Dispose()
    Write-Host "Created $output"
}
finally {
    foreach ($entry in $images) { if ($entry[1] -is [IDisposable]) { $entry[1].Dispose() } }
}
