# Extracts the Windows Remote Desktop icon from mstsc.exe at multiple sizes
# and packs them into assets/app.ico (PNG-compressed entries).
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$code = @'
using System;
using System.Runtime.InteropServices;
public class IconExtract {
    [DllImport("shell32.dll", CharSet=CharSet.Unicode)]
    public static extern int SHDefExtractIconW(string pszIconFile, int iIndex, uint uFlags,
        out IntPtr phiconLarge, out IntPtr phiconSmall, uint nIconSize);
    [DllImport("user32.dll")]
    public static extern bool DestroyIcon(IntPtr hIcon);
}
'@
Add-Type -TypeDefinition $code

$src = "$env:SystemRoot\System32\mstsc.exe"
$sizes = 256, 64, 48, 32, 16
$pngs = @()

foreach ($s in $sizes) {
    $large = [IntPtr]::Zero
    $small = [IntPtr]::Zero
    [IconExtract]::SHDefExtractIconW($src, 0, 0, [ref]$large, [ref]$small, [uint32]$s) | Out-Null
    if ($large -eq [IntPtr]::Zero) { throw "Failed to extract ${s}px icon" }
    $icon = [System.Drawing.Icon]::FromHandle($large)
    $bmp = $icon.ToBitmap()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs += , @($s, $ms.ToArray())
    $bmp.Dispose(); $icon.Dispose()
    [IconExtract]::DestroyIcon($large) | Out-Null
    if ($small -ne [IntPtr]::Zero) { [IconExtract]::DestroyIcon($small) | Out-Null }
}

$outDir = Join-Path $PSScriptRoot "..\assets"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$outPath = Join-Path $outDir "app.ico"

$fs = [System.IO.File]::Create($outPath)
$bw = New-Object System.IO.BinaryWriter($fs)
# ICONDIR
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$pngs.Count)
$offset = 6 + 16 * $pngs.Count
foreach ($p in $pngs) {
    $s = $p[0]; $data = $p[1]
    $dim = if ($s -ge 256) { 0 } else { $s }
    $bw.Write([byte]$dim); $bw.Write([byte]$dim)   # width, height (0 = 256)
    $bw.Write([byte]0); $bw.Write([byte]0)          # palette, reserved
    $bw.Write([uint16]1); $bw.Write([uint16]32)     # planes, bpp
    $bw.Write([uint32]$data.Length); $bw.Write([uint32]$offset)
    $offset += $data.Length
}
foreach ($p in $pngs) { $bw.Write($p[1]) }
$bw.Close()
Write-Host "Wrote $outPath ($((Get-Item $outPath).Length) bytes)"
