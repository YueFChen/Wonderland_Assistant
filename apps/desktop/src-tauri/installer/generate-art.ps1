Add-Type -AssemblyName System.Drawing

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$iconPath = Join-Path $root 'icons/icon.png'
$output = $PSScriptRoot

function New-Canvas([int]$width, [int]$height) {
  $bitmap = [System.Drawing.Bitmap]::new($width, $height)
  $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
  $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
  $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $graphics.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit
  return @{ Bitmap = $bitmap; Graphics = $graphics }
}

function Paint-Gradient($graphics, [int]$width, [int]$height) {
  $rect = [System.Drawing.Rectangle]::new(0, 0, $width, $height)
  $dark = [System.Drawing.Color]::FromArgb(22, 18, 43)
  $purple = [System.Drawing.Color]::FromArgb(74, 52, 130)
  $brush = [System.Drawing.Drawing2D.LinearGradientBrush]::new($rect, $dark, $purple, 48)
  try { $graphics.FillRectangle($brush, $rect) } finally { $brush.Dispose() }
  $glow = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(36, 173, 137, 250))
  try { $graphics.FillEllipse($glow, -70, -40, $width + 105, $height * 0.72) } finally { $glow.Dispose() }
}

function Paint-Brand($graphics, [int]$width, [int]$height, [bool]$wide) {
  $icon = [System.Drawing.Image]::FromFile($iconPath)
  $white = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::White)
  $muted = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(220, 204, 235))
  $accent = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(176, 145, 250), 2)
  $titleFont = [System.Drawing.Font]::new('Segoe UI Semibold', $(if ($wide) { 15 } else { 12 }), [System.Drawing.FontStyle]::Bold)
  $smallFont = [System.Drawing.Font]::new('Segoe UI', $(if ($wide) { 9 } else { 8 }), [System.Drawing.FontStyle]::Regular)
  try {
    if ($wide) {
      $graphics.DrawImage($icon, 20, 54, 116, 116)
      $graphics.DrawLine($accent, 21, 188, 121, 188)
      $graphics.DrawString('WONDERLAND', $titleFont, $white, 20, 206)
      $graphics.DrawString('ASSISTANT', $titleFont, $white, 20, 232)
      $graphics.DrawString('A quieter place to begin.', $smallFont, $muted, 21, 268)
    } else {
      $graphics.DrawImage($icon, 26, 52, 112, 112)
      $graphics.DrawLine($accent, 26, 185, 138, 185)
      $graphics.DrawString('WONDERLAND', $titleFont, $white, 15, 203)
      $graphics.DrawString('ASSISTANT', $titleFont, $white, 23, 230)
      $graphics.DrawString('YOUR WORKSPACE', $smallFont, $muted, 29, 270)
    }
  } finally {
    $icon.Dispose(); $white.Dispose(); $muted.Dispose(); $accent.Dispose(); $titleFont.Dispose(); $smallFont.Dispose()
  }
}

function Save-Bitmap($canvas, [string]$name) {
  try { $canvas.Bitmap.Save((Join-Path $output $name), [System.Drawing.Imaging.ImageFormat]::Bmp) }
  finally { $canvas.Graphics.Dispose(); $canvas.Bitmap.Dispose() }
}

$sidebar = New-Canvas 164 314
Paint-Gradient $sidebar.Graphics 164 314
Paint-Brand $sidebar.Graphics 164 314 $false
Save-Bitmap $sidebar 'nsis-sidebar.bmp'

$header = New-Canvas 150 57
$header.Graphics.Clear([System.Drawing.Color]::White)
$headerIcon = [System.Drawing.Image]::FromFile($iconPath)
$headerBrush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(76, 54, 135))
$headerFont = [System.Drawing.Font]::new('Segoe UI Semibold', 9, [System.Drawing.FontStyle]::Bold)
try {
  $header.Graphics.DrawImage($headerIcon, 7, 8, 39, 39)
  $header.Graphics.DrawString('WONDERLAND', $headerFont, $headerBrush, 50, 12)
  $header.Graphics.DrawString('ASSISTANT', $headerFont, $headerBrush, 50, 28)
} finally { $headerIcon.Dispose(); $headerBrush.Dispose(); $headerFont.Dispose() }
Save-Bitmap $header 'nsis-header.bmp'

$dialog = New-Canvas 493 312
$dialog.Graphics.Clear([System.Drawing.Color]::White)
Paint-Gradient $dialog.Graphics 170 312
Paint-Brand $dialog.Graphics 170 312 $true
Save-Bitmap $dialog 'wix-dialog.bmp'

$banner = New-Canvas 493 58
$banner.Graphics.Clear([System.Drawing.Color]::White)
$banner.Graphics.DrawLine([System.Drawing.Pens]::Gainsboro, 0, 57, 493, 57)
$bannerIcon = [System.Drawing.Image]::FromFile($iconPath)
try { $banner.Graphics.DrawImage($bannerIcon, 438, 7, 43, 43) } finally { $bannerIcon.Dispose() }
Save-Bitmap $banner 'wix-banner.bmp'

Write-Output 'Generated NSIS and WiX installer artwork.'
