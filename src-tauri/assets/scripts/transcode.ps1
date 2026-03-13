param([string]$Paths)

$fileList = $Paths -split '\|' | Where-Object { $_ -and (Test-Path $_) }
if (-not $fileList -or $fileList.Count -eq 0) { exit }

Add-Type -AssemblyName PresentationFramework, PresentationCore, WindowsBase

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class DwmHelper {
    [DllImport("dwmapi.dll", PreserveSig = true)]
    public static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int attrValue, int attrSize);
    [DllImport("shell32.dll", SetLastError = true)]
    public static extern int SetCurrentProcessExplicitAppUserModelID([MarshalAs(UnmanagedType.LPWStr)] string AppID);
}
"@
[DwmHelper]::SetCurrentProcessExplicitAppUserModelID("UFB.Transcode") | Out-Null

# Detect system dark mode
$isDark = (Get-ItemProperty -Path "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Themes\Personalize" -ErrorAction SilentlyContinue).AppsUseLightTheme -eq 0

# Get system accent color (stored as ABGR)
$accentDword = (Get-ItemProperty -Path "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Accent" -Name "AccentColorMenu" -ErrorAction SilentlyContinue).AccentColorMenu
if ($accentDword) {
    $aR = $accentDword -band 0xFF
    $aG = ($accentDword -shr 8) -band 0xFF
    $aB = ($accentDword -shr 16) -band 0xFF
    $accent = "#{0:X2}{1:X2}{2:X2}" -f $aR, $aG, $aB
} else {
    $accent = "#0078D4"
}

if ($isDark) {
    $bg = "#202020"; $fg = "#FFFFFF"; $dimFg = "#999999"
    $cancelBg = "#333333"; $cancelHover = "#444444"; $cancelPressed = "#2A2A2A"
    $cancelFg = "#FFFFFF"; $barBg = "#383838"
} else {
    $bg = "#F3F3F3"; $fg = "#000000"; $dimFg = "#666666"
    $cancelBg = "#E5E5E5"; $cancelHover = "#D5D5D5"; $cancelPressed = "#C5C5C5"
    $cancelFg = "#000000"; $barBg = "#CCCCCC"
}

$totalFiles = $fileList.Count

[xml]$xaml = @"
<Window xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
        xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
        Title="Transcode" Width="460" Height="180"
        WindowStartupLocation="CenterScreen" ResizeMode="NoResize"
        Background="$bg"
        Icon="C:\Program Files\ufb\assets\icons\ufpn.ico">
    <Window.Resources>
        <Style x:Key="SecondaryButton" TargetType="Button">
            <Setter Property="FocusVisualStyle" Value="{x:Null}"/>
            <Setter Property="FontFamily" Value="Segoe UI"/>
            <Setter Property="FontSize" Value="13"/>
            <Setter Property="Foreground" Value="$cancelFg"/>
            <Setter Property="Template">
                <Setter.Value>
                    <ControlTemplate TargetType="Button">
                        <Border x:Name="Bd" Background="$cancelBg"
                                CornerRadius="4" Padding="0">
                            <ContentPresenter HorizontalAlignment="Center"
                                              VerticalAlignment="Center"/>
                        </Border>
                        <ControlTemplate.Triggers>
                            <Trigger Property="IsMouseOver" Value="True">
                                <Setter TargetName="Bd" Property="Background" Value="$cancelHover"/>
                            </Trigger>
                            <Trigger Property="IsPressed" Value="True">
                                <Setter TargetName="Bd" Property="Background" Value="$cancelPressed"/>
                            </Trigger>
                        </ControlTemplate.Triggers>
                    </ControlTemplate>
                </Setter.Value>
            </Setter>
        </Style>
    </Window.Resources>
    <Grid Margin="20">
        <Grid.RowDefinitions>
            <RowDefinition Height="Auto"/>
            <RowDefinition Height="Auto"/>
            <RowDefinition Height="*"/>
            <RowDefinition Height="Auto"/>
        </Grid.RowDefinitions>
        <TextBlock Grid.Row="0" x:Name="StatusText" Text="Transcoding 1 / $totalFiles"
                   Foreground="$fg" FontFamily="Segoe UI" FontSize="14" Margin="0,0,0,4"/>
        <TextBlock Grid.Row="1" x:Name="FileNameText" Text=""
                   Foreground="$dimFg" FontFamily="Segoe UI" FontSize="12" Margin="0,0,0,10"
                   TextTrimming="CharacterEllipsis"/>
        <Border Grid.Row="2" Background="$barBg" CornerRadius="3" Height="6"
                VerticalAlignment="Center">
            <Border x:Name="ProgressFill" Background="$accent" CornerRadius="3"
                    Height="6" HorizontalAlignment="Left" Width="0"/>
        </Border>
        <StackPanel Grid.Row="3" Orientation="Horizontal" HorizontalAlignment="Right"
                    Margin="0,15,0,0">
            <Button x:Name="CancelButton" Content="Cancel" Width="80" Height="26"
                    Style="{StaticResource SecondaryButton}" IsCancel="True"/>
        </StackPanel>
    </Grid>
</Window>
"@

$reader = New-Object System.Xml.XmlNodeReader $xaml
$window = [Windows.Markup.XamlReader]::Load($reader)

if ($isDark) {
    $window.Add_SourceInitialized({
        $hwnd = (New-Object System.Windows.Interop.WindowInteropHelper $window).Handle
        $value = 1
        [DwmHelper]::DwmSetWindowAttribute($hwnd, 20, [ref]$value, 4)
    })
}

$statusText = $window.FindName("StatusText")
$fileNameText = $window.FindName("FileNameText")
$progressFill = $window.FindName("ProgressFill")
$cancelBtn = $window.FindName("CancelButton")

# Tool paths
$ffmpeg = "C:\Program Files\ufb\ffmpeg.exe"
$ffprobe = "C:\Program Files\ufb\ffprobe.exe"
$exiftool = "C:\Program Files\ufb\assets\exiftool\exiftool.exe"

# Shared state between UI and background runspace
$sync = [hashtable]::Synchronized(@{
    CurrentIndex = 0
    TotalFiles   = $totalFiles
    CurrentFile  = ""
    Progress     = 0
    Completed    = $false
    Cancelled    = $false
    FfmpegProcess = $null
})

# Background runspace for transcoding
$runspace = [runspacefactory]::CreateRunspace()
$runspace.ApartmentState = "STA"
$runspace.Open()
$runspace.SessionStateProxy.SetVariable("sync", $sync)
$runspace.SessionStateProxy.SetVariable("fileList", $fileList)
$runspace.SessionStateProxy.SetVariable("ffmpeg", $ffmpeg)
$runspace.SessionStateProxy.SetVariable("ffprobe", $ffprobe)
$runspace.SessionStateProxy.SetVariable("exiftool", $exiftool)

$ps = [powershell]::Create()
$ps.Runspace = $runspace
$ps.AddScript({
    foreach ($i in 0..($fileList.Count - 1)) {
        if ($sync.Cancelled) { break }

        $inputFile = $fileList[$i]
        $fileName = [System.IO.Path]::GetFileName($inputFile)
        $baseName = [System.IO.Path]::GetFileNameWithoutExtension($inputFile)
        $parentDir = [System.IO.Path]::GetDirectoryName($inputFile)
        $outputDir = Join-Path $parentDir "MP4"
        $outputFile = Join-Path $outputDir "$baseName.mp4"

        $sync.CurrentIndex = $i + 1
        $sync.CurrentFile = $fileName
        $sync.Progress = 0

        # Get total frame count via ffprobe
        $totalFrames = 0
        try {
            $probeInfo = New-Object System.Diagnostics.ProcessStartInfo
            $probeInfo.FileName = $ffprobe
            $probeInfo.Arguments = "-v error -count_packets -select_streams v:0 -show_entries stream=nb_read_packets -of csv=p=0 `"$inputFile`""
            $probeInfo.UseShellExecute = $false
            $probeInfo.RedirectStandardOutput = $true
            $probeInfo.RedirectStandardError = $true
            $probeInfo.CreateNoWindow = $true
            $probeProc = [System.Diagnostics.Process]::Start($probeInfo)
            $probeOutput = $probeProc.StandardOutput.ReadToEnd().Trim()
            $probeProc.WaitForExit()
            if ($probeOutput -match '^\d+') {
                $totalFrames = [int]$Matches[0]
            }
        } catch {}

        if ($sync.Cancelled) { break }

        # Create output directory
        if (-not (Test-Path $outputDir)) {
            New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
        }

        # Transcode with ffmpeg
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $ffmpeg
        $psi.Arguments = "-v quiet -stats_period 0.1 -progress pipe:1 -i `"$inputFile`" -c:v libx264 -pix_fmt yuv420p -crf 25 -preset fast -c:a aac -b:a 192k -y `"$outputFile`""
        $psi.UseShellExecute = $false
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $psi.CreateNoWindow = $true

        $proc = [System.Diagnostics.Process]::Start($psi)
        $sync.FfmpegProcess = $proc

        # Read progress from stdout line by line
        while (-not $proc.StandardOutput.EndOfStream) {
            if ($sync.Cancelled) {
                try { $proc.Kill() } catch {}
                break
            }
            $line = $proc.StandardOutput.ReadLine()
            if ($line -match '^frame=(\d+)') {
                $currentFrame = [int]$Matches[1]
                if ($totalFrames -gt 0) {
                    $sync.Progress = [Math]::Min(100, [Math]::Floor(($currentFrame / $totalFrames) * 100))
                }
            }
        }

        if (-not $sync.Cancelled) {
            $proc.WaitForExit()
        }
        $sync.FfmpegProcess = $null

        if ($sync.Cancelled) { break }

        # Copy AE metadata via exiftool (non-fatal)
        if (Test-Path $outputFile) {
            try {
                $exifInfo = New-Object System.Diagnostics.ProcessStartInfo
                $exifInfo.FileName = $exiftool
                $exifInfo.Arguments = "-TagsFromFile `"$inputFile`" `"-AeProjectLinkFullPath>AeProjectLinkFullPath`" -overwrite_original `"$outputFile`""
                $exifInfo.UseShellExecute = $false
                $exifInfo.RedirectStandardOutput = $true
                $exifInfo.RedirectStandardError = $true
                $exifInfo.CreateNoWindow = $true
                $exifProc = [System.Diagnostics.Process]::Start($exifInfo)
                $exifProc.WaitForExit()
            } catch {}
        }
    }

    $sync.Completed = $true
}) | Out-Null

$asyncResult = $ps.BeginInvoke()

# DispatcherTimer to poll shared state and update UI
$timer = New-Object System.Windows.Threading.DispatcherTimer
$timer.Interval = [TimeSpan]::FromMilliseconds(100)

# Capture the progress bar's parent width for calculating fill width
$progressBarMaxWidth = 420 - 40  # Window width minus Grid margins (20 each side)

$timer.Add_Tick({
    $statusText.Text = "Transcoding $($sync.CurrentIndex) / $($sync.TotalFiles)"
    $fileNameText.Text = $sync.CurrentFile
    $fillWidth = [Math]::Max(0, ($sync.Progress / 100) * $progressBarMaxWidth)
    $progressFill.Width = $fillWidth

    if ($sync.Completed) {
        $timer.Stop()
        $window.Close()
    }
})
$timer.Start()

# Cancel handling
$cancelAction = {
    $sync.Cancelled = $true
    $proc = $sync.FfmpegProcess
    if ($proc -and -not $proc.HasExited) {
        try { $proc.Kill() } catch {}
    }
    $timer.Stop()
    $window.Close()
}

$cancelBtn.Add_Click($cancelAction)
$window.Add_Closing({
    if (-not $sync.Completed) {
        $sync.Cancelled = $true
        $proc = $sync.FfmpegProcess
        if ($proc -and -not $proc.HasExited) {
            try { $proc.Kill() } catch {}
        }
    }
    $timer.Stop()
})

$window.ShowDialog() | Out-Null

# Cleanup
$timer.Stop()
$sync.Cancelled = $true
$proc = $sync.FfmpegProcess
if ($proc -and -not $proc.HasExited) {
    try { $proc.Kill() } catch {}
}
try {
    $ps.Stop()
    $ps.Dispose()
    $runspace.Close()
    $runspace.Dispose()
} catch {}
