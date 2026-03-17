param([string]$Paths)

$appRoot = Split-Path (Split-Path $PSScriptRoot)

$fileList = @($Paths -split '\|' | Where-Object { $_ -and (Test-Path $_) })
if (-not $fileList -or $fileList.Count -eq 0) { exit }

$sourceDir = [System.IO.Path]::GetDirectoryName($fileList[0])
$destination = Join-Path $sourceDir "Z_OLD"

if (-not (Test-Path $destination)) {
    New-Item -Path $destination -ItemType Directory -Force | Out-Null
}

foreach ($item in $fileList) {
    if (Test-Path $item) {
        Move-Item -Path $item -Destination $destination -Force
    }
}

# Show confirmation dialog
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
[DwmHelper]::SetCurrentProcessExplicitAppUserModelID("UFB.MoveToZold") | Out-Null

$isDark = (Get-ItemProperty -Path "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Themes\Personalize" -ErrorAction SilentlyContinue).AppsUseLightTheme -eq 0

$accentDword = (Get-ItemProperty -Path "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Accent" -Name "AccentColorMenu" -ErrorAction SilentlyContinue).AccentColorMenu
if ($accentDword) {
    $aR = $accentDword -band 0xFF
    $aG = ($accentDword -shr 8) -band 0xFF
    $aB = ($accentDword -shr 16) -band 0xFF
    $accent = "#{0:X2}{1:X2}{2:X2}" -f $aR, $aG, $aB
    $accentHover = "#{0:X2}{1:X2}{2:X2}" -f ([Math]::Min(255,$aR+30)), ([Math]::Min(255,$aG+30)), ([Math]::Min(255,$aB+30))
    $accentPressed = "#{0:X2}{1:X2}{2:X2}" -f ([Math]::Max(0,$aR-20)), ([Math]::Max(0,$aG-20)), ([Math]::Max(0,$aB-20))
} else {
    $accent = "#0078D4"; $accentHover = "#1A8CE6"; $accentPressed = "#006CBE"
}

if ($isDark) {
    $bg = "#202020"; $fg = "#FFFFFF"
} else {
    $bg = "#F3F3F3"; $fg = "#000000"
}

$count = $fileList.Count
$noun = if ($count -eq 1) { "item" } else { "items" }
$message = "Moved $count $noun to Z_OLD."

[xml]$xaml = @"
<Window xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
        xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
        Title="Move to Z_OLD" Width="420" Height="150"
        WindowStartupLocation="CenterScreen" ResizeMode="NoResize"
        Background="$bg"
        >
    <Window.Resources>
        <Style x:Key="PrimaryButton" TargetType="Button">
            <Setter Property="FocusVisualStyle" Value="{x:Null}"/>
            <Setter Property="FontFamily" Value="Segoe UI"/>
            <Setter Property="FontSize" Value="13"/>
            <Setter Property="Foreground" Value="#FFFFFF"/>
            <Setter Property="Template">
                <Setter.Value>
                    <ControlTemplate TargetType="Button">
                        <Border x:Name="Bd" Background="$accent"
                                CornerRadius="4" Padding="0">
                            <ContentPresenter HorizontalAlignment="Center"
                                              VerticalAlignment="Center"/>
                        </Border>
                        <ControlTemplate.Triggers>
                            <Trigger Property="IsMouseOver" Value="True">
                                <Setter TargetName="Bd" Property="Background" Value="$accentHover"/>
                            </Trigger>
                            <Trigger Property="IsPressed" Value="True">
                                <Setter TargetName="Bd" Property="Background" Value="$accentPressed"/>
                            </Trigger>
                        </ControlTemplate.Triggers>
                    </ControlTemplate>
                </Setter.Value>
            </Setter>
        </Style>
    </Window.Resources>
    <Grid Margin="20">
        <Grid.RowDefinitions>
            <RowDefinition Height="*"/>
            <RowDefinition Height="Auto"/>
        </Grid.RowDefinitions>
        <TextBlock Grid.Row="0" Text="$message" Foreground="$fg"
                   FontFamily="Segoe UI" FontSize="14" TextWrapping="Wrap"
                   VerticalAlignment="Center"/>
        <StackPanel Grid.Row="1" Orientation="Horizontal" HorizontalAlignment="Right"
                    Margin="0,15,0,0">
            <Button x:Name="OkButton" Content="OK" Width="80" Height="26"
                    Style="{StaticResource PrimaryButton}" IsDefault="True"/>
        </StackPanel>
    </Grid>
</Window>
"@

$reader = New-Object System.Xml.XmlNodeReader $xaml
$window = [Windows.Markup.XamlReader]::Load($reader)

$iconFile = Join-Path $appRoot 'icons\icon.ico'
if (Test-Path $iconFile) {
    $window.Icon = [Windows.Media.Imaging.BitmapFrame]::Create([Uri]::new($iconFile))
}

if ($isDark) {
    $window.Add_SourceInitialized({
        $hwnd = (New-Object System.Windows.Interop.WindowInteropHelper $window).Handle
        $value = 1
        [DwmHelper]::DwmSetWindowAttribute($hwnd, 20, [ref]$value, 4)
    })
}

$window.FindName("OkButton").Add_Click({ $window.Close() })
$window.ShowDialog() | Out-Null
